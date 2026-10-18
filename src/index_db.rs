use crate::{fs_events::RootIdentity, index::Entry};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

type Entries = BTreeMap<PathBuf, Arc<Entry>>;
type Metadata = (bool, u64, Option<(bool, u64, u32)>);
const APPLICATION_ID: i64 = 0x45564d43;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub roots: Vec<RootIdentity>,
    pub event_ids: Vec<u64>,
    pub verified_at: u64,
    pub errors: Vec<(Vec<u8>, String)>,
}

pub(crate) struct Database {
    connection: Option<Connection>,
    pub(crate) excluded: Option<PathBuf>,
    pub(crate) warning: Option<String>,
    pending: BTreeSet<PathBuf>,
    pub checkpoint: Option<Checkpoint>,
}

impl Database {
    pub(crate) fn open(path: Option<&Path>) -> Self {
        let mut db = Self {
            connection: None,
            excluded: path
                .and_then(Path::parent)
                .map(|parent| parent.canonicalize().unwrap_or_else(|_| parent.to_owned())),
            warning: None,
            pending: BTreeSet::new(),
            checkpoint: None,
        };
        let result = (|| -> Result<Connection, Box<dyn std::error::Error>> {
            let path = path.ok_or("No index database location is available")?;
            let parent = path.parent().ok_or("Invalid index database location")?;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            db.excluded = Some(parent.canonicalize()?);
            // Create privately before SQLite opens it; existing files are never truncated.
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)
            {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let mut connection = Connection::open(path)?;
            connection.busy_timeout(Duration::from_secs(3))?;
            let application_id: i64 =
                connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
            let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if application_id == 0 && version == 0 {
                let tables: i64 =
                    connection
                        .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))?;
                if tables != 0 {
                    return Err("Unrecognized index database; existing file was preserved".into());
                }
                let transaction = connection.transaction()?;
                transaction.execute_batch("CREATE TABLE files (path BLOB PRIMARY KEY NOT NULL, metadata BLOB NOT NULL) WITHOUT ROWID;
                    PRAGMA application_id = 1163283779; PRAGMA user_version = 1;")?;
                transaction.commit()?;
            } else if application_id != APPLICATION_ID || !matches!(version, 1 | 2) {
                return Err(
                    "Unsupported index database version; existing file was preserved".into(),
                );
            }
            if version < 2 {
                let transaction = connection.transaction()?;
                transaction.execute_batch("CREATE TABLE state (key TEXT PRIMARY KEY NOT NULL, value BLOB NOT NULL) WITHOUT ROWID; PRAGMA user_version = 2;")?;
                transaction.commit()?;
            }
            let saved: Option<Vec<u8>> = connection
                .query_row(
                    "SELECT value FROM state WHERE key = 'checkpoint'",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            // Invalid checkpoints require verification; cached file rows remain usable.
            db.checkpoint = saved.and_then(|bytes| serde_json::from_slice(&bytes).ok());
            Ok(connection)
        })();
        match result {
            Ok(connection) => db.connection = Some(connection),
            Err(error) => {
                db.warning = Some(format!(
                    "Index database unavailable: {error}. Search still works, but the index cannot be saved."
                ))
            }
        }
        db
    }

    pub(crate) fn load(&mut self, roots: &[PathBuf], mut preview: impl FnMut(&Entries)) -> Entries {
        let Some(connection) = &self.connection else {
            return Entries::new();
        };
        let result = (|| -> Result<Entries, Box<dyn std::error::Error>> {
            let mut entries = Entries::new();
            let mut statement = connection.prepare("SELECT path, metadata FROM files")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                let path = PathBuf::from(OsString::from_vec(row.get::<_, Vec<u8>>(0)?));
                if !path.is_absolute() {
                    return Err("Invalid cached path".into());
                }
                if !roots.iter().any(|root| path.starts_with(root))
                    || self
                        .excluded
                        .as_ref()
                        .is_some_and(|excluded| path.starts_with(excluded))
                {
                    self.pending.insert(path);
                    continue;
                }
                let (is_dir, size, modified): Metadata =
                    serde_json::from_slice(&row.get::<_, Vec<u8>>(1)?)?;
                let modified = modified
                    .map(|(before_epoch, seconds, nanos)| {
                        if nanos >= 1_000_000_000 {
                            return Err("Invalid cached modification time");
                        }
                        let duration = Duration::new(seconds, nanos);
                        if before_epoch {
                            UNIX_EPOCH.checked_sub(duration)
                        } else {
                            UNIX_EPOCH.checked_add(duration)
                        }
                        .ok_or("Cached modification time is out of range")
                    })
                    .transpose()?;
                entries.insert(
                    path.clone(),
                    Arc::new(Entry {
                        path,
                        is_dir,
                        size,
                        modified,
                    }),
                );
                // Show a small cached list while the rest of a large index loads.
                if entries.len() == 1024 {
                    preview(&entries);
                }
            }
            Ok(entries)
        })();
        match result {
            Ok(entries) => entries,
            Err(error) => {
                self.warning = Some(format!(
                    "Couldn’t load the index database: {error}. Existing data was preserved; changes will not be saved."
                ));
                self.connection = None;
                self.checkpoint = None;
                Entries::new()
            }
        }
    }

    pub(crate) fn save(
        &mut self,
        entries: &Entries,
        changed: &BTreeSet<PathBuf>,
        stopped: &AtomicBool,
    ) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        self.pending.extend(changed.iter().cloned());
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let transaction = connection.transaction()?;
            {
                let mut upsert = transaction.prepare("INSERT INTO files(path, metadata) VALUES (?1, ?2)
                    ON CONFLICT(path) DO UPDATE SET metadata = excluded.metadata WHERE files.metadata != excluded.metadata")?;
                let mut delete = transaction.prepare("DELETE FROM files WHERE path = ?1")?;
                for path in &self.pending {
                    if stopped.load(Ordering::Relaxed) {
                        return Err("Index update interrupted; previous cache kept".into());
                    }
                    if let Some(entry) = entries.get(path) {
                        let modified =
                            entry
                                .modified
                                .map(|time| match time.duration_since(UNIX_EPOCH) {
                                    Ok(duration) => {
                                        (false, duration.as_secs(), duration.subsec_nanos())
                                    }
                                    Err(error) => (
                                        true,
                                        error.duration().as_secs(),
                                        error.duration().subsec_nanos(),
                                    ),
                                });
                        let metadata = serde_json::to_vec(&(entry.is_dir, entry.size, modified))?;
                        upsert.execute(params![path.as_os_str().as_bytes(), metadata])?;
                    } else {
                        delete.execute([path.as_os_str().as_bytes()])?;
                    }
                }
            }
            if stopped.load(Ordering::Relaxed) {
                return Err("Index update interrupted; previous cache kept".into());
            }
            if let Some(checkpoint) = &self.checkpoint {
                transaction.execute("INSERT INTO state(key, value) VALUES ('checkpoint', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", [serde_json::to_vec(checkpoint)?])?;
            } else {
                transaction.execute("DELETE FROM state WHERE key = 'checkpoint'", [])?;
            }
            transaction.commit()?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                self.warning = None;
                self.pending.clear();
            }
            Err(error) => {
                self.warning = Some(format!(
                    "Couldn’t save the index database: {error}. Cached results may be outdated on next launch."
                ))
            }
        }
    }
}
