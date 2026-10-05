//! NetEase Cloud Music (music.163.com) as the last synced-lyrics source, tried
//! only after every other source has come up empty. It carries a lot of
//! Chinese-language music the others lack, but its API is unofficial and
//! undocumented, so every failure here (network, shape change, no match) is
//! `None` and never more than a missing source.
//!
//! Two requests per track: a search by "artist title", then the lyrics of the
//! best match. A match must agree on duration and on the artist or the title,
//! so a different song is never shown.

use super::cache::{read_cache, write_cache};
use super::*;
use serde::Deserialize;
use std::path::Path;

const BASE: &str = "https://music.163.com";
const USER_AGENT: &str = "Mozilla/5.0";
/// NetEase and Spotify list the same recording within a second or two of each
/// other; a wider window starts admitting remixes and live versions.
const DURATION_TOLERANCE_SECS: f64 = 2.0;
/// Fewer timed lines than this is a stub or a credits-only sheet.
const MIN_LINES: usize = 2;
/// Credit lines ("作词 : ...") sit in the first seconds, ahead of the lyrics.
const CREDIT_WINDOW_SECS: f64 = 10.0;
const MAX_CREDIT_KEY_CHARS: usize = 10;
const CREDIT: &str = "NetEase Cloud Music";

#[derive(Debug, Deserialize)]
struct SearchResponse {
    result: Option<SearchResult>,
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    songs: Option<Vec<Song>>,
}

#[derive(Debug, Deserialize)]
struct Song {
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    /// Milliseconds.
    #[serde(default)]
    duration: u64,
}

#[derive(Debug, Deserialize)]
struct Artist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct LyricResponse {
    code: Option<i64>,
    #[serde(default)]
    nolyric: bool,
    lrc: Option<LrcBody>,
}

#[derive(Debug, Deserialize)]
struct LrcBody {
    lyric: Option<String>,
}

/// Lowercased letters and digits only, so punctuation, spacing and case never
/// decide a match. CJK characters count as letters.
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Either name containing the other, so "DreamBeach" matches
/// "DreamBeach梦想海滩". An empty name matches nothing.
fn names_overlap(a: &str, b: &str) -> bool {
    let (a, b) = (normalize(a), normalize(b));
    !a.is_empty() && !b.is_empty() && (a.contains(&b) || b.contains(&a))
}

/// The search result that is the same recording: within the duration
/// tolerance, and agreeing on the artist or the title (the title alone is not
/// enough to require, since NetEase writes 自爱 where Spotify has 自愛). The
/// closest duration wins; ties keep NetEase's own ranking.
fn best_match<'a>(
    songs: &'a [Song],
    artist: &str,
    title: &str,
    target_secs: f64,
) -> Option<&'a Song> {
    songs
        .iter()
        .filter(|s| {
            let duration_ok =
                (s.duration as f64 / 1000.0 - target_secs).abs() <= DURATION_TOLERANCE_SECS;
            let artist_ok = s.artists.iter().any(|a| names_overlap(&a.name, artist));
            duration_ok && (artist_ok || names_overlap(&s.name, title))
        })
        .min_by(|a, b| {
            let da = (a.duration as f64 / 1000.0 - target_secs).abs();
            let db = (b.duration as f64 / 1000.0 - target_secs).abs();
            da.total_cmp(&db)
        })
}

/// Whether `text` is a credit line such as "作词 : 邹沛沛" or "编曲：x".
fn is_credit(text: &str) -> bool {
    text.split_once([':', '：']).is_some_and(|(key, _)| {
        !key.trim().is_empty() && key.trim().chars().count() <= MAX_CREDIT_KEY_CHARS
    })
}

