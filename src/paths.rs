//! Where spot-tui keeps its files.

use directories::ProjectDirs;
use std::path::PathBuf;

const APP: &str = "spot-tui";
/// The app's earlier name; its config directory is still read as a fallback.
const LEGACY_CONFIG_APP: &str = "ncspot-lyrics";

pub fn home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .expect("HOME not set")
}

/// Token, pins, lyrics and search caches, plus librespot's own cache.
pub fn cache_dir() -> PathBuf {
    ProjectDirs::from("", "", APP)
        .map(|d| d.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-cache"))
}

/// `config.toml` locations, most preferred first.
pub fn config_files() -> Vec<PathBuf> {
    [APP, LEGACY_CONFIG_APP]
        .iter()
        .filter_map(|app| ProjectDirs::from("", "", app))
        .map(|d| d.config_dir().join("config.toml"))
        .collect()
}

/// macOS convention for app logs, distinct from the cache dir.
pub fn log_file() -> PathBuf {
    home().join("Library/Logs/spot-tui/spot-tui.log")
}

/// ncspot's librespot cache: spot-tui reuses its stored login credentials.
pub fn ncspot_librespot_cache() -> PathBuf {
    home().join(".cache/ncspot/librespot")
}
