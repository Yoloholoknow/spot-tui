use super::*;

/// Shape of `librespot_core::spclient::SpClient::get_lyrics`'s real JSON
/// response body, confirmed against community reverse-engineering
/// documentation of Spotify's internal `/color-lyrics/v2/track/{id}`
/// endpoint (not guessed) -- `startTimeMs` is a *string*, not a number,
/// one of the well-known oddities of Spotify's internal APIs. Only the
/// fields this app actually uses are modeled; `syllables`/`endTimeMs`
/// are real fields in the response but out of scope for this phase (see
/// `spotify_lyrics`'s own doc comment).
#[derive(Debug, Clone, serde::Deserialize)]
struct SpotifyLyricsResponse {
    lyrics: SpotifyLyricsBody,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SpotifyLyricsBody {
    #[serde(rename = "syncType")]
    sync_type: String,
    lines: Vec<SpotifyLyricsLine>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SpotifyLyricsLine {
    #[serde(rename = "startTimeMs")]
    start_time_ms: String,
    words: String,
}

/// Only ever returns a positive result for a real, non-empty line-synced
/// sheet -- everything else (`UNSYNCED`, no lines, an unparseable
/// timestamp on every line) returns `None` so the caller falls through
/// to the existing lrclib path, which already has full, tested handling
/// for plain/instrumental/not-found. This keeps this function narrowly
/// scoped to "give a better *synced* result when Spotify's own catalog
/// actually has one" rather than trying to reproduce lrclib's whole
/// classification surface for a source this app has less experience
/// with.
fn classify_spotify(body: SpotifyLyricsBody) -> Option<CachedLyrics> {
    if body.sync_type != "LINE_SYNCED" {
        return None;
    }
    let lines: Vec<(f64, String)> = body
        .lines
        .iter()
        .filter_map(|l| l.start_time_ms.parse::<f64>().ok().map(|ms| (ms / 1000.0, l.words.clone())))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(CachedLyrics::Synced { lines, credit: None, words: Vec::new() })
}

/// Spotify's own first-party catalog lyrics, via librespot's already-
/// authenticated session -- `librespot_core::spclient::SpClient::get_lyrics`
/// hits the same internal `/color-lyrics/v2/track/{id}` endpoint real
/// Spotify clients use for their own lyrics feature, through the exact
/// client-token machinery this app's Connect session already exercises
/// successfully every run (confirmed by reading `spclient.rs` directly).
/// Broader catalog coverage than lrclib's community-contributed database
/// for anything in Spotify's own mainstream licensing, which is the
/// actual gap this phase exists to close -- but not exhaustive either
/// (bootlegs, unofficial remixes), so this is tried *first*, not as a
/// replacement: `None` means "let lrclib have a try," not "no lyrics
/// exist." A 5-second timeout guards against this undocumented,
/// non-contractual endpoint hanging indefinitely -- the caller is a
/// background fetch, not something worth blocking on.
pub async fn spotify_lyrics(
    session: &librespot_core::session::Session,
    track_id: librespot_core::SpotifyId,
) -> Option<CachedLyrics> {
    // Every early return here used to be a bare `?` -- silent, indistinguishable
    // failure modes (timeout vs. request error vs. bad JSON vs. a real but
    // non-synced result) all landed on the same "fall through to lrclib"
    // outcome with nothing logged. Reported live as a real track showing
    // unsynced when the user's own reference (Spicy Lyrics) shows it synced
    // for the same track -- with zero visibility into which of those cases
    // this actually was, there was nothing to root-cause yet. `log::info!`
    // (this app's filter drops `debug!` from its own code, see the Phase 18
    // note on the same trap) at each branch turns the next live run into
    // real evidence instead of another guess.
    let id = track_id.to_base62().unwrap_or_default();
    let bytes = match tokio::time::timeout(Duration::from_secs(5), session.spclient().get_lyrics(&track_id)).await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => {
            log::info!("spotify_lyrics[{id}]: get_lyrics request failed: {e}");
            return None;
        }
        Err(_) => {
            log::info!("spotify_lyrics[{id}]: get_lyrics timed out after 5s");
            return None;
        }
    };
    let response: SpotifyLyricsResponse = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(e) => {
            log::info!(
                "spotify_lyrics[{id}]: failed to parse response ({e}); body={}",
                String::from_utf8_lossy(&bytes)
            );
            return None;
        }
    };
    let sync_type = response.lyrics.sync_type.clone();
    let line_count = response.lyrics.lines.len();
    let result = classify_spotify(response.lyrics);
    if result.is_none() {
        log::info!("spotify_lyrics[{id}]: no usable synced result (syncType={sync_type}, lines={line_count})");
    } else {
        log::info!("spotify_lyrics[{id}]: got {line_count} synced lines");
    }
    result
}

#[cfg(test)]
mod spotify_lyrics_tests {
    use super::*;

    fn body(sync_type: &str, lines: Vec<(&str, &str)>) -> SpotifyLyricsBody {
        SpotifyLyricsBody {
            sync_type: sync_type.to_string(),
            lines: lines
                .into_iter()
                .map(|(ms, words)| SpotifyLyricsLine { start_time_ms: ms.to_string(), words: words.to_string() })
                .collect(),
        }
    }

    #[test]
    fn line_synced_with_real_lines_converts_ms_to_seconds() {
        let result = classify_spotify(body("LINE_SYNCED", vec![("960", "One, two, three, four")]));
        assert_eq!(result, Some(CachedLyrics::Synced { lines: vec![(0.96, "One, two, three, four".to_string())], credit: None, words: Vec::new() }));
    }

    #[test]
    fn unsynced_falls_through_to_lrclib() {
        assert_eq!(classify_spotify(body("UNSYNCED", vec![("0", "some line")])), None);
    }

    #[test]
    fn line_synced_with_no_lines_falls_through() {
        assert_eq!(classify_spotify(body("LINE_SYNCED", vec![])), None);
    }

    #[test]
    fn a_line_with_an_unparseable_timestamp_is_dropped_not_fatal() {
        let mut b = body("LINE_SYNCED", vec![("960", "good line")]);
        b.lines.push(SpotifyLyricsLine { start_time_ms: "not-a-number".to_string(), words: "bad line".to_string() });
        let result = classify_spotify(b);
        assert_eq!(result, Some(CachedLyrics::Synced { lines: vec![(0.96, "good line".to_string())], credit: None, words: Vec::new() }));
    }

    #[test]
    fn all_timestamps_unparseable_falls_through() {
        let b = body("LINE_SYNCED", vec![("nope", "line")]);
        assert_eq!(classify_spotify(b), None);
    }
}

