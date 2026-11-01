//! Global appearance preference; writes are performed by the desktop worker.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    #[default]
    Black,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::Black, Self::Dark, Self::Light];

    pub fn label(self) -> &'static str {
        match self {
            Self::Black => "Black",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StoredPreferences")]
pub struct Preferences {
    pub theme: ThemeChoice,
}

// Read the previous two-theme format without changing a user's saved choice.
// An explicit theme takes precedence; future writes use only the new format.
#[derive(Default, Deserialize)]
#[serde(default)]
struct StoredPreferences {
    theme: Option<ThemeChoice>,
    light_theme: bool,
}

impl From<StoredPreferences> for Preferences {
    fn from(stored: StoredPreferences) -> Self {
        Self {
            theme: stored.theme.unwrap_or(if stored.light_theme {
                ThemeChoice::Light
            } else {
                ThemeChoice::Black
            }),
        }
    }
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
    fn defaults_and_all_themes_persist_across_reloads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings/appearance.json");
        assert_eq!(
            Preferences::load_file(&path).unwrap().theme,
            ThemeChoice::Black
        );
        for theme in ThemeChoice::ALL {
            let preference = Preferences { theme };
            preference.save_file(&path).unwrap();
            assert_eq!(Preferences::load_file(&path).unwrap(), preference);
            assert!(!path.with_extension("json.tmp").exists());
            let json = std::fs::read_to_string(&path).unwrap();
            assert!(!json.contains("light_theme"));
        }
        for json in ["{}", r#"{"light_theme":false}"#] {
            std::fs::write(&path, json).unwrap();
            assert_eq!(
                Preferences::load_file(&path).unwrap().theme,
                ThemeChoice::Black
            );
        }
        for json in ["broken", r#"{"theme":"unknown"}"#] {
            std::fs::write(&path, json).unwrap();
            assert!(Preferences::load_file(&path).is_err());
        }
    }

    #[test]
    fn migrates_legacy_light_and_honors_explicit_theme() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        std::fs::write(&path, r#"{"light_theme":true}"#).unwrap();
        let preferences = Preferences::load_file(&path).unwrap();
        assert_eq!(preferences.theme, ThemeChoice::Light);
        preferences.save_file(&path).unwrap();
        assert_eq!(Preferences::load_file(&path).unwrap(), preferences);
        std::fs::write(&path, r#"{"theme":"dark","light_theme":true}"#).unwrap();
        assert_eq!(
            Preferences::load_file(&path).unwrap().theme,
            ThemeChoice::Dark
        );
    }

    #[test]
    fn reports_unwritable_destination_without_changing_existing_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("appearance.json");
        Preferences::default().save_file(&path).unwrap();
        std::fs::create_dir(path.with_extension("json.tmp")).unwrap();
        assert!(
            Preferences {
                theme: ThemeChoice::Dark
            }
            .save_file(&path)
            .is_err()
        );
        assert_eq!(
            Preferences::load_file(&path).unwrap(),
            Preferences::default()
        );
    }
}
