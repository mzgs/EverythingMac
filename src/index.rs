use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant, SystemTime},
};

use crate::{
    bulk, fs_events,
    index_db::{Checkpoint, Database},
};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub entries: Arc<Vec<Arc<Entry>>>,
    pub scanning: bool,
    pub skipped: usize,
    pub warning: Option<String>,
    pub revision: u64,
    #[cfg(test)]
    pub full_scans: usize,
    #[cfg(test)]
    pub replayed: bool,
}

pub struct Index {
    snapshot: Arc<RwLock<Snapshot>>,
    sender: Sender<Message>,
    stopped: Arc<AtomicBool>,
}

pub(crate) enum Message {
    Events(Vec<fs_events::Event>),
    Refresh,
    Stop,
}

type Entries = BTreeMap<PathBuf, Arc<Entry>>;
type Errors = BTreeMap<PathBuf, String>;

impl Index {
    pub fn start(mut roots: Vec<PathBuf>, database_path: Option<PathBuf>) -> Self {
        if let Ok(cwd) = std::env::current_dir() {
            for root in &mut roots {
                if root.is_relative() {
                    *root = cwd.join(&*root);
                }
            }
        }
        // FSEvents reports canonical parent paths (for example /private/var).
        for root in &mut roots {
            if let (Some(parent), Some(name)) = (root.parent(), root.file_name()) {
                if let Ok(parent) = parent.canonicalize() {
                    *root = parent.join(name);
                }
            }
        }
        roots.sort();
        roots.dedup();
        let mut distinct_roots: Vec<PathBuf> = Vec::new();
        for root in roots {
            if !distinct_roots.iter().any(|parent| root.starts_with(parent)) {
                distinct_roots.push(root);
            }
        }
        let snapshot = Arc::new(RwLock::new(Snapshot {
            entries: Arc::new(Vec::new()),
            scanning: true,
            skipped: 0,
            warning: None,
            revision: 0,
            #[cfg(test)]
            full_scans: 0,
            #[cfg(test)]
            replayed: false,
        }));
        let stopped = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let worker_snapshot = snapshot.clone();
        let worker_stopped = stopped.clone();
        let event_sender = sender.clone();
        std::thread::spawn(move || {
            run(
                distinct_roots,
                database_path,
                worker_snapshot,
                worker_stopped,
                receiver,
                event_sender,
            );
        });
        Self {
            snapshot,
            sender,
            stopped,
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.read().unwrap().clone()
    }

    pub(crate) fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.sender.send(Message::Stop);
    }

