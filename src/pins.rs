//! Locally-pinned playlists and tracks. Spotify's public Web API exposes
//! no pinned state at all (`GET /me/playlists` has no such field) --
//! pinning is a client-side-only concept in the official app, not
//! something a third-party client can read or write against the real
//! account. This is spot-tui's own substitute: pins live only in local
//! JSON files and never sync with the official app's pins.
//!
//! Playlists and tracks get separate files (`kind` picks which) rather
//! than one shared set -- a playlist and a track could theoretically
//! share a URI namespace collision in principle, and keeping them
//! separate means never having to reason about that.

use std::collections::HashSet;

fn pins_path(kind: &str) -> std::path::PathBuf {
    crate::paths::cache_dir().join(format!("pinned_{kind}.json"))
}

pub fn load(kind: &str) -> HashSet<String> {
    let Ok(raw) = std::fs::read_to_string(pins_path(kind)) else {
        return HashSet::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(kind: &str, pinned: &HashSet<String>) {
    let path = pins_path(kind);
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
