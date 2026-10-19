use crate::index::{Entry, is_cloud_path};
use std::{
    collections::{BTreeMap, BTreeSet, hash_map::DefaultHasher},
    fs::{self, File, Metadata, OpenOptions},
    hash::Hasher,
    io::{self, Read},
    os::macos::fs::MetadataExt as _,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

const BUFFER_SIZE: usize = 64 * 1024;
// SF_DATALESS in macOS sys/stat.h: reading a placeholder would download its contents.
const SF_DATALESS: u32 = 0x4000_0000;

#[derive(Clone, Default)]
pub(crate) struct Report {
    pub results: Arc<Vec<Arc<Entry>>>,
    pub group_ends: Vec<usize>,
    pub skipped: usize,
    pub warning: Option<String>,
}

impl Report {
    fn add_group(&mut self, entries: impl IntoIterator<Item = Arc<Entry>>) {
        let results = Arc::make_mut(&mut self.results);
        results.extend(entries);
        self.group_ends.push(results.len());
    }

    fn skip(&mut self, count: usize, error: impl std::fmt::Display) {
        self.skipped += count;
        if self.warning.is_none() {
            self.warning = Some(error.to_string());
        }
    }

    // Keep scan groups conservative as live index updates remove or change their members.
    pub fn retain_current(&mut self, entries: &[Arc<Entry>]) {
        let mut start = 0;
        let mut groups = Vec::new();
        for &end in &self.group_ends {
            let group: Vec<_> = self.results[start..end]
                .iter()
                .filter(|entry| {
                    entries
                        .binary_search_by(|current| current.path.cmp(&entry.path))
                        .is_ok_and(|i| entries[i] == **entry)
                })
                .cloned()
                .collect();
            let removed = end - start - group.len();
            if removed > 0 {
                self.skipped += removed;
                self.warning = Some("Files changed since this scan; scan again to refresh.".into());
            }
            if group.len() > 1 {
                groups.push(group);
            }
            start = end;
        }
        self.results = Arc::new(Vec::new());
        self.group_ends.clear();
        for group in groups {
            self.add_group(group);
        }
    }
}

struct Candidate {
    entry: Arc<Entry>,
    metadata: Metadata,
}

fn unchanged(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
        && b.st_flags() & SF_DATALESS == 0
}

impl Candidate {
    fn verify(&self, metadata: &Metadata) -> io::Result<()> {
        if unchanged(&self.metadata, metadata) {
            Ok(())
        } else {
            Err(io::Error::other("File changed during duplicate scan"))
        }
    }

    fn open(&self) -> io::Result<File> {
        self.verify(&fs::symlink_metadata(&self.entry.path)?)?;
        // Do not follow a replaced symlink or block on a replaced FIFO.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.entry.path)?;
        self.verify(&file.metadata()?)?;
        Ok(file)
    }

    fn finish(&self, file: &File) -> io::Result<()> {
        self.verify(&file.metadata()?)?;
        self.verify(&fs::symlink_metadata(&self.entry.path)?)
    }
}

fn check_cancelled(cancelled: &impl Fn() -> bool) -> io::Result<()> {
    if cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Duplicate scan cancelled",
        ))
    } else {
        Ok(())
    }
}

fn fingerprint(candidate: &Candidate, cancelled: &impl Fn() -> bool) -> io::Result<u64> {
    let mut file = candidate.open()?;
    let mut hash = DefaultHasher::new();
    let mut buffer = [0; BUFFER_SIZE];
    let mut remaining = candidate.metadata.len();
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let count = remaining.min(BUFFER_SIZE as u64) as usize;
        file.read_exact(&mut buffer[..count])?;
        hash.write(&buffer[..count]);
        remaining -= count as u64;
    }
    candidate.finish(&file)?;
    Ok(hash.finish())
}