/// Drops the credit lines (and blank lines) NetEase puts ahead of the lyrics.
/// Only the opening run is touched, and only inside `CREDIT_WINDOW_SECS`, so a
/// lyric that happens to contain a colon is left alone.
fn strip_leading_credits(lines: Vec<(f64, String)>) -> Vec<(f64, String)> {
    let skip = lines
        .iter()
        .take_while(|(t, text)| *t <= CREDIT_WINDOW_SECS && (text.is_empty() || is_credit(text)))
        .count();
    lines.into_iter().skip(skip).collect()
}

/// Timed lines from a lyrics response, or `None` when there is no usable sheet
/// (an error code, an instrumental, an empty or credits-only body).
fn parse_lyrics(bytes: &[u8]) -> Option<Vec<(f64, String)>> {
    let response: LyricResponse = serde_json::from_slice(bytes).ok()?;
    if response.code.is_some_and(|code| code != 200) || response.nolyric {
        return None;
    }
    let lrc = response.lrc?.lyric?;
    let lines: Vec<(f64, String)> = lrc::parse_lrc(&lrc)
        .into_iter()
        .map(|l| (l.timestamp.as_secs_f64(), l.text))
        .collect();
    let lines = strip_leading_credits(lines);
    let real = lines.iter().filter(|(_, text)| !text.is_empty()).count();
    (real >= MIN_LINES).then_some(lines)
}

fn search_blocking(agent: &ureq::Agent, query: &str) -> Result<Vec<Song>, String> {
    let response = agent
        .post(&format!("{BASE}/api/search/get"))
        .set("Referer", BASE)
        .set("User-Agent", USER_AGENT)
        .send_form(&[("s", query), ("type", "1"), ("limit", "10")])
        .map_err(|e| e.to_string())?;
    let bytes = crate::http::read_limited(response, crate::http::MAX_BODY_BYTES)?;
    let parsed: SearchResponse = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Ok(parsed.result.and_then(|r| r.songs).unwrap_or_default())
}

fn lyrics_blocking(agent: &ureq::Agent, id: u64) -> Result<Vec<u8>, String> {
    let response = agent
        .get(&format!("{BASE}/api/song/lyric"))
        .query("id", &id.to_string())
        .query("lv", "1")
        .query("kv", "-1")
        .query("tv", "-1")
        .set("Referer", BASE)
        .set("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| e.to_string())?;
    crate::http::read_limited(response, crate::http::MAX_BODY_BYTES)
}

/// What the network said about a track.
#[derive(Debug, PartialEq)]
enum Fetched {
    Synced(Vec<(f64, String)>),
    /// NetEase answered and has no usable sheet for the track.
    Miss,
    /// The request itself failed; says nothing about the track.
    Failed,
}

/// Synced lyrics for the track from the network. Every branch logs at `info!`,
/// like the other sources, so a missing sheet can be traced.
fn fetch(artist: &str, title: &str, duration_secs: f64) -> Fetched {
    let agent = crate::http::agent();
    let query = format!("{artist} {title}");
    let songs = match search_blocking(&agent, &query) {
        Ok(songs) => songs,
        Err(e) => {
            log::info!("netease_lyrics: search failed: {e}");
            return Fetched::Failed;
        }
    };
    let Some(song) = best_match(&songs, artist, title, duration_secs) else {
        log::info!(
            "netease_lyrics: no match for {query:?} within {DURATION_TOLERANCE_SECS}s ({} results)",
            songs.len()
        );
        return Fetched::Miss;
    };
    log::info!(
        "netease_lyrics: matched id={} {:?} ({:.1}s)",
        song.id,
        song.name,
        song.duration as f64 / 1000.0
    );
    let bytes = match lyrics_blocking(&agent, song.id) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::info!("netease_lyrics[{}]: lyrics request failed: {e}", song.id);
            return Fetched::Failed;
        }
    };
    match parse_lyrics(&bytes) {
        Some(lines) => {
            log::info!("netease_lyrics[{}]: got {} lines", song.id, lines.len());
            Fetched::Synced(lines)
        }
        None => {
            log::info!("netease_lyrics[{}]: no usable synced sheet", song.id);
            Fetched::Miss
        }
    }
}

