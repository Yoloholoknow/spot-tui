//! Locally-pinned playlists. Spotify's public Web API exposes no pinned
//! state at all (`GET /me/playlists` has no such field) -- pinning is a
//! client-side-only concept in the official app, not something a
//! third-party client can read or write against the real account. This
//! is spot-tui's own substitute: pins live only in a local JSON file and
//! never sync with the official app's pins.

use std::collections::HashSet;

fn pins_path() -> std::path::PathBuf {
    directories::ProjectDirs::from("", "", "spot-tui")
        .map(|d| d.cache_dir().join("pinned_playlists.json"))
        .unwrap_or_else(|| std::env::temp_dir().join("spot-tui-pinned-playlists.json"))
}

pub fn load() -> HashSet<String> {
    let Ok(raw) = std::fs::read_to_string(pins_path()) else {
        return HashSet::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(pinned: &HashSet<String>) {
    let path = pins_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(pinned) {
        let _ = std::fs::write(path, json);
    }
}

/// Pure toggle, no I/O -- callers persist explicitly via `save` afterward.
/// Kept separate so this stays unit-testable without touching the real
/// cache path (same discipline `api::search`'s cache helpers already use).
pub fn toggle_in_place(pinned: &mut HashSet<String>, playlist_uri: &str) {
    if !pinned.remove(playlist_uri) {
        pinned.insert(playlist_uri.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes() {
        let mut pinned = HashSet::new();
        toggle_in_place(&mut pinned, "spotify:playlist:abc");
        assert!(pinned.contains("spotify:playlist:abc"));
        toggle_in_place(&mut pinned, "spotify:playlist:abc");
        assert!(!pinned.contains("spotify:playlist:abc"));
    }
}
