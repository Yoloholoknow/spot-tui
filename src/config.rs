//! `~/.config/ncspot-lyrics/config.toml` -- optional; every field has a
//! default so a missing or partial file is fine.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub context_lines: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self { context_lines: 2 }
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
    }

    #[test]
    fn partial_toml_overrides_only_given_fields() {
        let cfg: Config = toml::from_str("context_lines = 4").unwrap();
        assert_eq!(cfg.context_lines, 4);
    }
}
