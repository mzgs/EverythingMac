use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAX_SETTINGS_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Settings {
    pub(crate) roots: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            roots: dirs::home_dir().into_iter().collect(),
        }
    }
}

impl Settings {
    /// A separate data directory keeps development previews away from personal folders.
    pub(crate) fn create_preview(directory: &Path) -> io::Result<PathBuf> {
        fs::create_dir(directory)?;
        let root = directory.join("Sample files");
        fs::create_dir_all(root.join("Projects"))?;
        fs::write(
            root.join("Projects/Readme.md"),
            "# Welcome to Everything Mac\nSearch for files, or try a file-type filter.\n",
        )?;
        fs::write(
            root.join("Meeting notes.txt"),
            "Ideas for the next release\n",
        )?;
        fs::write(
            root.join("Budget.csv"),
            "Category,Amount\nDesign,1200\nDevelopment,2400\n",
        )?;
        fs::write(
            root.join("Landscape.svg"),
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="640" height="360"><rect width="640" height="360" fill="#dbeafe"/><path d="M0 360 220 100 420 360M280 360 470 160 640 360" fill="#60a5fa"/></svg>"##,
        )?;
        let path = directory.join("data/settings.json");
        Self { roots: vec![root] }.save(&path)?;
        Ok(path)
    }

    pub(crate) fn path() -> io::Result<PathBuf> {
        let directory = dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("Could not find the application data directory"))?;
        Ok(directory.join(env!("CARGO_PKG_NAME")).join("settings.json"))
    }

    pub(crate) fn load(path: &Path) -> io::Result<Self> {
        match Self::read(path) {
            Ok(settings) => Ok(settings),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let settings = Self::default();
                settings.save(path)?;
                Ok(settings)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn read(path: &Path) -> io::Result<Self> {
        let settings: Self = read_json(path)?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> io::Result<()> {
        if self.roots.len() > 64 || self.roots.iter().any(|path| !path.is_absolute()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Use at most 64 absolute folder paths",
            ));
        }
        Ok(())
    }

    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        self.validate()?;
        write_json(path, self)
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_SETTINGS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Settings JSON exceeds 1 MiB",
        ));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub(crate) fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    if bytes.len() > MAX_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Settings JSON exceeds 1 MiB",
        ));
    }
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid settings path"))?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_uses_only_its_own_sample_folder() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("everything-preview-test-{}", std::process::id()));
        let path = Settings::create_preview(&directory)?;
        let result = (|| -> io::Result<()> {
            let settings = Settings::read(&path)?;
            assert_eq!(settings.roots, vec![directory.join("Sample files")]);
            assert!(settings.roots[0].join("Projects/Readme.md").is_file());
            let index = crate::index::Index::start(
                settings.roots.clone(),
                Some(path.with_file_name("index.sqlite3")),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let snapshot = index.snapshot();
                if !snapshot.scanning {
                    assert_eq!(
                        snapshot.entries.len(),
                        6,
                        "Sample files must not be excluded as app data"
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "Preview index did not finish"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            index.stop();
            assert!(
                Settings::create_preview(&directory).is_err(),
                "Never overwrite an existing directory"
            );
            assert_eq!(Settings::read(&path)?, settings);
            Ok(())
        })();
        fs::remove_dir_all(directory)?;
        result
    }

    #[test]
    fn settings_persist_defaults_changes_and_preserve_invalid_files() -> io::Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "{}-settings-test-{}",
            env!("CARGO_PKG_NAME"),
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        let path = directory.join("app/settings.json");
        let result = (|| -> io::Result<()> {
            let mut settings = Settings::load(&path)?;
            assert_eq!(settings, Settings::default());
            assert!(path.exists());
            let missing = directory.join("missing.json");
            assert!(Settings::read(&missing).is_err());
            assert!(!missing.exists());
            settings.roots = vec![directory.clone()];
            settings.save(&path)?;
            assert_eq!(Settings::load(&path)?, settings);
            fs::write(&path, "{}")?;
            assert_eq!(Settings::load(&path)?, Settings::default());
            for invalid in [
                "invalid JSON",
                r#"{"roots":["relative"]}"#,
                r#"{"roots":-1}"#,
            ] {
                fs::write(&path, invalid)?;
                assert!(Settings::load(&path).is_err());
                assert_eq!(fs::read_to_string(&path)?, invalid);
            }
            let oversized = directory.join("oversized.json");
            fs::write(&oversized, vec![b' '; MAX_SETTINGS_BYTES + 1])?;
            assert_eq!(
                Settings::read(&oversized).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            let huge = Settings {
                roots: vec![PathBuf::from("x".repeat(MAX_SETTINGS_BYTES))],
            };
            assert!(huge.save(&path).is_err());
            assert_eq!(fs::read_to_string(&path)?, r#"{"roots":-1}"#);
            let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
            fs::write(&temporary, "existing temporary file")?;
            assert!(settings.save(&path).is_err());
            assert_eq!(fs::read_to_string(&path)?, r#"{"roots":-1}"#);
            assert_eq!(fs::read_to_string(&temporary)?, "existing temporary file");
            assert!(
                Settings::path()?
                    .ends_with(Path::new(env!("CARGO_PKG_NAME")).join("settings.json"))
            );
            Ok(())
        })();
        fs::remove_dir_all(directory)?;
        result
    }
}
