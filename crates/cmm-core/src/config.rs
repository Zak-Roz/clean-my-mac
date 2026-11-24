//! User configuration (`data_dir/config.toml`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Tunables. Every field is optional in `config.toml`; missing fields take the defaults below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Project artifacts (node_modules, target, ...) are stale after this many days.
    pub stale_days: u32,
    pub downloads_older_than_days: u32,
    pub installers_older_than_days: u32,
    /// Versioned siblings: keep the newest N.
    pub keep_versions: u32,
    /// Smaller findings are dropped from the report.
    pub min_finding_bytes: u64,
    /// Passive monitoring: the menu bar item turns to its warning state, and a background scan
    /// notifies, when free space is below this many bytes (decimal, like Finder: 50 GB).
    pub low_space_threshold_bytes: u64,
    /// A background scan notifies when one finding grew by more than this many bytes within
    /// `growth_alert_days` (5 GB).
    pub growth_alert_bytes: u64,
    /// Window for `growth_alert_bytes` and for the menu bar's "grew recently" list (7 days).
    pub growth_alert_days: u32,
    /// Background scans (`cmm scan --notify`, run by the LaunchAgent) post macOS notifications.
    pub notifications: bool,
    /// `~/Library/Application Support/clean-my-mac`. Not read from `config.toml`.
    #[serde(skip)]
    pub data_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stale_days: 60,
            downloads_older_than_days: 30,
            installers_older_than_days: 7,
            keep_versions: 1,
            min_finding_bytes: 50 * 1024 * 1024,
            low_space_threshold_bytes: 50_000_000_000,
            growth_alert_bytes: 5_000_000_000,
            growth_alert_days: 7,
            notifications: true,
            data_dir: default_data_dir(),
        }
    }
}

impl Config {
    /// Reads `data_dir/config.toml`; a missing or unreadable/invalid file falls back to defaults.
    pub fn load() -> Self {
        let data_dir = default_data_dir();
        Self::load_from(&data_dir.join("config.toml")).unwrap_or_default()
    }

    /// Strict variant of [`Config::load`]: a missing file yields defaults, an invalid one is an error.
    pub fn load_from(path: &Path) -> Result<Self, CoreError> {
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str::<Config>(&text)
                .map_err(|e| CoreError::Rule(format!("invalid config {}: {e}", path.display())))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        if let Some(dir) = path.parent() {
            config.data_dir = dir.to_path_buf();
        }
        Ok(config)
    }
}

/// The current user's home directory (`$HOME`).
pub fn home_dir() -> PathBuf {
    std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/Library/Application Support/clean-my-mac`
pub fn default_data_dir() -> PathBuf {
    home_dir().join("Library/Application Support/clean-my-mac")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config.stale_days, 60);
        assert_eq!(config.min_finding_bytes, 50 * 1024 * 1024);
        assert_eq!(config.low_space_threshold_bytes, 50_000_000_000);
        assert_eq!(config.growth_alert_bytes, 5_000_000_000);
        assert_eq!(config.growth_alert_days, 7);
        assert!(config.notifications);
        assert_eq!(config.data_dir, dir.path());
    }

    #[test]
    fn partial_file_overrides_some_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "stale_days = 90\nkeep_versions = 2\n").unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.stale_days, 90);
        assert_eq!(config.keep_versions, 2);
        assert_eq!(config.downloads_older_than_days, 30);
    }

    #[test]
    fn monitoring_fields_are_optional_and_overridable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "low_space_threshold_bytes = 80_000_000_000\nnotifications = false\n").unwrap();
        let config = Config::load_from(&path).unwrap();
        assert_eq!(config.low_space_threshold_bytes, 80_000_000_000);
        assert!(!config.notifications);
        assert_eq!(config.growth_alert_bytes, 5_000_000_000);
        assert_eq!(config.growth_alert_days, 7);
    }
}