    pub fn refresh(&self) {
        let _ = self.sender.send(Message::Refresh);
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    roots: Vec<PathBuf>,
    database_path: Option<PathBuf>,
    snapshot: Arc<RwLock<Snapshot>>,
    stopped: Arc<AtomicBool>,
    receiver: Receiver<Message>,
    event_sender: Sender<Message>,
) {
    let mut database = Database::open(database_path.as_deref());
    let mut entries = database.load(&roots);
    let mut plan = fs_events::Plan::new(&roots);
    let mut identity = plan.as_ref().map(|plan| plan.identities.clone());
    let baseline = plan
        .as_ref()
        .map(|plan| plan.current_ids())
        .unwrap_or_default();
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let latest_id = fs_events::current_id();
    // Zero is a valid conservative checkpoint: FSEvents replays all available history.
    let saved = database.checkpoint.clone().filter(|saved| {
        identity.as_ref() == Some(&saved.roots)
            && saved.event_ids.len() == roots.len()
            && saved
                .event_ids
                .iter()
                .zip(&baseline)
                .all(|(id, baseline)| *id != u64::MAX && *id <= latest_id.max(*baseline))
            && now
                .checked_sub(saved.verified_at)
                .is_some_and(|age| age < 7 * 24 * 60 * 60)
            && saved.errors.iter().all(|(path, _)| {
                let path = PathBuf::from(std::ffi::OsString::from_vec(path.clone()));
                path.is_absolute() && roots.iter().any(|root| path.starts_with(root))
            })
    });
    let mut processed_ids = saved
        .as_ref()
        .map_or(baseline, |saved| saved.event_ids.clone());
    let mut verified_at = saved.as_ref().map_or(now, |saved| saved.verified_at);
    let mut watch_warning = None;
    let mut generation = 0;
    // Subscribe before crawling; events created during the crawl remain queued.
    let mut watcher = start_watcher(
        &roots,
        plan.as_ref(),
        &processed_ids,
        generation,
        &event_sender,
        &mut watch_warning,
    );
    let mut replaying = saved.is_some() && watcher.is_some();
    #[cfg(test)]
    {
        snapshot.write().unwrap().replayed = replaying;
    }
    let mut pending_history: BTreeSet<usize> = if replaying {
        (0..roots.len()).collect()
    } else {
        BTreeSet::new()
    };
    let mut errors: Errors = saved
        .as_ref()
        .map(|saved| {
            saved
                .errors
                .iter()
                .map(|(path, error)| {
                    (
                        PathBuf::from(std::ffi::OsString::from_vec(path.clone())),
                        error.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    publish(
        &snapshot,
        &entries,
        &errors,
        &watch_warning,
        &database.warning,
        true,
    );
    if replaying {
        // Access permissions can change without a filesystem event.
        let failed: BTreeMap<_, _> = errors.keys().cloned().map(|path| (path, true)).collect();
        let changed = apply_paths(
            failed,
            &mut entries,
            &mut errors,
            &stopped,
            database.excluded.as_deref(),
        );
        set_checkpoint(
            &mut database,
            identity.clone(),
            processed_ids.clone(),
            verified_at,
            &errors,
            watcher.is_some(),
        );
        database.save(&entries, &changed, &stopped);
    } else {
        set_checkpoint(
            &mut database,
            identity.clone(),
            processed_ids.clone(),
            verified_at,
            &errors,
            watcher.is_some(),
        );
        rebuild(
            &roots,
            &mut entries,
            &mut errors,
            &snapshot,
            &stopped,
            &watch_warning,
            &mut database,
        );
    }
    let mut replay_started = Instant::now();
    while !stopped.load(Ordering::Relaxed) {
        let first = match receiver.recv_timeout(Duration::from_secs(1)) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout)
                if replaying && replay_started.elapsed() > Duration::from_secs(15) =>
            {
                // No completion sentinel means history cannot be trusted.
                Message::Refresh
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let mut rebuild_needed = false;
        let mut paths = BTreeMap::new();
        let mut history_done = false;
        let mut batch_ids = processed_ids.clone();
        let started = Instant::now();
        let mut message = first;
        loop {
            match message {
                Message::Stop => return,
                Message::Refresh => rebuild_needed = true,
                Message::Events(events) => {
                    for event in events {
                        if event.generation != generation {
                            continue;
                        }
                        if event.lost_history() {
                            rebuild_needed = true;
                        }
                        if event.history_done() {
                            history_done = true;
                            if let Some(i) = event.stream {
                                batch_ids[i] = batch_ids[i].max(event.id);
                                pending_history.remove(&i);
                            }
                            continue;
                        }
                        if event.recursive()
                            && roots
                                .iter()
                                .any(|root| root.starts_with(&event.path) && root != &event.path)
                        {
                            rebuild_needed = true;
                        }
                        if !roots.iter().any(|root| event.path.starts_with(root))
                            || database
                                .excluded
                                .as_ref()
                                .is_some_and(|excluded| event.path.starts_with(excluded))
                        {
                            continue;
                        }
                        if let Some(i) = event.stream {
                            batch_ids[i] = batch_ids[i].max(event.id);
                        }
                        let recursive = event.recursive();
                        paths
                            .entry(event.path)
                            .and_modify(|full| *full |= recursive)
                            .or_insert(recursive);
                    }
                }
            }
            // Bound batching even under continuous filesystem activity.
            if started.elapsed() >= Duration::from_secs(1) {
                break;
            }
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(next) => message = next,
                Err(_) => break,
            }
        }
        if rebuild_needed {
            replaying = false;
            verified_at = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            drop(watcher.take());
            generation += 1;
            plan = fs_events::Plan::new(&roots);
            identity = plan.as_ref().map(|plan| plan.identities.clone());
            processed_ids = plan
                .as_ref()
                .map(|plan| plan.current_ids())
                .unwrap_or_default();
            watcher = start_watcher(
                &roots,
                plan.as_ref(),
                &processed_ids,
                generation,
                &event_sender,
                &mut watch_warning,
            );
            pending_history.clear();
            set_checkpoint(
                &mut database,
                identity.clone(),
                processed_ids.clone(),
                verified_at,
                &errors,
                watcher.is_some(),
            );
            rebuild(
                &roots,
                &mut entries,
                &mut errors,
                &snapshot,
                &stopped,
                &watch_warning,
                &mut database,
            );
        } else if !paths.is_empty() || history_done {
            let changed = apply_paths(
                paths,
                &mut entries,
                &mut errors,
                &stopped,
                database.excluded.as_deref(),
            );
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            processed_ids = batch_ids;
            if history_done && pending_history.is_empty() {
                replaying = false;
            }
            set_checkpoint(
                &mut database,
                identity.clone(),
                processed_ids.clone(),
                verified_at,
                &errors,
                watcher.is_some(),
            );
            database.save(&entries, &changed, &stopped);
            publish(
                &snapshot,
                &entries,
                &errors,
                &watch_warning,
                &database.warning,
                replaying,
            );
        }
        if !replaying {
            replay_started = Instant::now();
        }
    }
}

fn start_watcher(
    roots: &[PathBuf],
    plan: Option<&fs_events::Plan>,
    since: &[u64],
    generation: u64,
    sender: &Sender<Message>,
    warning: &mut Option<String>,
) -> Option<fs_events::Watcher> {
    if roots.is_empty() {
        return None;
    }
    match fs_events::Watcher::start(roots, plan, since, generation, sender.clone()) {
        Ok(watcher) => {
            *warning = None;
            Some(watcher)
        }
        Err(error) => {
            *warning = Some(format!("Live updates unavailable: {error}. Use Rebuild."));
            None
        }
    }
}

fn set_checkpoint(
    database: &mut Database,
    roots: Option<Vec<fs_events::RootIdentity>>,
    event_ids: Vec<u64>,
    verified_at: u64,
    errors: &Errors,
    watching: bool,
) {
    database.checkpoint = roots.filter(|_| watching).map(|roots| Checkpoint {
        roots,
        event_ids,
        verified_at,
        errors: errors
            .iter()
            .map(|(path, error)| (path.as_os_str().as_bytes().to_vec(), error.clone()))
            .collect(),
    });
}

fn apply_paths(
    paths: BTreeMap<PathBuf, bool>,
    entries: &mut Entries,
    errors: &mut Errors,
    stopped: &AtomicBool,
    excluded: Option<&Path>,
) -> BTreeSet<PathBuf> {
    let mut scanned: Vec<PathBuf> = Vec::new();
    let mut changed = BTreeSet::new();
    for (path, recursive) in paths {
        if stopped.load(Ordering::Relaxed) {
            return changed;
        }
        if scanned.iter().any(|parent| path.starts_with(parent)) {
            continue;
        }
        if !recursive && entries.get(&path).is_some_and(|entry| entry.is_dir) {
            changed.extend(refresh_directory(&path, entries, errors, stopped, excluded));
            // A shallow parent update does not cover queued events in its existing children.
            continue;
        }
        remove_subtree(&path, entries, errors, &mut changed);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => scan(&path, entries, errors, stopped, excluded, usize::MAX),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                errors.insert(path.clone(), error.to_string());
            }
        }
        changed.extend(
            entries
                .range(path.clone()..)
                .take_while(|(entry, _)| entry.starts_with(&path))
                .map(|(entry, _)| entry.clone()),
        );
        scanned.push(path);
    }
    changed
}

fn remove_subtree(
    path: &Path,
    entries: &mut Entries,
    errors: &mut Errors,
    changed: &mut BTreeSet<PathBuf>,
) {
    let removed: Vec<_> = entries
        .range(path.to_owned()..)
        .take_while(|(entry, _)| entry.starts_with(path))
        .map(|(entry, _)| entry.clone())
        .collect();
    for entry in removed {
        entries.remove(&entry);
        changed.insert(entry);
    }
    let removed: Vec<_> = errors
        .range(path.to_owned()..)
        .take_while(|(entry, _)| entry.starts_with(path))
        .map(|(entry, _)| entry.clone())
        .collect();
    for entry in removed {
        errors.remove(&entry);
    }
}

fn refresh_directory(
    path: &Path,
    entries: &mut Entries,
    errors: &mut Errors,
    stopped: &AtomicBool,
    excluded: Option<&Path>,
) -> BTreeSet<PathBuf> {
    let mut changed = BTreeSet::new();
    if matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        remove_subtree(path, entries, errors, &mut changed);
        return changed;
    }
    let previous: Entries = entries
        .range(path.to_owned()..)
        .take_while(|(entry, _)| entry.starts_with(path))
        .filter(|(entry, _)| entry.as_path() == path || entry.parent() == Some(path))
        .map(|(entry, value)| (entry.clone(), value.clone()))
        .collect();
    let mut current = Entries::new();
    let mut scan_errors = Errors::new();
    scan(path, &mut current, &mut scan_errors, stopped, excluded, 1);
    let new_folders: Vec<_> = current
        .iter()
        .filter(|(path, entry)| entry.is_dir && previous.get(*path).is_none_or(|old| !old.is_dir))
        .map(|(path, _)| path.clone())
        .collect();
    for folder in new_folders {
        scan(
            &folder,
            &mut current,
            &mut scan_errors,
            stopped,
            excluded,
            usize::MAX,
        );
    }
    if stopped.load(Ordering::Relaxed) {
        return changed;
    }
    errors.retain(|entry, _| entry.as_path() != path && entry.parent() != Some(path));
    for (entry, old) in &previous {
        if current
            .get(entry)
            .is_none_or(|now| old.is_dir != now.is_dir)
        {
            remove_subtree(entry, entries, errors, &mut changed);
        }
    }
    for (path, entry) in current {
        if entries.get(&path) != Some(&entry) {
            changed.insert(path.clone());
            entries.insert(path, entry);
        }
    }
    errors.extend(scan_errors);
    changed
}

fn rebuild(
    roots: &[PathBuf],
    entries: &mut Entries,
    errors: &mut Errors,
    snapshot: &RwLock<Snapshot>,
    stopped: &AtomicBool,
    watch_warning: &Option<String>,
    database: &mut Database,
) {
    {
        let mut snapshot = snapshot.write().unwrap();
        snapshot.scanning = true;
        #[cfg(test)]
        {
            snapshot.full_scans += 1;
        }
        snapshot.revision += 1;
    }
    let previous = std::mem::take(entries);
    errors.clear();
    for root in roots {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        scan(
            root,
            entries,
            errors,
            stopped,
            database.excluded.as_deref(),
            usize::MAX,
        );
    }
    if !stopped.load(Ordering::Relaxed) {
        let changed = previous
            .keys()
            .chain(entries.keys())
            .filter(|path| previous.get(*path) != entries.get(*path))
            .cloned()
            .collect();
        if let Some(checkpoint) = &mut database.checkpoint {
            checkpoint.errors = errors
                .iter()
                .map(|(path, error)| (path.as_os_str().as_bytes().to_vec(), error.clone()))
                .collect();
        }
        database.save(entries, &changed, stopped);
        publish(
            snapshot,
            entries,
            errors,
            watch_warning,
            &database.warning,
            false,
        );
    }
}

fn scan(
    root: &Path,
    entries: &mut Entries,
    errors: &mut Errors,
    stopped: &AtomicBool,
    excluded: Option<&Path>,
    depth: usize,
) {
    if stopped.load(Ordering::Relaxed) || excluded.is_some_and(|path| root.starts_with(path)) {
        return;
    }
    let root_entry = match bulk::metadata(root) {
        Ok(entry) => entry,
        Err(error) => {
            errors.insert(root.to_owned(), error.to_string());
            return;
        }
    };
    let mut folders = Vec::new();
    if root_entry.is_dir && depth > 0 {
        folders.push((root.to_owned(), 0));
    }
    entries.insert(root.to_owned(), Arc::new(root_entry));
    while let Some((folder, level)) = folders.pop() {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        match bulk::read_directory(&folder, stopped) {
            Ok(children) => {
                for (path, entry) in children {
                    if stopped.load(Ordering::Relaxed) {
                        return;
                    }
                    if excluded.is_some_and(|excluded| path.starts_with(excluded)) {
                        continue;
                    }
                    match entry {
                        Ok(entry) => {
                            if entry.is_dir && level + 1 < depth {
                                folders.push((path.clone(), level + 1));
                            }
                            entries.insert(path, Arc::new(entry));
                        }
                        Err(error) => {
                            errors.insert(path, error.to_string());
                        }
                    }
                }
            }
            Err(error) => {
                errors.insert(folder, error.to_string());
            }
        }
    }
}

fn publish(
    snapshot: &RwLock<Snapshot>,
    entries: &Entries,
    errors: &Errors,
    watch_warning: &Option<String>,
    database_warning: &Option<String>,
    scanning: bool,
) {
    let scan_warning = errors.first_key_value().map(|(path, error)| {
        format!(
            "{} inaccessible paths; {}: {error}",
            errors.len(),
            path.display()
        )
    });
    let warnings: Vec<_> = [
        watch_warning.clone(),
        database_warning.clone(),
        scan_warning,
    ]
    .into_iter()
    .flatten()
    .collect();
    let warning = (!warnings.is_empty()).then(|| warnings.join("\n"));
    let mut snapshot = snapshot.write().unwrap();
    snapshot.entries = Arc::new(entries.values().cloned().collect());
    snapshot.scanning = scanning;
    snapshot.skipped = errors.len();
    snapshot.warning = warning;
    snapshot.revision += 1;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "everythingmac-index-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wait_for(index: &Index, condition: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let snapshot = index.snapshot();
            if !snapshot.scanning && condition(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "Timed out waiting for index: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    #[test]
    fn scans_metadata_and_refreshes_without_following_symlinks() {
        let root = TempDir::new();
        let outside = TempDir::new();
        fs::write(root.0.join(".hidden"), "hello").unwrap();
        fs::write(outside.0.join("outside.txt"), "outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside.0, root.0.join("link")).unwrap();
        let index = Index::start(vec![root.0.clone()], None);
        let snapshot = wait_for(&index, |snapshot| {
            snapshot
                .entries
                .iter()
                .any(|entry| entry.path.ends_with(".hidden"))
        });
        let hidden = snapshot
            .entries
            .iter()
            .find(|entry| entry.path.ends_with(".hidden"))
            .unwrap();
        assert_eq!(hidden.size, 5);
        assert!(hidden.modified.is_some());
        assert!(Arc::ptr_eq(&snapshot.entries, &index.snapshot().entries));
        assert!(!hidden.is_dir);
        assert!(
            snapshot
                .entries
                .iter()
                .any(|entry| entry.path == root.0 && entry.is_dir)
        );
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.path.ends_with("outside.txt"))
        );
        #[cfg(unix)]
        assert!(
            !snapshot
                .entries
                .iter()
                .find(|entry| entry.path.ends_with("link"))
                .unwrap()
                .is_dir
        );
        let revision = snapshot.revision;
        fs::write(root.0.join("refreshed.txt"), "refresh").unwrap();
        index.refresh();
        wait_for(&index, |snapshot| {
            snapshot.revision > revision
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.path.ends_with("refreshed.txt"))
        });
    }

    #[test]
    fn watches_create_rename_and_delete_subtrees() {
        let root = TempDir::new();
        let index = Index::start(vec![root.0.clone()], None);
        wait_for(&index, |_| true);
        let created = root.0.join("created");
        fs::create_dir(&created).unwrap();
        fs::write(created.join("file.txt"), "live").unwrap();
        wait_for(&index, |snapshot| {
            snapshot
                .entries
                .iter()
                .any(|entry| entry.path == created.join("file.txt"))
        });
        let renamed = root.0.join("renamed");
        fs::rename(&created, &renamed).unwrap();
        wait_for(&index, |snapshot| {
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.path.starts_with(&created))
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.path == renamed.join("file.txt"))
        });
        fs::remove_dir_all(&renamed).unwrap();
        wait_for(&index, |snapshot| {
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.path.starts_with(&renamed))
        });
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_root_symlinks() {
        let root = TempDir::new();
        let target = root.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("file"), "contents").unwrap();
        let link = root.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let linked_index = Index::start(vec![link.clone()], None);
        let linked = wait_for(&linked_index, |_| true);
        assert_eq!(linked.entries.len(), 1);
        assert_eq!(linked.entries[0].path, link);
        assert!(!linked.entries[0].is_dir);
    }

    #[test]
    fn path_order_keeps_subtrees_contiguous_before_similarly_named_siblings() {
        let paths: BTreeMap<_, _> = [
            "/foo-bar/child",
            "/foo/child/nested",
            "/foo",
            "/foo/child",
            "/foo-bar",
        ]
        .into_iter()
        .map(|path| (PathBuf::from(path), ()))
        .collect();
        let root = PathBuf::from("/foo");
        let subtree: Vec<_> = paths
            .range(root.clone()..)
            .take_while(|(path, _)| path.starts_with(&root))
            .map(|(path, _)| path.as_path())
            .collect();
        assert_eq!(
            subtree,
            [
                Path::new("/foo"),
                Path::new("/foo/child"),
                Path::new("/foo/child/nested")
            ]
        );
    }

    #[test]
    fn directory_diff_preserves_existing_subtrees_and_updates_new_deleted_and_replaced_children() {
        let root = TempDir::new();
        for name in ["existing/deep/file", "gone/sub/file", "swap/old"] {
            let path = root.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "old").unwrap();
        }
        fs::write(root.0.join("file-to-dir"), "old").unwrap();
        let stopped = AtomicBool::new(false);
        let mut entries = Entries::new();
        let mut errors = Errors::new();
        scan(
            &root.0,
            &mut entries,
            &mut errors,
            &stopped,
            None,
            usize::MAX,
        );
        let deep = root.0.join("existing/deep/file");
        let cached_deep = entries[&deep].clone();
        let denied = root.0.join("existing/deep/denied");
        errors.insert(denied.clone(), "Access denied".into());
        fs::write(&deep, "changed deeper in the tree").unwrap();
        fs::remove_dir_all(root.0.join("gone")).unwrap();
        fs::remove_dir_all(root.0.join("swap")).unwrap();
        fs::write(root.0.join("swap"), "now a file").unwrap();
        fs::remove_file(root.0.join("file-to-dir")).unwrap();
        fs::create_dir_all(root.0.join("file-to-dir/sub")).unwrap();
        fs::write(root.0.join("file-to-dir/sub/new"), "new").unwrap();
        fs::create_dir_all(root.0.join("new/sub")).unwrap();
        fs::write(root.0.join("new/sub/new"), "new").unwrap();
        let changed = refresh_directory(&root.0, &mut entries, &mut errors, &stopped, None);
        assert!(
            Arc::ptr_eq(&entries[&deep], &cached_deep),
            "Existing descendant entries must remain untouched"
        );
        assert!(
            errors.contains_key(&denied),
            "Unvisited descendant errors must remain visible"
        );
        assert!(
            !entries
                .keys()
                .any(|path| path.starts_with(root.0.join("gone")))
        );
        assert!(changed.contains(&root.0.join("gone/sub/file")));
        assert!(!entries.contains_key(&root.0.join("swap/old")));
        assert!(!entries[&root.0.join("swap")].is_dir);
        assert!(entries.contains_key(&root.0.join("file-to-dir/sub/new")));
        assert!(entries.contains_key(&root.0.join("new/sub/new")));
        assert!(
            refresh_directory(&root.0, &mut entries, &mut errors, &stopped, None).is_empty(),
            "Replayed parent updates must be idempotent"
        );
        let changed = refresh_directory(
            deep.parent().unwrap(),
            &mut entries,
            &mut errors,
            &stopped,
            None,
        );
        assert!(changed.contains(&deep));
        assert_eq!(entries[&deep].size, 26);
        let previous = entries.clone();
        stopped.store(true, Ordering::Relaxed);
        assert!(refresh_directory(&root.0, &mut entries, &mut errors, &stopped, None).is_empty());
        assert_eq!(
            entries, previous,
            "Cancellation must leave the previous index intact"
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_diff_handles_disappearing_folders_and_symlink_replacements() {
        let root = TempDir::new();
        let outside = TempDir::new();
        let folder = root.0.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("old"), "old").unwrap();
        fs::write(outside.0.join("outside"), "outside").unwrap();
        let stopped = AtomicBool::new(false);
        let mut entries = Entries::new();
        let mut errors = Errors::new();
        scan(
            &root.0,
            &mut entries,
            &mut errors,
            &stopped,
            None,
            usize::MAX,
        );
        fs::remove_dir_all(&folder).unwrap();
        std::os::unix::fs::symlink(&outside.0, &folder).unwrap();
        refresh_directory(&folder, &mut entries, &mut errors, &stopped, None);
        assert!(!entries[&folder].is_dir);
        assert!(!entries.contains_key(&folder.join("old")));
        assert!(!entries.contains_key(&folder.join("outside")));
        fs::remove_file(&folder).unwrap();
        refresh_directory(&folder, &mut entries, &mut errors, &stopped, None);
        assert!(!entries.contains_key(&folder));
        assert!(errors.is_empty(), "A deleted folder is not a read failure");
    }

    #[test]
    fn reports_missing_roots() {
        let root = TempDir::new();
        let index = Index::start(vec![root.0.join("missing")], None);
        let snapshot = wait_for(&index, |_| true);
        assert_eq!(snapshot.skipped, 1);
        assert!(snapshot.warning.is_some());
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    use crate::index_db::Database;
    use std::{
        fs,
        os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    };

    fn fixture() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "everything-db-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn wait(index: &Index, check: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let snapshot = index.snapshot();
            if !snapshot.scanning && check(&snapshot) {
                return snapshot;
            }
            assert!(Instant::now() < end, "Index timed out: {snapshot:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn database_survives_restart_reconciles_offline_changes_and_saves_live_updates() {
        let fixture = fixture();
        let root = fixture.join("files");
        let db_path = fixture.join("cache/index.sqlite3");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("old.txt"), "old").unwrap();
        fs::write(root.join("edited.txt"), "old").unwrap();
        let index = Index::start(vec![root.clone()], Some(db_path.clone()));
        let first = wait(&index, |s| s.entries.len() == 3);
        assert_eq!(first.full_scans, 1);
        assert_eq!(
            fs::metadata(&db_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut db = Database::open(Some(&db_path));
        assert_eq!(db.load(&[root.clone()]).len(), 3);
        drop(index);
        fs::rename(root.join("old.txt"), root.join("renamed.txt")).unwrap();
        fs::write(root.join("edited.txt"), "updated while closed").unwrap();
        fs::write(root.join("new.txt"), "new").unwrap();
        // A cache read returns the saved snapshot before filesystem reconciliation.
        let cached = db.load(&[root.clone()]);
        assert!(cached.contains_key(&root.join("old.txt")));
        assert!(!cached.contains_key(&root.join("new.txt")));
        let index = Index::start(vec![root.clone()], Some(db_path.clone()));
        let replayed = wait(&index, |s| {
            s.entries.len() == 4
                && s.entries
                    .iter()
                    .any(|e| e.path == root.join("edited.txt") && e.size == 20)
        });
        assert!(
            replayed.replayed,
            "A valid cache must select historical replay; native history-loss flags may still request recovery"
        );
        let stored = db.load(&[root.clone()]);
        assert!(stored.contains_key(&root.join("renamed.txt")));
        assert!(stored.contains_key(&root.join("new.txt")));
        assert!(!stored.contains_key(&root.join("old.txt")));
        assert_eq!(stored[&root.join("edited.txt")].size, 20);
        fs::remove_file(root.join("new.txt")).unwrap();
        wait(&index, |s| {
            !s.entries.iter().any(|e| e.path == root.join("new.txt"))
        });
        assert!(!db.load(&[root.clone()]).contains_key(&root.join("new.txt")));
        drop(index);
        let index = Index::start(Vec::new(), Some(db_path.clone()));
        wait(&index, |s| s.entries.is_empty());
        assert!(
            db.load(&[root]).is_empty(),
            "Removed roots must be pruned from disk"
        );
        drop(index);
        drop(db);
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn replay_handles_multiple_roots_quiet_reopens_and_history_loss() {
        let fixture = fixture();
        let roots = vec![fixture.join("one"), fixture.join("two")];
        for root in &roots {
            fs::create_dir(root).unwrap();
            fs::write(root.join("old"), b"old").unwrap();
        }
        let path = fixture.join("cache/index.sqlite3");
        let index = Index::start(roots.clone(), Some(path.clone()));
        wait(&index, |s| s.entries.len() == 4);
        drop(index);
        fs::create_dir(roots[0].join("new")).unwrap();
        fs::write(roots[0].join("new/deep"), b"deep").unwrap();
        fs::remove_file(roots[1].join("old")).unwrap();
        let index = Index::start(roots.clone(), Some(path.clone()));
        let replayed = wait(&index, |s| {
            s.entries.len() == 5
                && s.entries
                    .iter()
                    .any(|e| e.path == roots[0].join("new/deep"))
        });
        assert!(replayed.replayed);
        let checkpoint = Database::open(Some(&path)).checkpoint.unwrap();
        assert_eq!(checkpoint.event_ids.len(), 2);
        assert!(checkpoint.event_ids.iter().all(|id| *id > 0));
        drop(index);
        // macOS can return zero before its history has been flushed; replay from zero is safe.
        let mut database = Database::open(Some(&path));
        let cached = database.load(&roots);
        database.checkpoint.as_mut().unwrap().event_ids.fill(0);
        database.save(&cached, &BTreeSet::new(), &AtomicBool::new(false));
        assert!(database.warning.is_none());
        drop(database);
        let index = Index::start(roots.clone(), Some(path.clone()));
        let quiet = wait(&index, |s| s.entries.len() == 5);
        assert!(
            quiet.replayed,
            "A quiet restart must complete without a verification walk"
        );
        // Force the real worker through overflow recovery, independently of OS timing.
        fs::write(roots[0].join("old"), b"updated after overflow").unwrap();
        index
            .sender
            .send(Message::Events(vec![fs_events::Event {
                path: roots[0].clone(),
                flags: fsevent_sys::kFSEventStreamEventFlagKernelDropped,
                id: 0,
                stream: Some(0),
                generation: 0,
            }]))
            .unwrap();
        wait(&index, |s| {
            s.full_scans > quiet.full_scans
                && s.entries
                    .iter()
                    .any(|e| e.path == roots[0].join("old") && e.size == 22)
        });
        drop(index);
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn raw_paths_times_and_cancelled_transactions_round_trip() {
        let fixture = fixture();
        let path = fixture.join("cache/index.sqlite3");
        let mut db = Database::open(Some(&path));
        let raw = fixture.join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec()));
        let entry = Arc::new(Entry {
            path: raw.clone(),
            is_dir: false,
            size: u64::MAX,
            modified: Some(SystemTime::UNIX_EPOCH - Duration::new(10, 123)),
        });
        let mut entries = Entries::from([(raw.clone(), entry.clone())]);
        let changed = BTreeSet::from([raw.clone()]);
        let stopped = AtomicBool::new(false);
        db.checkpoint = Some(Checkpoint {
            roots: Vec::new(),
            event_ids: vec![1],
            verified_at: 123,
            errors: vec![(raw.as_os_str().as_bytes().to_vec(), "denied".into())],
        });
        db.save(&entries, &changed, &stopped);
        assert_eq!(Database::open(Some(&path)).checkpoint, db.checkpoint);
        assert!(db.warning.is_none(), "{:?}", db.warning);
        assert_eq!(db.load(&[fixture.clone()])[&raw], entry);
        entries.clear();
        stopped.store(true, Ordering::Relaxed);
        db.checkpoint.as_mut().unwrap().event_ids = vec![2];
        db.save(&entries, &changed, &stopped);
        assert!(db.warning.is_some());
        assert_eq!(
            Database::open(Some(&path)).checkpoint.unwrap().event_ids,
            vec![1],
            "Rows and cursor must roll back together"
        );
        assert!(
            db.load(&[fixture.clone()]).contains_key(&raw),
            "Cancelled transaction must preserve previous snapshot"
        );
        stopped.store(false, Ordering::Relaxed);
        db.save(&entries, &BTreeSet::new(), &stopped);
        assert!(
            db.load(&[fixture.clone()]).is_empty(),
            "Failed writes must be retried"
        );
        assert_eq!(
            Database::open(Some(&path)).checkpoint.unwrap().event_ids,
            vec![2]
        );
        db.checkpoint.as_mut().unwrap().event_ids = vec![3];
        db.save(&entries, &BTreeSet::new(), &stopped);
        assert_eq!(
            Database::open(Some(&path)).checkpoint.unwrap().event_ids,
            vec![3],
            "A checkpoint must save even with no changed file rows"
        );
        drop(db);
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn migrates_version_one_without_losing_files_and_rejects_stale_roots() {
        let fixture = fixture();
        let root = fixture.join("files");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("file"), b"old").unwrap();
        let path = fixture.join("cache/index.sqlite3");
        let index = Index::start(vec![root.clone()], Some(path.clone()));
        wait(&index, |s| s.entries.len() == 2);
        index.stop();
        // The worker stops asynchronously; serialize behind any finishing transaction.
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.busy_timeout(Duration::from_secs(3)).unwrap();
        connection
            .execute_batch("DROP TABLE state; PRAGMA user_version = 1;")
            .unwrap();
        drop(index);
        let mut database = Database::open(Some(&path));
        assert!(database.warning.is_none(), "{:?}", database.warning);
        assert_eq!(database.load(&[root.clone()]).len(), 2);
        assert!(database.checkpoint.is_none());
        drop(database);
        fs::write(root.join("new"), b"new").unwrap();
        let index = Index::start(vec![root.clone()], Some(path.clone()));
        let built = wait(&index, |s| s.entries.len() == 3);
        assert_eq!(built.full_scans, 1, "Version 1 needs an initial checkpoint");
        drop(index);
        // Changed configured roots cannot reuse the old checkpoint.
        let added = fixture.join("added");
        fs::create_dir(&added).unwrap();
        fs::write(added.join("child"), b"new root").unwrap();
        let index = Index::start(vec![root.clone(), added.clone()], Some(path));
        let built = wait(&index, |s| {
            s.entries.iter().any(|e| e.path == added.join("child"))
        });
        assert_eq!(built.full_scans, 1);
        drop(index);
        drop(connection);
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn corrupt_database_is_preserved_and_own_cache_is_not_indexed() {
        let fixture = fixture();
        let db_path = fixture.join("cache/index.sqlite3");
        fs::create_dir(db_path.parent().unwrap()).unwrap();
        fs::write(&db_path, "broken database").unwrap();
        let index = Index::start(vec![fixture.clone()], Some(db_path.clone()));
        let snapshot = wait(&index, |s| s.warning.is_some());
        assert!(snapshot.warning.unwrap().contains("database"));
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|e| e.path.starts_with(db_path.parent().unwrap()))
        );
        assert_eq!(fs::read_to_string(&db_path).unwrap(), "broken database");
        drop(index);
        fs::remove_dir_all(fixture).unwrap();
    }
}
