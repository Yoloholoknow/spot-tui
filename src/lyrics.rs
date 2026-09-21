//! lrclib client, LRC parsing, and (Phase 17) Spotify's own first-party
//! lyrics via librespot's session.

use std::time::Duration;

/// One timed piece of a lyric line -- a word, or a syllable of one -- for
/// word-by-word highlighting. `text` carries its own trailing space, so a
/// line's segments concatenate to exactly the line's text (the renderer
/// relies on that: styling per segment must never change what is drawn or
/// how it wraps). Times are seconds from the start of the track.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WordSeg {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    pub timestamp: Duration,
    pub text: String,
    /// Word timing for word-by-word highlighting; empty when this line is
    /// only timed as a whole (every source but a Spicy Lyrics syllable sync).
    pub words: Vec<WordSeg>,
}

/// Parses LRC-format synced lyrics. Metadata tags (`[ar:]`, `[ti:]`,
/// `[length:]`, etc.) are silently skipped -- only tags matching
/// `mm:ss(.frac)?` are treated as timestamps. A line may carry multiple
/// timestamp tags (repeated chorus lines share one text). Output is sorted
/// ascending by timestamp so callers can binary-search it.
pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();

    for raw_line in input.split('\n') {
        let raw_line = raw_line.trim_end_matches('\r');
        let mut rest = raw_line;
        let mut timestamps = Vec::new();

        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            let tag_content = &tag[..end];
            if let Some(ts) = parse_timestamp_tag(tag_content) {
                timestamps.push(ts);
            }
            rest = &tag[end + 1..];
        }

        if timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for ts in timestamps {
            lines.push(LyricLine {
                timestamp: ts,
                text: text.clone(),
                words: Vec::new(),
            });
        }
    }

    lines.sort_by_key(|a| a.timestamp);
    lines
}

