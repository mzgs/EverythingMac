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