/// A synced result for the track: the disk cache first, so a replay never
/// touches the network, then NetEase, storing what it returns. A definite
/// miss is cached for the negative TTL (so a plain-only track doesn't search
/// on every play); a failed request is not, and is retried on the next play.
pub fn lookup(
    cache_dir: &Path,
    track_uri: &str,
    artist: &str,
    title: &str,
    duration_secs: f64,
    now_unix: u64,
) -> Option<CachedLyrics> {
    let key = netease_cache_key(track_uri);
    if let Some(hit) = cached_synced(cache_dir, &key, now_unix) {
        return Some(hit);
    }
    // `cached_synced` ignores a fresh `NotFound`, which here means "asked
    // recently, nothing there".
    if matches!(
        read_cache(cache_dir, &key, now_unix),
        Some(CachedLyrics::NotFound)
    ) {
        return None;
    }
    let lines = match fetch(artist, title, duration_secs) {
        Fetched::Synced(lines) => lines,
        Fetched::Miss => {
            if let Err(e) = write_cache(cache_dir, &key, &CachedLyrics::NotFound, now_unix) {
                log::warn!("netease_lyrics: couldn't cache the miss: {e}");
            }
            return None;
        }
        Fetched::Failed => return None,
    };
    if let Err(e) = store_synced(
        cache_dir,
        &key,
        lines.clone(),
        Vec::new(),
        Some(CREDIT.to_string()),
        now_unix,
    ) {
        log::warn!("netease_lyrics: couldn't cache the result: {e}");
    }
    Some(CachedLyrics::Synced {
        lines,
        words: Vec::new(),
        credit: Some(CREDIT.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_cached_miss_skips_the_network() {
        let dir =
            std::env::temp_dir().join(format!("spot-tui-test-netease-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let uri = "spotify:track:negcache";
        // A search for this gibberish would be a network call; the cached
        // miss must answer first.
        write_cache(
            &dir,
            &netease_cache_key(uri),
            &CachedLyrics::NotFound,
            1_000,
        )
        .unwrap();
        assert_eq!(lookup(&dir, uri, "a", "b", 100.0, 1_000 + 60), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn song(id: u64, name: &str, artists: &[&str], secs: f64) -> Song {
        Song {
            id,
            name: name.to_string(),
            artists: artists
                .iter()
                .map(|a| Artist {
                    name: a.to_string(),
                })
                .collect(),
            duration: (secs * 1000.0) as u64,
        }
    }

    #[test]
    fn matches_the_same_recording_despite_simplified_vs_traditional_title() {
        // The real case: Spotify has 自愛·在 / DreamBeach, NetEase 自爱·在 /
        // DreamBeach梦想海滩, and the durations agree to the millisecond.
        let songs = vec![
            song(1, "自爱·在", &["DreamBeach梦想海滩", "邹沛沛"], 189.699),
            song(2, "꿈결같아서", &["MINNIE"], 249.217),
        ];
        assert_eq!(
            best_match(&songs, "DreamBeach", "自愛·在", 189.699).map(|s| s.id),
            Some(1)
        );
    }

    #[test]
    fn a_wrong_duration_is_never_accepted_even_with_the_right_names() {
        let songs = vec![song(1, "Song", &["Artist"], 300.0)];
        assert!(best_match(&songs, "Artist", "Song", 200.0).is_none());
    }

    #[test]
    fn matching_duration_alone_is_not_enough() {
        let songs = vec![song(1, "Other Tune", &["Someone Else"], 200.0)];
        assert!(best_match(&songs, "Artist", "Song", 200.0).is_none());
    }

    #[test]
    fn the_title_can_stand_in_for_a_differently_spelled_artist() {
        let songs = vec![song(1, "Song Title", &["艺人"], 200.5)];
        assert_eq!(
            best_match(&songs, "Artist", "song title!", 200.0).map(|s| s.id),
            Some(1)
        );
    }

    #[test]
    fn the_closest_duration_wins_and_ties_keep_ranking() {
        let songs = vec![
            song(1, "Song", &["Artist"], 201.5),
            song(2, "Song", &["Artist"], 200.2),
            song(3, "Song", &["Artist"], 200.2),
        ];
        assert_eq!(
            best_match(&songs, "Artist", "Song", 200.0).map(|s| s.id),
            Some(2)
        );
    }

    #[test]
    fn an_empty_name_never_matches() {
        assert!(!names_overlap("", "Artist"));
        assert!(!names_overlap("Artist", "  !! "));
    }

    #[test]
    fn leading_credit_lines_are_dropped_but_lyrics_with_colons_survive() {
        let lines = vec![
            (0.0, "作词 : 邹沛沛".to_string()),
            (1.0, "作曲 : 沈良权".to_string()),
            (3.0, "制作人 : Sirius 孙明旭".to_string()),
            (15.66, "别惊讶".to_string()),
            (20.0, "他说: 别走".to_string()),
        ];
        assert_eq!(
            strip_leading_credits(lines),
            vec![
                (15.66, "别惊讶".to_string()),
                (20.0, "他说: 别走".to_string())
            ]
        );
    }

    #[test]
    fn a_colon_after_the_credit_window_is_lyrics() {
        let lines = vec![(30.0, "Note: this is a lyric".to_string())];
        assert_eq!(strip_leading_credits(lines.clone()), lines);
    }

    #[test]
    fn parses_a_real_shaped_response() {
        let body = r#"{"sgc":false,"code":200,"lrc":{"version":3,"lyric":"[00:00.00] 作词 : 邹沛沛\n[00:01.00] 作曲 : 沈良权\n[00:15.66]别惊讶\n[00:16.80]到底何时能卸下防备\n"},"tlyric":{"lyric":null}}"#;
        assert_eq!(
            parse_lyrics(body.as_bytes()),
            Some(vec![
                (15.66, "别惊讶".to_string()),
                (16.8, "到底何时能卸下防备".to_string())
            ])
        );
    }

    #[test]
    fn instrumentals_errors_and_stubs_are_not_lyrics() {
        let nolyric = r#"{"code":200,"nolyric":true}"#;
        assert_eq!(parse_lyrics(nolyric.as_bytes()), None);
        let error = r#"{"code":404}"#;
        assert_eq!(parse_lyrics(error.as_bytes()), None);
        let credits_only =
            r#"{"code":200,"lrc":{"lyric":"[00:00.00] 作词 : a\n[00:01.00] 作曲 : b\n"}}"#;
        assert_eq!(parse_lyrics(credits_only.as_bytes()), None);
        let one_line = r#"{"code":200,"lrc":{"lyric":"[00:20.00]only line\n"}}"#;
        assert_eq!(parse_lyrics(one_line.as_bytes()), None);
        assert_eq!(parse_lyrics(b"not json"), None);
    }

    #[test]
    fn a_search_with_no_result_block_is_empty_not_an_error() {
        let parsed: SearchResponse = serde_json::from_str(r#"{"code":200}"#).unwrap();
        assert!(
            parsed
                .result
                .and_then(|r| r.songs)
                .unwrap_or_default()
                .is_empty()
        );
    }

    /// Hits the real NetEase API, so it is skipped by default. Run with
    /// `cargo test netease_live -- --ignored` to check the unofficial API still
    /// has the shape this module expects.
    #[test]
    #[ignore = "needs network access to music.163.com"]
    fn netease_live_finds_the_lyrics_spotify_lacks() {
        // 自愛·在 by DreamBeach: no lyrics on Spotify, Spicy, YouTube Music or lrclib.
        let Fetched::Synced(lines) = fetch("DreamBeach", "自愛·在", 189.699) else {
            panic!("a synced sheet");
        };
        assert!(lines.len() > 20, "{} lines", lines.len());
        assert_eq!(lines[0].1, "别惊讶", "credits were stripped");
    }
}