/// Parses a `mm:ss(.frac)?` tag body into a Duration. Returns `None` for
/// anything else (metadata tags like `ar:Radiohead` or `length:03:59`).
fn parse_timestamp_tag(tag: &str) -> Option<Duration> {
    let (mm_str, ss_frac_str) = tag.split_once(':')?;
    let minutes: u64 = mm_str.parse().ok()?;
    let seconds: f64 = ss_frac_str.parse().ok()?;
    Some(Duration::from_secs_f64(minutes as f64 * 60.0 + seconds))
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LrcLibEntry {
    #[allow(dead_code)] // kept for schema completeness / debugging
    #[serde(rename = "trackName")]
    pub track_name: String,
    #[allow(dead_code)] // kept for schema completeness / future disambiguation
    #[serde(rename = "artistName")]
    pub artist_name: String,
    pub duration: f64,
    pub instrumental: bool,
    #[serde(rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
}

/// Picks the best `/api/search` candidate: must have synced lyrics, then
/// minimize distance from the known track duration (search results span
/// covers, remixes, etc. with mismatched runtimes).
pub fn best_search_candidate(
    candidates: &[LrcLibEntry],
    target_duration_secs: f64,
) -> Option<&LrcLibEntry> {
    candidates
        .iter()
        .filter(|c| c.synced_lyrics.is_some())
        .min_by(|a, b| {
            let da = (a.duration - target_duration_secs).abs();
            let db = (b.duration - target_duration_secs).abs();
            da.partial_cmp(&db).unwrap()
        })
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum CachedLyrics {
    /// `credit` is the "where these came from" line shown under the lyrics
    /// (Phase 26: Spicy Lyrics asks for its uploaders to be credited). It is
    /// optional in the file so cache entries written before it existed still
    /// load, and omitted when absent so other sources' files are unchanged.
    Synced {
        lines: Vec<(f64, String)>,
        /// Word timing, parallel to `lines` (one entry per line), for
        /// word-by-word highlighting; empty when the sync is line-level
        /// only, which is every source but a Spicy Lyrics syllable sync.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        words: Vec<Vec<WordSeg>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        credit: Option<String>,
    },
    Plain { text: String },
    Instrumental,
    NotFound,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    fetched_at_unix: u64,
    result: CachedLyrics,
}

const NEGATIVE_CACHE_TTL_SECS: u64 = 7 * 24 * 60 * 60;

fn cache_path(cache_dir: &std::path::Path, track_uri: &str) -> std::path::PathBuf {
    use sha1::{Digest, Sha1};
    let hash = format!("{:x}", Sha1::digest(track_uri.as_bytes()));
    cache_dir.join(format!("{hash}.json"))
}

fn read_cache(cache_dir: &std::path::Path, track_uri: &str, now_unix: u64) -> Option<CachedLyrics> {
    let path = cache_path(cache_dir, track_uri);
    let data = std::fs::read_to_string(path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&data).ok()?;
    if matches!(entry.result, CachedLyrics::NotFound)
        && now_unix.saturating_sub(entry.fetched_at_unix) > NEGATIVE_CACHE_TTL_SECS
    {
        return None; // stale negative cache entry, allow refetch
    }
    Some(entry.result)
}

fn write_cache(
    cache_dir: &std::path::Path,
    track_uri: &str,
    result: &CachedLyrics,
    now_unix: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let entry = CacheEntry {
        fetched_at_unix: now_unix,
        result: result.clone(),
    };
    let path = cache_path(cache_dir, track_uri);
    std::fs::write(path, serde_json::to_string(&entry).unwrap())
}

/// A cached *synced* result, or `None` for anything else. The Spicy path
/// only accepts synced hits on purpose: an earlier lrclib `Plain` or
/// negative `NotFound` entry for the same track is exactly the case Spicy
/// exists to improve, so it must not count as "already answered".
pub fn cached_synced(cache_dir: &std::path::Path, track_uri: &str, now_unix: u64) -> Option<CachedLyrics> {
    read_cache(cache_dir, track_uri, now_unix).filter(|cached| matches!(cached, CachedLyrics::Synced { .. }))
}

/// Stores a synced result, overwriting any weaker entry for the track.
pub fn store_synced(
    cache_dir: &std::path::Path,
    track_uri: &str,
    lines: Vec<(f64, String)>,
    words: Vec<Vec<WordSeg>>,
    credit: Option<String>,
    now_unix: u64,
) -> std::io::Result<()> {
    write_cache(cache_dir, track_uri, &CachedLyrics::Synced { lines, words, credit }, now_unix)
}

/// The cache key for a Spicy Lyrics result. Its own namespace, so entries
/// written before word timing existed (under the plain track uri) are never
/// served without words, and a lrclib entry can't be mistaken for one.
pub fn spicy_cache_key(track_uri: &str) -> String {
    format!("spicy:{track_uri}")
}

pub struct LyricsClient {
    agent: ureq::Agent,
    base_url: String,
    cache_dir: std::path::PathBuf,
}

impl LyricsClient {
    pub fn new(cache_dir: std::path::PathBuf) -> Self {
        Self {
            agent: ureq::AgentBuilder::new().build(),
            base_url: "https://lrclib.net".to_string(),
            cache_dir,
        }
    }

    #[cfg(test)]
    fn with_base_url(cache_dir: std::path::PathBuf, base_url: &str) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_millis(500))
                .build(),
            base_url: base_url.to_string(),
            cache_dir,
        }
    }

    pub fn fetch(
        &self,
        track_uri: &str,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_ms: u32,
    ) -> CachedLyrics {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        if let Some(cached) = read_cache(&self.cache_dir, track_uri, now) {
            return cached;
        }

        let result = self.fetch_from_network(artist, title, album, duration_ms);
        let _ = write_cache(&self.cache_dir, track_uri, &result, now);
        result
    }

    fn fetch_from_network(
        &self,
        artist: &str,
        title: &str,
        album: Option<&str>,
        duration_ms: u32,
    ) -> CachedLyrics {
        let duration_secs = duration_ms as f64 / 1000.0;

        let mut req = self
            .agent
            .get(&format!("{}/api/get", self.base_url))
            .query("artist_name", artist)
            .query("track_name", title)
            .query("duration", &duration_secs.to_string())
            .set("User-Agent", "spot-tui/0.1 (personal use)");
        if let Some(album) = album {
            req = req.query("album_name", album);
        }

        if let Ok(resp) = req.call()
            && let Ok(entry) = resp.into_json::<LrcLibEntry>() {
                return classify(&entry);
            }

        // Fall back to fuzzy search when the exact match misses.
        let search = self
            .agent
            .get(&format!("{}/api/search", self.base_url))
            .query("artist_name", artist)
            .query("track_name", title)
            .set("User-Agent", "spot-tui/0.1 (personal use)")
            .call();

        if let Ok(resp) = search
            && let Ok(candidates) = resp.into_json::<Vec<LrcLibEntry>>()
                && let Some(best) = best_search_candidate(&candidates, duration_secs) {
                    return classify(best);
                }

        CachedLyrics::NotFound
    }
}

