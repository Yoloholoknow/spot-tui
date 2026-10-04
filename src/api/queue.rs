// The Connect queue: what is playing and what is up next, plus `add_to_queue`.
// rspotify has no remove or reorder call for the queue; the official app cannot
// pluck an item back out either, so this is a platform gap.

use rspotify::clients::OAuthClient;
use rspotify::model::{PlayableId, PlayableItem, TrackId};
use rspotify::prelude::Id;
use rspotify::AuthCodeSpotify;

use super::{describe_client_error, ensure_fresh};
use super::search::TrackResult;

#[derive(Debug, Clone, PartialEq)]
pub struct QueueSummary {
    pub currently_playing: Option<TrackResult>,
    pub queue: Vec<TrackResult>,
}

/// Episodes are skipped, as everywhere else: podcasts are out of scope and
/// `TrackResult` has no shape for one.
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
        // The queue endpoint's track objects lack `external_ids`, which rspotify's
        // `FullTrack` requires (every other endpoint includes it). The untagged enum's
        // Track and Episode arms both fail and it falls back to the raw-JSON catch-all
        // (ramsayleung/rspotify#525). Every field `TrackResult` needs is present in that
        // JSON, so it is extracted leniently instead of discarding the track.
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

/// Appends to the playback queue (played next, before the context resumes), on the
/// active device (`device_id: None`), normally this app since it reclaims active
/// state on launch. The likeliest failure is no active device or a non-Premium
/// account, reported as a bare 404/403; `describe_client_error` surfaces the
/// reason from the body.
pub async fn add_to_queue(client: &AuthCodeSpotify, track_uri: &str) -> Result<(), String> {
    if let Err(e) = ensure_fresh(client).await {
        log::warn!("token refresh before add_to_queue failed, trying with existing token anyway: {e}");
    }
    let track = TrackId::from_id_or_uri(track_uri).map_err(|e| e.to_string())?;
    match client.add_item_to_queue(PlayableId::Track(track), None).await {
        Ok(()) => Ok(()),
        Err(e) => Err(describe_client_error(e).await),
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

#[cfg(test)]
mod lenient_parse_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_a_track_missing_external_ids() {
        // The real shape from /v1/me/player/queue: no external_ids, which is
        // what makes FullTrack's strict deserialization fail and land here.
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
