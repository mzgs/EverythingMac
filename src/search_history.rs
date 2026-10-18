use crate::settings::{read_json, write_json};
use std::{io, path::PathBuf};

const MAX_ENTRIES: usize = 50;

pub(crate) struct SearchHistory {
    pub(crate) entries: Vec<String>,
    pub(crate) error: Option<String>,
    path: Option<PathBuf>,
}

impl SearchHistory {
    pub(crate) fn load(path: Option<PathBuf>) -> Self {
        let mut history = Self {
            entries: Vec::new(),
            error: None,
            path,
        };
        if let Some(path) = &history.path {
            match read_json::<Vec<String>>(path) {
                Ok(entries) if entries.len() <= MAX_ENTRIES => history.entries = entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                result => {
                    let reason = result.err().map_or_else(
                        || "Too many history entries".to_owned(),
                        |error| error.to_string(),
                    );
                    history.error = Some(format!(
                        "Couldn’t load search history: {reason}. Existing file preserved; saving disabled."
                    ));
                    history.path = None;
                }
            }
        }
        history
    }

    pub(crate) fn remember(&mut self, query: &str) {
        if query.trim().is_empty() || self.entries.first().is_some_and(|entry| entry == query) {
            return;
        }
        self.entries.retain(|entry| entry != query);
        self.entries.insert(0, query.to_owned());
        self.entries.truncate(MAX_ENTRIES);
        self.save();
    }

    pub(crate) fn remove(&mut self, index: usize) {
        self.entries.remove(index);
        self.save();
    }

    fn save(&mut self) {
        if let Some(path) = &self.path {
            self.error = write_json(path, &self.entries)
                .err()
                .map(|error| format!("Couldn’t save search history: {error}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn recent_queries_are_unique_bounded_persisted_and_removable() {
        let directory =
            std::env::temp_dir().join(format!("everything-history-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("search-history.json");
        let mut history = SearchHistory::load(Some(path.clone()));
        history.remember("  ");
        assert!(history.entries.is_empty());
        for i in 0..=MAX_ENTRIES {
            history.remember(&format!("query {i}"));
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert!(!history.entries.contains(&"query 0".to_owned()));
        history.remember("query 1");
        history.remember("query 1");
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert_eq!(history.entries[0], "query 1");
        history.remove(0);
        let loaded = SearchHistory::load(Some(path.clone()));
        assert!(loaded.error.is_none());
        assert_eq!(loaded.entries, history.entries);
        assert!(!loaded.entries.contains(&"query 1".to_owned()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&path, "invalid json").unwrap();
        let mut invalid = SearchHistory::load(Some(path.clone()));
        assert!(invalid.error.is_some());
        invalid.remember("new query");
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid json");
        fs::remove_dir_all(directory).unwrap();
    }
}