fn classify(entry: &LrcLibEntry) -> CachedLyrics {
    if entry.instrumental {
        return CachedLyrics::Instrumental;
    }
    if let Some(synced) = &entry.synced_lyrics {
        let lines = parse_lrc(synced)
            .into_iter()
            .map(|l| (l.timestamp.as_secs_f64(), l.text))
            .collect();
        return CachedLyrics::Synced { lines, credit: None, words: Vec::new() };
    }
    if let Some(plain) = &entry.plain_lyrics {
        return CachedLyrics::Plain {
            text: plain.clone(),
        };
    }
    CachedLyrics::NotFound
}

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

/// Index of the currently-active line: the last line whose timestamp has
/// already passed. `lines` must be sorted ascending (guaranteed by
/// `parse_lrc`'s output and by how callers reconstruct it from the cache).
pub fn current_line_index(lines: &[LyricLine], position: Duration) -> Option<usize> {
    match lines.partition_point(|l| l.timestamp <= position) {
        0 => None,
        n => Some(n - 1),
    }
}

#[cfg(test)]
mod current_line_tests {
    use super::*;

    fn lines() -> Vec<LyricLine> {
        vec![
            LyricLine { timestamp: Duration::from_secs(10), text: "a".into(), words: Vec::new() },
            LyricLine { timestamp: Duration::from_secs(20), text: "b".into(), words: Vec::new() },
            LyricLine { timestamp: Duration::from_secs(30), text: "c".into(), words: Vec::new() },
        ]
    }

    #[test]
    fn before_first_line_is_none() {
        assert_eq!(current_line_index(&lines(), Duration::from_secs(5)), None);
    }

    #[test]
    fn exactly_on_a_timestamp_selects_that_line() {
        assert_eq!(current_line_index(&lines(), Duration::from_secs(20)), Some(1));
    }

    #[test]
    fn between_timestamps_selects_the_earlier_line() {
        assert_eq!(current_line_index(&lines(), Duration::from_secs(25)), Some(1));
    }

    #[test]
    fn after_last_line_selects_the_last_line() {
        assert_eq!(current_line_index(&lines(), Duration::from_secs(999)), Some(2));
    }

    #[test]
    fn empty_lines_is_always_none() {
        assert_eq!(current_line_index(&[], Duration::from_secs(5)), None);
    }
}

#[cfg(test)]
mod client_tests {
    use super::*;

    fn entry(track: &str, duration: f64, synced: Option<&str>) -> LrcLibEntry {
        LrcLibEntry {
            track_name: track.to_string(),
            artist_name: "A".to_string(),
            duration,
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: synced.map(|s| s.to_string()),
        }
    }

