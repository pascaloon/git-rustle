//! Global appearance preference; writes are performed by the desktop worker.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub light_theme: bool,
}

pub fn settings_file() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(|root| PathBuf::from(root).join("git-rustle/appearance.json"))
        .ok_or_else(|| "Cannot locate the appearance settings directory".into())
}

impl Preferences {
    pub fn load() -> Result<Self, String> {
        Self::load_file(&settings_file()?)
    }

    fn load_file(path: &Path) -> Result<Self, String> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| format!("Cannot read appearance settings: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("Cannot read appearance settings: {error}")),
        }
    }

    pub fn save(self) -> Result<(), String> {
        self.save_file(&settings_file()?)
    }

    fn save_file(self, path: &Path) -> Result<(), String> {
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(path.parent().expect("settings directory"))?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, serde_json::to_vec_pretty(&self)?)?;
            std::fs::rename(temporary, path)
        };
        write().map_err(|error| format!("Cannot save appearance settings: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_persistent_light_dark_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings/appearance.json");
        assert_eq!(
            Preferences::load_file(&path).unwrap(),
            Preferences::default()
        );
        for light_theme in [true, false, true] {
            let preference = Preferences { light_theme };
            preference.save_file(&path).unwrap();
            assert_eq!(Preferences::load_file(&path).unwrap(), preference);
            assert!(!path.with_extension("json.tmp").exists());
        }
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(
            Preferences::load_file(&path).unwrap(),
            Preferences::default()
        );
        std::fs::write(&path, "broken").unwrap();
        assert!(Preferences::load_file(&path).is_err());
    }

    #[test]
    fn reports_unwritable_destination_without_changing_existing_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        Preferences::default().save_file(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(Preferences { light_theme: true }.save_file(&path).is_err());
        assert_eq!(
            Preferences::load_file(&path).unwrap(),
            Preferences::default()
        );
    }
}
