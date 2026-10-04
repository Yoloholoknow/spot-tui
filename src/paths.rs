//! Where spot-tui keeps its files.

use directories::ProjectDirs;
use std::path::{Path, PathBuf};

const APP: &str = "spot-tui";

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

/// `config.toml` location, if the platform has a config directory.
pub fn config_file() -> Option<PathBuf> {
    ProjectDirs::from("", "", APP).map(|d| d.config_dir().join("config.toml"))
}

/// macOS convention for app logs, distinct from the cache dir.
pub fn log_file() -> PathBuf {
    home().join("Library/Logs/spot-tui/spot-tui.log")
}

/// librespot's own cache: the stored login credentials, volume and audio cache.
pub fn librespot_cache() -> PathBuf {
    cache_dir().join("librespot")
}

/// Creates `dir` and its parents, then limits `dir` to the owner. The stored
/// logins live under it, and a cache directory is world-readable by default.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Writes `contents` to `path`, readable by the owner only, including when the
/// file already existed with looser permissions.
pub fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_files_and_dirs_are_owner_only_even_over_a_loose_existing_one() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-private-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        let file = dir.join("token.json");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"new").unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(std::fs::read(&file).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