    #[test]
    fn best_search_candidate_prefers_synced_and_closest_duration() {
        let candidates = vec![
            entry("far, no sync", 500.0, None),
            entry("far but synced", 300.0, Some("[00:01.00] x")),
            entry("closest, synced", 240.0, Some("[00:01.00] y")),
        ];
        let best = best_search_candidate(&candidates, 238.0).unwrap();
        assert_eq!(best.track_name, "closest, synced");
    }

    #[test]
    fn best_search_candidate_returns_none_if_no_synced_available() {
        let candidates = vec![entry("a", 100.0, None), entry("b", 200.0, None)];
        assert!(best_search_candidate(&candidates, 150.0).is_none());
    }

    #[test]
    fn classify_prefers_synced_over_plain() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: Some("plain text".into()),
            synced_lyrics: Some("[00:01.00] synced line".into()),
        };
        match classify(&e) {
            CachedLyrics::Synced { lines, .. } => {
                assert_eq!(lines, vec![(1.0, "synced line".to_string())]);
            }
            other => panic!("expected Synced, got {other:?}"),
        }
    }

    #[test]
    fn classify_falls_back_to_plain_when_no_sync() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: Some("plain text".into()),
            synced_lyrics: None,
        };
        assert_eq!(
            classify(&e),
            CachedLyrics::Plain {
                text: "plain text".to_string()
            }
        );
    }

    #[test]
    fn classify_reports_instrumental_regardless_of_lyrics_fields() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: true,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert_eq!(classify(&e), CachedLyrics::Instrumental);
    }

    #[test]
    fn classify_reports_not_found_when_nothing_present() {
        let e = LrcLibEntry {
            track_name: "t".into(),
            artist_name: "a".into(),
            duration: 10.0,
            instrumental: false,
            plain_lyrics: None,
            synced_lyrics: None,
        };
        assert_eq!(classify(&e), CachedLyrics::NotFound);
    }

    #[test]
    fn cache_read_returns_none_when_file_absent() {
        let dir = std::env::temp_dir().join(format!("ncspot-lyrics-test-{}", std::process::id()));
        assert!(read_cache(&dir, "spotify:track:missing", 1_000_000).is_none());
    }

    #[test]
    fn cache_round_trips_write_then_read() {
        let dir = std::env::temp_dir().join(format!(
            "ncspot-lyrics-test-roundtrip-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let result = CachedLyrics::Synced {
            lines: vec![(1.0, "hi".to_string())],
            words: Vec::new(),
            credit: None,
        };
        write_cache(&dir, "spotify:track:x", &result, 1_000_000).unwrap();
        let read_back = read_cache(&dir, "spotify:track:x", 1_000_010).unwrap();
        assert_eq!(read_back, result);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn negative_cache_expires_after_ttl() {
        let dir = std::env::temp_dir().join(format!("ncspot-lyrics-test-ttl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_cache(&dir, "spotify:track:y", &CachedLyrics::NotFound, 1_000_000).unwrap();

        // Just under 7 days later: still cached as NotFound.
        assert_eq!(
            read_cache(&dir, "spotify:track:y", 1_000_000 + NEGATIVE_CACHE_TTL_SECS - 1),
            Some(CachedLyrics::NotFound)
        );
        // Just over 7 days later: treated as stale, allow refetch.
        assert_eq!(
            read_cache(&dir, "spotify:track:y", 1_000_000 + NEGATIVE_CACHE_TTL_SECS + 1),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Hits the real lrclib.net API. Values verified by hand with curl
    /// before writing this test (see docs/phase0-calibration.md sibling
    /// note in the plan): Radiohead/Creep/Pablo Honey/238s returns synced
    /// lyrics starting "When you were here before".
    #[test]
    fn integration_fetches_real_synced_lyrics_from_lrclib() {
        let dir = std::env::temp_dir().join(format!(
            "ncspot-lyrics-test-integration-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let client = LyricsClient::new(dir.clone());

        let result = client.fetch(
            "spotify:track:test-creep",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );

        match result {
            CachedLyrics::Synced { lines, .. } => {
                assert!(!lines.is_empty());
                assert_eq!(lines[0].1, "When you were here before");
            }
            other => panic!("expected Synced lyrics, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn integration_second_fetch_is_cache_only_no_network() {
        let dir = std::env::temp_dir().join(format!(
            "ncspot-lyrics-test-cachehit-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let live = LyricsClient::new(dir.clone());
        let first = live.fetch(
            "spotify:track:test-creep-2",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );
        assert!(matches!(first, CachedLyrics::Synced { .. }));

        // Bogus, unroutable base URL: if this call reached the network at
        // all it would fail/timeout instead of returning Synced.
        let cached_only = LyricsClient::with_base_url(dir.clone(), "http://127.0.0.1:1");
        let second = cached_only.fetch(
            "spotify:track:test-creep-2",
            "Radiohead",
            "Creep",
            Some("Pablo Honey"),
            238_000,
        );
        assert_eq!(first, second);
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parses_basic_two_line_lrc() {
        let input = "[00:19.16] When you were here before\n[00:24.09] Couldn't look you in the eye";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp, Duration::from_secs_f64(19.16));
        assert_eq!(lines[0].text, "When you were here before");
        assert_eq!(lines[1].timestamp, Duration::from_secs_f64(24.09));
        assert_eq!(lines[1].text, "Couldn't look you in the eye");
    }

    #[test]
    fn skips_metadata_tags_that_arent_timestamps() {
        let input = "[ar:Radiohead]\n[al:Pablo Honey]\n[ti:Creep]\n[length:03:59]\n[00:19.16] When you were here before";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "When you were here before");
    }

    #[test]
    fn multiple_timestamps_share_one_lyric_line() {
        let input = "[00:10.00][00:20.00] repeated chorus line";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp, Duration::from_secs(10));
        assert_eq!(lines[0].text, "repeated chorus line");
        assert_eq!(lines[1].timestamp, Duration::from_secs(20));
        assert_eq!(lines[1].text, "repeated chorus line");
    }

    #[test]
    fn blank_text_after_timestamp_is_kept_as_gap() {
        let input = "[00:10.00]\n[00:15.00] Next line";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[1].text, "Next line");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let input = "[00:19.16] line one\r\n[00:24.09] line two\r\n";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "line one");
        assert_eq!(lines[1].text, "line two");
    }

    #[test]
    fn sorts_output_by_timestamp_even_if_input_is_out_of_order() {
        let input = "[00:30.00] later line\n[00:05.00] earlier line";
        let lines = parse_lrc(input);
        assert_eq!(lines[0].text, "earlier line");
        assert_eq!(lines[1].text, "later line");
    }

    #[test]
    fn accepts_two_or_three_digit_fractional_seconds_as_equal_duration() {
        let two_digit = parse_lrc("[00:19.16] a");
        let three_digit = parse_lrc("[00:19.160] a");
        assert_eq!(two_digit[0].timestamp, three_digit[0].timestamp);
    }
}

#[cfg(test)]
mod spicy_cache_tests {
    use super::*;

    fn fresh_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ncspot-lyrics-test-spicy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const URI: &str = "spotify:track:7knSngLX3gWTH8ch4Y5aGr";

    #[test]
    fn cache_files_written_before_credits_existed_still_load() {
        let old = r#"{"kind":"Synced","lines":[[1.0,"a"]]}"#;
        let parsed: CachedLyrics = serde_json::from_str(old).unwrap();
        assert_eq!(parsed, CachedLyrics::Synced { lines: vec![(1.0, "a".to_string())], credit: None, words: Vec::new() });
    }

    #[test]
    fn a_credit_survives_a_round_trip() {
        let original = CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string())],
            words: Vec::new(),
            credit: Some("Apple Music via Spicy Lyrics".to_string()),
        };
        let back: CachedLyrics = serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn no_credit_is_left_out_of_the_file_entirely() {
        let json = serde_json::to_string(&CachedLyrics::Synced { lines: vec![], credit: None, words: Vec::new() }).unwrap();
        assert!(!json.contains("credit"), "{json}");
    }

    #[test]
    fn only_a_synced_entry_counts_as_a_hit_for_the_spicy_path() {
        // A stale lrclib Plain or NotFound for the same track must not stop
        // Spicy being asked -- that is exactly the track it exists for.
        let dir = fresh_dir("hits");
        for (i, weaker) in [
            CachedLyrics::Plain { text: "x".to_string() },
            CachedLyrics::NotFound,
            CachedLyrics::Instrumental,
        ]
        .iter()
        .enumerate()
        {
            let uri = format!("{URI}{i}");
            write_cache(&dir, &uri, weaker, 100).unwrap();
            assert_eq!(cached_synced(&dir, &uri, 101), None, "{weaker:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_synced_entry_is_a_hit_and_keeps_its_credit() {
        let dir = fresh_dir("synced");
        store_synced(&dir, URI, vec![(2.5, "line".to_string())], Vec::new(), Some("credit".to_string()), 100)
            .unwrap();
        assert_eq!(
            cached_synced(&dir, URI, 101),
            Some(CachedLyrics::Synced {
                lines: vec![(2.5, "line".to_string())],
                words: Vec::new(),
                credit: Some("credit".to_string())
            })
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    #[test]
    fn word_timing_survives_a_round_trip() {
        let original = CachedLyrics::Synced {
            lines: vec![(1.0, "hi there".to_string())],
            words: vec![vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)]],
            credit: None,
        };
        let back: CachedLyrics = serde_json::from_str(&serde_json::to_string(&original).unwrap()).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn no_word_timing_is_left_out_of_the_file_so_other_sources_are_unchanged() {
        let json = serde_json::to_string(&CachedLyrics::Synced {
            lines: vec![(1.0, "a".to_string())],
            words: Vec::new(),
            credit: None,
        })
        .unwrap();
        assert!(!json.contains("words"), "{json}");
    }

    #[test]
    fn cache_files_written_before_word_timing_existed_still_load() {
        let old = r#"{"kind":"Synced","lines":[[1.0,"a"]],"credit":"Apple Music via Spicy Lyrics"}"#;
        let parsed: CachedLyrics = serde_json::from_str(old).unwrap();
        assert_eq!(
            parsed,
            CachedLyrics::Synced {
                lines: vec![(1.0, "a".to_string())],
                words: Vec::new(),
                credit: Some("Apple Music via Spicy Lyrics".to_string()),
            }
        );
    }

    #[test]
    fn spicy_results_live_under_their_own_key() {
        // The Phase 26 entries (line-level only, already on disk) sit under the
        // plain track uri. A different key means they are ignored and simply
        // refreshed -- "missing words" can't be read as "stale", because an
        // Apple Music line sync legitimately has none.
        assert_eq!(spicy_cache_key(URI), format!("spicy:{URI}"));
        let dir = fresh_dir("keys");
        store_synced(&dir, URI, vec![(1.0, "old line-level".to_string())], Vec::new(), None, 100).unwrap();
        assert_eq!(cached_synced(&dir, &spicy_cache_key(URI), 101), None);
        store_synced(&dir, &spicy_cache_key(URI), vec![(1.0, "new".to_string())], Vec::new(), None, 102).unwrap();
        assert!(cached_synced(&dir, &spicy_cache_key(URI), 103).is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_synced_result_upgrades_an_earlier_plain_entry() {
        let dir = fresh_dir("upgrade");
        write_cache(&dir, URI, &CachedLyrics::Plain { text: "unsynced".to_string() }, 100).unwrap();
        assert_eq!(cached_synced(&dir, URI, 101), None);
        store_synced(&dir, URI, vec![(1.0, "now synced".to_string())], Vec::new(), None, 102).unwrap();
        assert!(matches!(cached_synced(&dir, URI, 103), Some(CachedLyrics::Synced { .. })));
        let _ = std::fs::remove_dir_all(dir);
    }
}