fn same_contents(a: &Candidate, b: &Candidate, cancelled: &impl Fn() -> bool) -> io::Result<bool> {
    let mut left = a.open()?;
    let mut right = b.open()?;
    let mut left_buffer = [0; BUFFER_SIZE];
    let mut right_buffer = [0; BUFFER_SIZE];
    let mut remaining = a.metadata.len();
    if remaining != b.metadata.len() {
        return Ok(false);
    }
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let count = remaining.min(BUFFER_SIZE as u64) as usize;
        left.read_exact(&mut left_buffer[..count])?;
        right.read_exact(&mut right_buffer[..count])?;
        if left_buffer[..count] != right_buffer[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    a.finish(&left)?;
    b.finish(&right)?;
    Ok(true)
}

pub(crate) fn find(entries: &[Arc<Entry>], current: &AtomicU64, generation: u64) -> Option<Report> {
    let cancelled = || current.load(Ordering::Relaxed) != generation;
    let mut report = Report::default();
    let mut sizes = BTreeMap::<u64, Vec<Candidate>>::new();
    let mut identities = BTreeSet::new();
    for entry in entries {
        if cancelled() {
            return None;
        }
        if entry.is_dir || is_cloud_path(&entry.path) {
            continue;
        }
        match fs::symlink_metadata(&entry.path) {
            Ok(metadata) if metadata.is_file() => {
                if metadata.st_flags() & SF_DATALESS != 0 {
                    report.skip(
                        1,
                        format!("{} is not stored locally.", entry.path.display()),
                    );
                } else if metadata.len() != entry.size || metadata.modified().ok() != entry.modified
                {
                    report.skip(
                        1,
                        format!(
                            "{} changed; scan again after the index updates.",
                            entry.path.display()
                        ),
                    );
                } else if identities.insert((metadata.dev(), metadata.ino())) {
                    sizes.entry(metadata.len()).or_default().push(Candidate {
                        entry: entry.clone(),
                        metadata,
                    });
                }
            }
            Ok(_) => {}
            Err(error) => report.skip(1, format!("{}: {error}", entry.path.display())),
        }
    }
    for candidates in sizes.into_values().rev().filter(|files| files.len() > 1) {
        let mut hashes = BTreeMap::<u64, Vec<Candidate>>::new();
        for candidate in candidates {
            if cancelled() {
                return None;
            }
            match fingerprint(&candidate, &cancelled) {
                Ok(hash) => hashes.entry(hash).or_default().push(candidate),
                Err(error) => {
                    report.skip(1, format!("{}: {error}", candidate.entry.path.display()))
                }
            }
        }
        for candidates in hashes.into_values().filter(|files| files.len() > 1) {
            if cancelled() {
                return None;
            }
            match confirm_groups(candidates, &cancelled) {
                Ok(groups) => {
                    for group in groups {
                        let mut valid = Vec::new();
                        for candidate in group {
                            if cancelled() {
                                return None;
                            }
                            match fs::symlink_metadata(&candidate.entry.path)
                                .and_then(|m| candidate.verify(&m))
                            {
                                Ok(()) => valid.push(candidate.entry),
                                Err(error) => report.skip(1, error),
                            }
                        }
                        if valid.len() > 1 {
                            report.add_group(valid);
                        }
                    }
                }
                Err((count, error)) => report.skip(count, error),
            }
        }
    }
    if cancelled() { None } else { Some(report) }
}

// Fingerprints only narrow candidates: byte comparison also handles hash collisions.
fn confirm_groups(
    candidates: Vec<Candidate>,
    cancelled: &impl Fn() -> bool,
) -> Result<Vec<Vec<Candidate>>, (usize, io::Error)> {
    let count = candidates.len();
    let mut groups: Vec<Vec<Candidate>> = Vec::new();
    for candidate in candidates {
        check_cancelled(cancelled).map_err(|error| (count, error))?;
        let mut matching = None;
        for (i, group) in groups.iter().enumerate() {
            if same_contents(&group[0], &candidate, cancelled).map_err(|error| (count, error))? {
                matching = Some(i);
                break;
            }
        }
        if let Some(i) = matching {
            groups[i].push(candidate);
        } else {
            groups.push(vec![candidate]);
        }
    }
    groups.retain(|group| group.len() > 1);
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, path::PathBuf, sync::atomic::AtomicUsize};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "everything-duplicates-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn entry(&self, name: &str) -> Arc<Entry> {
            let path = self.0.join(name);
            let metadata = fs::symlink_metadata(&path).unwrap();
            Arc::new(Entry {
                path,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                allocated_size: metadata.blocks() * 512,
                modified: metadata.modified().ok(),
            })
        }

        fn write(&self, name: &str, bytes: &[u8]) -> Arc<Entry> {
            fs::write(self.0.join(name), bytes).unwrap();
            self.entry(name)
        }

        fn candidate(&self, name: &str) -> Candidate {
            Candidate {
                entry: self.entry(name),
                metadata: fs::symlink_metadata(self.0.join(name)).unwrap(),
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn groups_identical_contents_including_empty_files_and_ignores_links_and_special_files() {
        let fixture = Fixture::new();
        let data = vec![b'a'; BUFFER_SIZE * 2 + 3];
        let mut different = data.clone();
        *different.last_mut().unwrap() = b'b';
        let mut entries = vec![
            fixture.write("a.txt", &data),
            fixture.write("b.jpg", &data),
            fixture.write("different", &different),
            fixture.write("unique-size", b"unique"),
            fixture.write("small-a", b"small"),
            fixture.write("small-b", b"small"),
            fixture.write("empty-a", b""),
            fixture.write("empty-b", b""),
        ];
        fs::hard_link(fixture.0.join("a.txt"), fixture.0.join("hard-link")).unwrap();
        symlink(fixture.0.join("a.txt"), fixture.0.join("symlink")).unwrap();
        symlink("missing", fixture.0.join("broken-link")).unwrap();
        fs::create_dir(fixture.0.join("folder")).unwrap();
        let fifo =
            std::ffi::CString::new(fixture.0.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        entries.extend([
            fixture.entry("hard-link"),
            fixture.entry("symlink"),
            fixture.entry("broken-link"),
            fixture.entry("folder"),
            fixture.entry("fifo"),
        ]);
        let report = find(&entries, &AtomicU64::new(0), 0).unwrap();
        assert_eq!(report.group_ends, vec![2, 4, 6]);
        assert_eq!(report.skipped, 0);
        assert!(report.warning.is_none());
        let names: Vec<_> = report
            .results
            .iter()
            .map(|e| e.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["a.txt", "b.jpg", "small-a", "small-b", "empty-a", "empty-b"]
        );
        assert!(
            report
                .results
                .windows(2)
                .all(|pair| pair[0].size >= pair[1].size)
        );
    }

    #[test]
    fn confirms_bytes_even_when_fingerprints_collide() {
        let fixture = Fixture::new();
        for (name, bytes) in [("a", b"one"), ("b", b"two"), ("c", b"one"), ("d", b"two")] {
            fixture.write(name, bytes);
        }
        let groups = confirm_groups(
            ["a", "b", "c", "d"]
                .map(|name| fixture.candidate(name))
                .into(),
            &|| false,
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2);
        assert_eq!(groups[0][1].entry.path, fixture.0.join("c"));
        assert_eq!(groups[1][1].entry.path, fixture.0.join("d"));
    }

    #[test]
    fn reports_missing_and_changed_files_and_rejects_replaced_symlinks() {
        let fixture = Fixture::new();
        let a = fixture.write("a", b"same");
        let b = fixture.write("b", b"same");
        let missing = fixture.write("missing", b"same");
        let stale = fixture.write("stale", b"same");
        fs::remove_file(&missing.path).unwrap();
        fs::write(&stale.path, b"longer").unwrap();
        let report = find(&[a, b, missing, stale], &AtomicU64::new(0), 0).unwrap();
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.skipped, 2);
        assert!(report.warning.is_some());
        let candidate = fixture.candidate("a");
        fs::remove_file(&candidate.entry.path).unwrap();
        symlink(fixture.0.join("b"), &candidate.entry.path).unwrap();
        assert!(candidate.open().is_err());
        let candidate = fixture.candidate("b");
        fs::write(&candidate.entry.path, b"edit").unwrap();
        assert!(fingerprint(&candidate, &|| false).is_err());
    }

    #[test]
    fn cancels_before_scanning_and_during_reads_and_detects_changes_during_reads() {
        let fixture = Fixture::new();
        let a = fixture.write("a", &vec![0; BUFFER_SIZE * 3]);
        assert!(find(&[a], &AtomicU64::new(2), 1).is_none());
        let calls = AtomicUsize::new(0);
        assert_eq!(
            fingerprint(&fixture.candidate("a"), &|| calls
                .fetch_add(1, Ordering::Relaxed)
                > 0)
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        let calls = AtomicUsize::new(0);
        let candidate = fixture.candidate("a");
        assert!(
            fingerprint(&candidate, &|| {
                if calls.fetch_add(1, Ordering::Relaxed) == 1 {
                    fs::write(&candidate.entry.path, b"edited").unwrap();
                }
                false
            })
            .is_err()
        );
    }

    #[test]
    fn live_updates_remove_changed_members_and_groups_with_only_one_file_left() {
        let fixture = Fixture::new();
        let entries = vec![
            fixture.write("a", b"same"),
            fixture.write("b", b"same"),
            fixture.write("c", b"same"),
        ];
        let mut report = find(&entries, &AtomicU64::new(0), 0).unwrap();
        report.retain_current(&entries[1..]);
        assert_eq!(report.results.len(), 2);
        assert_eq!(report.group_ends, [2]);
        report.retain_current(&entries[2..]);
        assert!(report.results.is_empty());
        assert!(report.group_ends.is_empty());
        assert_eq!(report.skipped, 2);
        assert!(report.warning.is_some());
    }
}
