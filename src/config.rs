// `config.toml`, optional: every field has a default, so a missing or partial file
// is fine. Looked up as `spot-tui/config.toml` under the platform config dir
// (macOS `~/Library/Application Support`, Linux `~/.config`), falling back to the
// older `ncspot-lyrics/` directory (see `paths::config_files`). It can hold a
// secret API key, so keep it private (`chmod 600`).

use serde::Deserialize;
use std::fmt;

/// A secret API key. Its `Debug` never prints the value, so a stray
/// `{:?}` of the `Config` (or of anything holding one) can't leak it into a
/// log -- this app's log file is plain text and gets pasted into bug reports.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw key, for the one place that has to put it in a header.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// Env var that overrides `spicy_lyrics_key` from the config file.
pub const SPICY_LYRICS_KEY_ENV: &str = "SPICY_LYRICS_API_KEY";

/// The key to use: the environment wins over the config file, both are
/// trimmed, and a blank value counts as unset -- `SPICY_LYRICS_API_KEY=`
/// (set but empty) is a common shell accident and must not shadow a good
/// key in the file.
pub fn resolve_key(config_value: Option<&ApiKey>, env_value: Option<&str>) -> Option<ApiKey> {
    let usable = |raw: &str| {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| ApiKey::new(trimmed))
    };
    env_value.and_then(usable).or_else(|| config_value.and_then(|k| usable(k.expose())))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `Shift+Q` asks "Quit spot-tui? y/n" first, like every other destructive action;
    /// set `confirm_quit = false` for an immediate quit. Never applies to text typed
    /// into a field, or to Ctrl+C, which always quits at once.
    pub confirm_quit: bool,
    /// Key for Spicy Lyrics' developer API (`sl_sk_...`, secret). Kept outside the
    /// repo and never logged. Optional: without one that source is skipped.
    /// `SPICY_LYRICS_API_KEY` in the environment overrides it.
    pub spicy_lyrics_key: Option<ApiKey>,
    /// Start with lyrics romanized (Japanese, Chinese, Korean shown in Latin
    /// letters). `t` toggles it at any time; this only sets where it starts.
    pub romanize_lyrics: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { confirm_quit: true, spicy_lyrics_key: None, romanize_lyrics: false }
    }
}

impl Config {
    /// The Spicy Lyrics key in effect (environment first, then the file).
    pub fn spicy_lyrics_key(&self) -> Option<ApiKey> {
        resolve_key(self.spicy_lyrics_key.as_ref(), std::env::var(SPICY_LYRICS_KEY_ENV).ok().as_deref())
    }
}

pub fn load() -> Config {
    let Some(raw) = crate::paths::config_files().iter().find_map(|path| std::fs::read_to_string(path).ok()) else {
        return Config::default();
    };
    toml::from_str(&raw).unwrap_or_else(|e| {
        log::warn!("ignoring config.toml, it did not parse: {e}");
        Config::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_toml_uses_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.confirm_quit);
    }

    #[test]
    fn confirm_quit_can_be_disabled() {
        let cfg: Config = toml::from_str("confirm_quit = false").unwrap();
        assert!(!cfg.confirm_quit);
    }

    #[test]
    fn romanized_lyrics_start_off_and_can_start_on() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(!cfg.romanize_lyrics);
        let cfg: Config = toml::from_str("romanize_lyrics = true").unwrap();
        assert!(cfg.romanize_lyrics);
    }

    #[test]
    fn spicy_lyrics_key_defaults_to_none() {
        let cfg: Config = toml::from_str("").unwrap();
        assert!(cfg.spicy_lyrics_key.is_none());
    }

    #[test]
    fn spicy_lyrics_key_is_read_from_toml() {
        let cfg: Config = toml::from_str(r#"spicy_lyrics_key = "sl_sk_from_toml""#).unwrap();
        assert_eq!(cfg.spicy_lyrics_key.unwrap().expose(), "sl_sk_from_toml");
    }

    #[test]
    fn debug_output_never_contains_the_key() {
        let cfg: Config = toml::from_str(r#"spicy_lyrics_key = "sl_sk_super_secret""#).unwrap();
        let printed = format!("{cfg:?} {:?}", cfg.spicy_lyrics_key);
        assert!(!printed.contains("sl_sk_super_secret"), "leaked: {printed}");
        assert!(printed.contains("redacted"));
    }

    #[test]
    fn env_key_beats_the_config_file() {
        let cfg = Some(ApiKey::new("from_config"));
        assert_eq!(resolve_key(cfg.as_ref(), Some("from_env")).unwrap().expose(), "from_env");
    }

    #[test]
    fn config_key_is_used_when_env_is_unset() {
        let cfg = Some(ApiKey::new("from_config"));
        assert_eq!(resolve_key(cfg.as_ref(), None).unwrap().expose(), "from_config");
    }

    #[test]
    fn a_blank_env_var_does_not_shadow_the_config_key() {
        // `SPICY_LYRICS_API_KEY=` (set but empty) is a common shell accident.
        let cfg = Some(ApiKey::new("from_config"));
        assert_eq!(resolve_key(cfg.as_ref(), Some("   ")).unwrap().expose(), "from_config");
    }

    #[test]
    fn keys_are_trimmed() {
        assert_eq!(resolve_key(None, Some("  sl_sk_x \n")).unwrap().expose(), "sl_sk_x");
        let cfg = Some(ApiKey::new("  sl_sk_y "));
        assert_eq!(resolve_key(cfg.as_ref(), None).unwrap().expose(), "sl_sk_y");
    }

    #[test]
    fn no_usable_key_anywhere_is_none() {
        assert!(resolve_key(None, None).is_none());
        assert!(resolve_key(Some(&ApiKey::new("  ")), Some("")).is_none());
    }

    // `context_lines` was removed once Now Playing started showing the
    // whole lyric sheet instead of a windowed few lines around the
    // current one -- a leftover `context_lines = N` in an old config
    // file should be silently ignored, not rejected, matching serde's
    // default (non-`deny_unknown_fields`) behavior.
    #[test]
    fn unknown_fields_are_ignored_not_rejected() {
        let cfg: Config = toml::from_str("context_lines = 4").unwrap();
        assert!(cfg.confirm_quit);
    }
}
