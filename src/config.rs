//! `~/.config/ncspot-lyrics/config.toml` -- optional; every field has a
//! default so a missing or partial file is fine.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub context_lines: usize,
    /// `q` asks "Quit spot-tui? y/n" first, same overlay every other
    /// destructive action already confirms through, rather than exiting
    /// immediately. Reported live as wanted, with an escape hatch for
    /// anyone who'd rather have the old immediate-quit behavior back --
    /// set `confirm_quit = false` in this file. Never applies to `q`
    /// typed into Search's query box (it isn't a quit key there at all)
    /// or to Ctrl+C, which stays an immediate, unconfirmed quit -- a
    /// harder interrupt than a soft quit key, by terminal convention.
    pub confirm_quit: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { context_lines: 2, confirm_quit: true }
    }
}

pub fn load() -> Config {
    let Some(dirs) = directories::ProjectDirs::from("", "", "ncspot-lyrics") else {
        return Config::default();
    };
    let path = dirs.config_dir().join("config.toml");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Config::default();
    };
    toml::from_str(&raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.context_lines, 2);
        assert!(cfg.confirm_quit);
    }

    #[test]
    fn confirm_quit_can_be_disabled() {
        let cfg: Config = toml::from_str("confirm_quit = false").unwrap();
        assert!(!cfg.confirm_quit);
        assert_eq!(cfg.context_lines, 2); // untouched field keeps its default
    }

    #[test]
    fn partial_toml_overrides_only_given_fields() {
        let cfg: Config = toml::from_str("context_lines = 4").unwrap();
        assert_eq!(cfg.context_lines, 4);
    }
}
