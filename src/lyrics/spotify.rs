use super::*;

/// Shape of the JSON from `SpClient::get_lyrics` (Spotify's internal
/// `/color-lyrics/v2/track/{id}`), per community reverse-engineering docs.
/// `startTimeMs` is a string, not a number. Only the fields this app uses are
/// modelled; `syllables` and `endTimeMs` exist but are unused.
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

/// Positive only for a real, non-empty line-synced sheet. Anything else
/// (`UNSYNCED`, no lines, no parseable timestamps) is `None`, so the caller falls
/// through to lrclib, which already handles plain, instrumental and not-found.
fn classify_spotify(body: SpotifyLyricsBody) -> Option<CachedLyrics> {
    if body.sync_type != "LINE_SYNCED" {
        return None;
    }
    let lines: Vec<(f64, String)> = body
        .lines
        .iter()
        .filter_map(|l| {
            l.start_time_ms
                .parse::<f64>()
                .ok()
                .map(|ms| (ms / 1000.0, l.words.clone()))
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(CachedLyrics::Synced {
        lines,
        credit: None,
        words: Vec::new(),
    })
}

/// Spotify's own catalogue lyrics, via librespot's authenticated session (the same
/// internal endpoint Spotify's clients use). Broader than lrclib's community
/// database for mainstream licensed music but not exhaustive (bootlegs, unofficial
/// remixes), so it is tried first and `None` means "let the next source try", not
/// "no lyrics exist". A 5 s timeout guards against this undocumented endpoint
/// hanging; the caller is a background fetch.
pub async fn spotify_lyrics(
    session: &librespot_core::session::Session,
    track_id: librespot_core::SpotifyId,
) -> Option<CachedLyrics> {
    // Each early return logs why at `info!` (the log filter drops `debug!` from this
    // app's own code), so a track that shows unsynced can be traced to timeout, request
    // error, bad JSON or a genuinely non-synced result.
    let id = track_id.to_base62().unwrap_or_default();
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        session.spclient().get_lyrics(&track_id),
    )
    .await
    {
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
        log::info!(
            "spotify_lyrics[{id}]: no usable synced result (syncType={sync_type}, lines={line_count})"
        );
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
                .map(|(ms, words)| SpotifyLyricsLine {
                    start_time_ms: ms.to_string(),
                    words: words.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn line_synced_with_real_lines_converts_ms_to_seconds() {
        let result = classify_spotify(body("LINE_SYNCED", vec![("960", "One, two, three, four")]));
        assert_eq!(
            result,
            Some(CachedLyrics::Synced {
                lines: vec![(0.96, "One, two, three, four".to_string())],
                credit: None,
                words: Vec::new()
            })
        );
    }

    #[test]
    fn unsynced_falls_through_to_lrclib() {
        assert_eq!(
            classify_spotify(body("UNSYNCED", vec![("0", "some line")])),
            None
        );
    }

    #[test]
    fn line_synced_with_no_lines_falls_through() {
        assert_eq!(classify_spotify(body("LINE_SYNCED", vec![])), None);
    }

    #[test]
    fn a_line_with_an_unparseable_timestamp_is_dropped_not_fatal() {
        let mut b = body("LINE_SYNCED", vec![("960", "good line")]);
        b.lines.push(SpotifyLyricsLine {
            start_time_ms: "not-a-number".to_string(),
            words: "bad line".to_string(),
        });
        let result = classify_spotify(b);
        assert_eq!(
            result,
            Some(CachedLyrics::Synced {
                lines: vec![(0.96, "good line".to_string())],
                credit: None,
                words: Vec::new()
            })
        );
    }

    #[test]
    fn all_timestamps_unparseable_falls_through() {
        let b = body("LINE_SYNCED", vec![("nope", "line")]);
        assert_eq!(classify_spotify(b), None);
    }
}
