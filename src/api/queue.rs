//! The Connect queue (Phase 7): what's currently playing plus what's up
//! next. Read-only -- this app doesn't call `add_item_to_queue` yet
//! (adding a *new* track to the queue from Search/Liked Songs/etc is a
//! real, viable feature, deferred as future scope rather than half-wired
//! with no keybind reaching it). There is no remove or reorder endpoint
//! anywhere in rspotify's `OAuthClient` for the queue at all (confirmed
//! by reading its full method list), matching the official app's own
//! inability to manually reorder or pluck a single item back out of the
//! queue once it's there -- that part is a real platform gap, not
//! something deferred by choice, the same category as Liked Songs having
//! no reorder capability at all.

use rspotify::clients::OAuthClient;
use rspotify::model::PlayableItem;
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::ensure_fresh;
use super::search::TrackResult;

#[derive(Debug, Clone, PartialEq)]
pub struct QueueSummary {
    pub currently_playing: Option<TrackResult>,
    pub queue: Vec<TrackResult>,
}

/// Episodes are silently skipped, same as everywhere else in this
/// codebase -- podcasts are an explicit non-goal (see the design-scope
/// plan), and `TrackResult` has no shape for one anyway.
fn playable_to_track(item: PlayableItem) -> Option<TrackResult> {
    match item {
        PlayableItem::Track(t) => {
            let first_artist = t.artists.first();
            Some(TrackResult {
                uri: t.id.map(|id| id.uri()).unwrap_or_default(),
                title: t.name,
                artist: first_artist.map(|a| a.name.clone()).unwrap_or_default(),
                artist_uri: first_artist.and_then(|a| a.id.clone()).map(|id| id.uri()).unwrap_or_default(),
                album_uri: t.album.id.clone().map(|id| id.uri()).unwrap_or_default(),
                album: t.album.name,
            })
        }
        PlayableItem::Episode(_) => None,
        // Confirmed live root cause of "queued items don't show up": rspotify's
        // `FullTrack` (used by `#[serde(untagged)]`'s Track arm) requires
        // `external_ids`, a field the queue endpoint's track objects simply don't
        // include -- every other track-returning endpoint does, so this is a real
        // inconsistency in Spotify's own API surface, not a bug in this app or in
        // how rspotify models a track in general. The untagged enum's Track/Episode
        // arms both fail (Episode for the obvious shape reason) and it falls back
        // to this raw-JSON catch-all (see ramsayleung/rspotify#525). Every field
        // `TrackResult` actually needs is present in that raw JSON regardless --
        // extracted leniently here instead of discarding a perfectly good track.
        PlayableItem::Unknown(raw) => match lenient_track_from_raw(&raw) {
            Some(track) => Some(track),
            None => {
                log::warn!("queue item did not parse as Track, Episode, or a lenient fallback, dropped: {raw}");
                None
            }
        },
    }
}

fn lenient_track_from_raw(raw: &serde_json::Value) -> Option<TrackResult> {
    if raw.get("type").and_then(|v| v.as_str()) != Some("track") {
        return None; // not track-shaped at all -- a real unknown, not this issue
    }
    let uri = raw.get("uri").and_then(|v| v.as_str())?.to_string();
    let title = raw.get("name").and_then(|v| v.as_str())?.to_string();
    let artist = raw
        .get("artists")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let album = raw
        .get("album")
        .and_then(|v| v.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let artist_uri = raw
        .get("artists")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|a| a.get("uri"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let album_uri = raw
        .get("album")
        .and_then(|v| v.get("uri"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    Some(TrackResult { uri, title, artist, album, artist_uri, album_uri })
}

#[cfg(test)]
mod lenient_parse_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_a_track_missing_external_ids() {
        // Real shape confirmed live from /v1/me/player/queue -- no
        // external_ids field, which is exactly what makes FullTrack's
        // strict deserialization fail and land here in the first place.
        let raw = json!({
            "album": {"name": "Some Album", "uri": "spotify:album:xyz"},
            "artists": [{"name": "Some Artist", "uri": "spotify:artist:xyz"}],
            "disc_number": 1,
            "duration_ms": 200000,
            "explicit": false,
            "external_urls": {"spotify": "https://open.spotify.com/track/abc"},
            "href": "https://api.spotify.com/v1/tracks/abc",
            "id": "abc",
            "is_local": false,
            "name": "Some Track",
            "preview_url": null,
            "track_number": 1,
            "type": "track",
            "uri": "spotify:track:abc"
        });
        let track = lenient_track_from_raw(&raw).expect("should extract a track");
        assert_eq!(track.uri, "spotify:track:abc");
        assert_eq!(track.title, "Some Track");
        assert_eq!(track.artist, "Some Artist");
        assert_eq!(track.album, "Some Album");
        assert_eq!(track.artist_uri, "spotify:artist:xyz");
        assert_eq!(track.album_uri, "spotify:album:xyz");
    }

    #[test]
    fn non_track_shaped_json_returns_none() {
        let raw = json!({"type": "episode", "name": "Some Podcast Episode"});
        assert_eq!(lenient_track_from_raw(&raw), None);
    }

    #[test]
    fn missing_required_field_returns_none_rather_than_a_half_built_track() {
        let raw = json!({"type": "track", "name": "No URI Here"});
        assert_eq!(lenient_track_from_raw(&raw), None);
    }
}

pub async fn current_queue(client: &AuthCodeSpotify) -> Result<QueueSummary, String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before current_queue failed, trying with existing token anyway: {e}");
    }
    let raw = client.current_user_queue().await.map_err(|e| e.to_string())?;
    Ok(QueueSummary {
        currently_playing: raw.currently_playing.and_then(playable_to_track),
        queue: raw.queue.into_iter().filter_map(playable_to_track).collect(),
    })
}
