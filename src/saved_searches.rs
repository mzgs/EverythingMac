use crate::settings::{read_json, write_json};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum Sort {
    BestMatch,
    Name,
    Path,
    Size,
    Modified,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SavedSearch {
    pub name: String,
    pub query: String,
    pub case: bool,
    pub path: bool,
    pub regex: bool,
    pub content: bool,
    // Retained for saved-search compatibility; typo tolerance is now automatic.
    pub fuzzy: bool,
    pub scope: Option<PathBuf>,
    pub filter: usize,
    pub sort: Sort,
    pub descending: bool,
}

impl SavedSearch {
    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name.chars().count() <= 120
            && !self.name.chars().any(char::is_control)
            && self.query.len() <= 4096
            && self.filter < 7
            && self.scope.as_ref().is_none_or(|path| {
                path.is_absolute()
                    && !path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
            })
    }
}

pub(crate) struct SavedSearches {
    pub entries: Vec<SavedSearch>,
    pub error: Option<String>,
    path: Option<PathBuf>,
    writable: bool,
}

impl SavedSearches {
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut saved = Self {
            entries: Vec::new(),
            error: None,
            path,
            writable: true,
        };
        if let Some(path) = &saved.path {
            match read_json::<Vec<SavedSearch>>(path) {
                Ok(entries)
                    if entries.len() <= 50
                        && entries.iter().enumerate().all(|(i, entry)| {
                            entry.valid()
                                && !entries[..i].iter().any(|other| other.name == entry.name)
                        }) =>
                {
                    saved.entries = entries
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                _ => {
                    saved.error = Some(
                        "Couldn’t load saved searches. Existing file preserved; saving disabled."
                            .into(),
                    );
                    saved.writable = false;
                }
            }
        }
        saved
    }

    pub fn add(&mut self, mut search: SavedSearch) -> bool {
        if !self.writable {
            return false;
        }
        search.name = search.name.trim().to_owned();
        if !search.valid() {
            self.error = Some(
                "Enter a name of 1–120 characters without control characters, and a valid search."
                    .into(),
            );
            return false;
        }
        if self.entries.len() >= 50 || self.entries.iter().any(|entry| entry.name == search.name) {
            self.error = Some("Use a unique name; at most 50 searches can be saved.".into());
            return false;
        }
        self.entries.push(search);
        if !self.save() {
            self.entries.pop();
            return false;
        }
        true
    }

    pub fn remove(&mut self, index: usize) {
        if !self.writable || index >= self.entries.len() {
            return;
        }
        let removed = self.entries.remove(index);
        if !self.save() {
            self.entries.insert(index, removed);
        }
    }

    fn save(&mut self) -> bool {
        self.error = self
            .path
            .as_ref()
            .and_then(|path| write_json(path, &self.entries).err())
            .map(|error| format!("Couldn’t save searches: {error}"));
        self.error.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search() -> SavedSearch {
        SavedSearch {
            name: "Reports".into(),
            query: "ext:pdf modified:today".into(),
            case: true,
            path: false,
            regex: false,
            content: false,
            fuzzy: true,
            scope: Some("/docs".into()),
            filter: 6,
            sort: Sort::BestMatch,
            descending: true,
        }
    }

    #[test]
    fn saves_all_options_validates_and_preserves_invalid_files() {
        let directory =
            std::env::temp_dir().join(format!("everything-saved-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("saved.json");
        let mut saved = SavedSearches::load(Some(path.clone()));
        assert!(saved.add(search()));
        assert!(!saved.add(search()));
        let loaded = SavedSearches::load(Some(path.clone()));
        assert_eq!(loaded.entries, vec![search()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        saved.remove(0);
        assert!(SavedSearches::load(Some(path.clone())).entries.is_empty());
        for (index, mut invalid) in [search(), search(), search(), search(), search()]
            .into_iter()
            .enumerate()
        {
            match index {
                0 => invalid.name.clear(),
                1 => invalid.filter = 7,
                2 => invalid.scope = Some("relative".into()),
                3 => invalid.name = "é".repeat(121),
                _ => invalid.query = "x".repeat(4097),
            }
            assert!(!saved.add(invalid));
        }
        let mut unicode_name = search();
        unicode_name.name = "é".repeat(120);
        assert!(saved.add(unicode_name));
        for contents in [
            "bad json".to_owned(),
            serde_json::to_string(&vec![{
                let mut s = search();
                s.filter = 7;
                s
            }])
            .unwrap(),
        ] {
            std::fs::write(&path, &contents).unwrap();
            let mut invalid = SavedSearches::load(Some(path.clone()));
            assert!(invalid.error.is_some());
            assert!(!invalid.add(search()));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), contents);
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_write_does_not_claim_a_search_was_saved() {
        let mut saved = SavedSearches::load(None);
        saved.path = Some(PathBuf::from("/dev/null/saved.json"));
        assert!(!saved.add(search()));
        assert!(saved.entries.is_empty());
        assert!(saved.error.is_some());
    }
}
