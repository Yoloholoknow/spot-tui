//! YouTube Music as a fourth lyrics source (Phase 22). Draws from
//! LyricFind's catalog -- a different licensing backer than Spotify's own
//! (historically Musixmatch), so it has genuine potential to cover tracks
//! neither Spotify-direct nor lrclib have, not just duplicate them.
//!
//! This is YouTube's own internal "innertube" API -- the literal JSON its
//! web player renders from, not a small documented REST surface. The
//! exact response shapes below are transcribed from reading `ytmusicapi`
//! (the reference Python library) source directly, not guessed, but this
//! session has no outbound network path to music.youtube.com itself to
//! confirm them against a live call -- expect at least one round of
//! real-log-driven fixes once this is actually run, the same posture
//! this codebase already took with Spotify-direct and Spicy Lyrics.
//! Every navigation step below is defensive (`Option`-returning, never a
//! panic) so an unexpected shape degrades to a clean fallthrough to the
//! next lyrics source, never a crash or a hang.

use crate::lyrics::CachedLyrics;
use serde_json::Value;
use std::time::Duration;

const YT_BASE: &str = "https://music.youtube.com/youtubei/v1";
// A plausible, stable WEB_REMIX client version -- ytmusicapi itself
// hardcodes a fixed string here rather than tracking YouTube's real
// release calendar; this endpoint has not been observed to reject a
// slightly-stale version string.
const YT_CONTEXT_WEB_VERSION: &str = "1.20250101.01.00";
const YT_CONTEXT_MOBILE_VERSION: &str = "7.21.50";
// The "Songs" search filter param, reverse-engineered and hardcoded by
// `ytmusicapi` itself (its own `filtered_param1 + params("songs") +
// param3` constants) -- opaque but stable.
const YT_SEARCH_SONGS_PARAM: &str = "EgWKAQIIAWoMEA4QChADEAQQCRAF";
// How far a candidate's duration may drift from the track's own known
// duration and still be considered the same song -- guards against a
// same-titled cover/remix/different-artist match. A judgment call, not a
// value confirmed against real search results; revisit if live testing
// shows false positives (too loose) or the intended track never matching
// (too strict).
const DURATION_TOLERANCE_SECS: f64 = 5.0;
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

enum NavStep<'a> {
    Key(&'a str),
    Idx(usize),
}

/// A small, defensive get-by-key-or-index chain over `serde_json::Value`,
/// mirroring `ytmusicapi`'s own `nav(..., is_optional=True)` -- any single
/// missing key or out-of-range index returns `None` instead of panicking,
/// so a real shape drift in YouTube's own internal JSON degrades to a
/// clean fallthrough rather than crashing this app.
fn nav<'a>(value: &'a Value, path: &[NavStep]) -> Option<&'a Value> {
    let mut cur = value;
    for step in path {
        cur = match step {
            NavStep::Key(k) => cur.get(*k)?,
            NavStep::Idx(i) => cur.get(*i)?,
        };
    }
    Some(cur)
}

/// Parses a `mm:ss` or `h:mm:ss` duration string (how YouTube Music's
/// search results render a track's length as plain text) into seconds.
/// `None` for anything that isn't cleanly 2-3 all-digit colon-separated
/// parts, rather than guessing.
fn parse_mmss(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() < 2 || parts.len() > 3 {
        return None;
    }
    if parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let nums: Vec<u64> = parts.iter().map(|p| p.parse().unwrap()).collect();
    Some(match nums.len() {
        2 => (nums[0] * 60 + nums[1]) as f64,
        3 => (nums[0] * 3600 + nums[1] * 60 + nums[2]) as f64,
        _ => unreachable!(),
    })
}

#[derive(Debug, Clone, PartialEq)]
struct YtSongCandidate {
    video_id: String,
    duration_secs: f64,
}

/// Picks the *first* candidate (in YouTube's own search-ranked order)
/// whose duration is within `tolerance_secs` of `target_duration_secs` --
/// not the globally closest-by-duration candidate across the whole
/// result set. Real bug found live (a track's lyrics came back for a
/// completely different song, "neon skies"): picking by duration
/// proximity alone treats every candidate as equally likely to be the
/// right song and discards YouTube's own relevance ranking (title/artist/
/// channel match against the text query) entirely -- a same-titled or
/// even unrelated video whose runtime happens to land a hair closer to
/// the target can outrank the actual correct, top-ranked hit. Duration
/// stays as a real filter (rejects a cover/remix/wrong version even if it
/// search-ranks first), just no longer the primary sort key -- the first
/// tolerance-passing result in ranked order wins. Only `video_id` and
/// duration are extracted from search results at all: title/artist text
/// would need YouTube's own heuristic "flex column run" classification
/// (real, genuine complexity flagged in this phase's design), but
/// matching by duration alone against a query already built from the
/// real artist+title doesn't need it.
/// Returns the winning candidate's rank alongside it -- purely for
/// diagnostics (logging exactly which position in YouTube's own ranked
/// results actually won), not used to change the decision itself.
fn best_song_candidate(
    candidates: &[YtSongCandidate],
    target_duration_secs: f64,
    tolerance_secs: f64,
) -> Option<(usize, &YtSongCandidate)> {
    candidates.iter().enumerate().find(|(_, c)| (c.duration_secs - target_duration_secs).abs() <= tolerance_secs)
}

const SEARCH_SECTIONS: &[NavStep] = &[
    NavStep::Key("contents"),
    NavStep::Key("tabbedSearchResultsRenderer"),
    NavStep::Key("tabs"),
    NavStep::Idx(0),
    NavStep::Key("tabRenderer"),
    NavStep::Key("content"),
    NavStep::Key("sectionListRenderer"),
    NavStep::Key("contents"),
];

const VIDEO_ID_PATH: &[NavStep] = &[
    NavStep::Key("overlay"),
    NavStep::Key("musicItemThumbnailOverlayRenderer"),
    NavStep::Key("content"),
    NavStep::Key("musicPlayButtonRenderer"),
    NavStep::Key("playNavigationEndpoint"),
    NavStep::Key("watchEndpoint"),
    NavStep::Key("videoId"),
];

/// A row's duration is buried among several "flex column" text runs
/// (title, artist, album, view count, duration -- YouTube's own web
/// player renders these as one bullet-separated line, "Song • Artist •
/// Album • 3:45"). Rather than classifying which run is which (the real
/// heuristic complexity `ytmusicapi` itself has to do), this just scans
/// every run for the one that parses as `mm:ss` -- the only field this
/// app actually needs from a row.
fn parse_song_row(mrlir: &Value) -> Option<YtSongCandidate> {
    let video_id = nav(mrlir, VIDEO_ID_PATH)?.as_str()?.to_string();
    let flex_columns = mrlir.get("flexColumns")?.as_array()?;
    let mut duration_secs = None;
    for col in flex_columns {
        let Some(runs) = nav(
            col,
            &[NavStep::Key("musicResponsiveListItemFlexColumnRenderer"), NavStep::Key("text"), NavStep::Key("runs")],
        )
        .and_then(Value::as_array) else {
            continue;
        };
        for run in runs {
            if let Some(text) = run.get("text").and_then(Value::as_str)
                && let Some(secs) = parse_mmss(text) {
                    duration_secs = Some(secs);
                }
        }
    }
    Some(YtSongCandidate { video_id, duration_secs: duration_secs? })
}

fn parse_search_results(response: &Value) -> Vec<YtSongCandidate> {
    let Some(sections) = nav(response, SEARCH_SECTIONS).and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for section in sections {
        let Some(items) =
            section.get("musicShelfRenderer").and_then(|s| s.get("contents")).and_then(Value::as_array)
        else {
            continue;
        };
        for item in items {
            if let Some(mrlir) = item.get("musicResponsiveListItemRenderer")
                && let Some(candidate) = parse_song_row(mrlir) {
                    out.push(candidate);
                }
        }
    }
    out
}

const WATCH_NEXT_TABS: &[NavStep] = &[
    NavStep::Key("contents"),
    NavStep::Key("singleColumnMusicWatchNextResultsRenderer"),
    NavStep::Key("tabbedRenderer"),
    NavStep::Key("watchNextTabbedResultsRenderer"),
    NavStep::Key("tabs"),
];

const TAB_PAGE_TYPE: &[NavStep] = &[
    NavStep::Key("tabRenderer"),
    NavStep::Key("endpoint"),
    NavStep::Key("browseEndpoint"),
    NavStep::Key("browseEndpointContextSupportedConfigs"),
    NavStep::Key("browseEndpointContextMusicConfig"),
    NavStep::Key("pageType"),
];

const TAB_BROWSE_ID: &[NavStep] =
    &[NavStep::Key("tabRenderer"), NavStep::Key("endpoint"), NavStep::Key("browseEndpoint"), NavStep::Key("browseId")];

/// Finds the watch-next tab tagged `MUSIC_PAGE_TYPE_TRACK_LYRICS` and
/// returns its `browseId` -- the id the timed-lyrics `/browse` call
/// (Step 3) needs.
fn parse_lyrics_browse_id(response: &Value) -> Option<String> {
    let tabs = nav(response, WATCH_NEXT_TABS).and_then(Value::as_array)?;
    for tab in tabs {
        if nav(tab, TAB_PAGE_TYPE).and_then(Value::as_str) == Some("MUSIC_PAGE_TYPE_TRACK_LYRICS") {
            return nav(tab, TAB_BROWSE_ID).and_then(Value::as_str).map(String::from);
        }
    }
    None
}

/// Depth-first search for the first array field literally named
/// `timedLyricsData` anywhere in the response. The exact nesting under
/// YouTube Music's timed-lyrics wrapper (`elementRenderer`/
/// `timedLyricsModel`, per community documentation) isn't independently
/// confirmed against a live response -- searching by field name rather
/// than assuming one exact path hedges against getting an intermediate
/// key wrong, while still degrading to `None` (fallthrough) rather than
/// guessing at a shape this session can't verify.
fn find_timed_lyrics_array(value: &Value) -> Option<&Vec<Value>> {
    if let Value::Object(map) = value {
        if let Some(Value::Array(arr)) = map.get("timedLyricsData") {
            return Some(arr);
        }
        for v in map.values() {
            if let Some(found) = find_timed_lyrics_array(v) {
                return Some(found);
            }
        }
    } else if let Value::Array(arr) = value {
        for v in arr {
            if let Some(found) = find_timed_lyrics_array(v) {
                return Some(found);
            }
        }
    }
    None
}

/// A timed-lyrics entry's exact field names are, likewise, not confirmed
/// against a live response -- tries several plausible variants for both
/// the text and the start-time fields (a string or a number either way,
/// Spotify's own internal API already showed this pattern of numbers
/// serialized as strings) rather than committing to one guess.
fn parse_timed_line(entry: &Value) -> Option<(f64, String)> {
    let text = entry
        .get("lyricLine")
        .or_else(|| entry.get("text"))
        .or_else(|| entry.get("words"))
        .and_then(Value::as_str)?
        .to_string();
    let start_field = entry
        .get("cueRange")
        .and_then(|cr| cr.get("startTimeMilliseconds"))
        .or_else(|| entry.get("startTimeMilliseconds"))
        .or_else(|| entry.get("startTimeMs"))
        .or_else(|| entry.get("start_time"))?;
    let start_ms = start_field.as_f64().or_else(|| start_field.as_str().and_then(|s| s.parse::<f64>().ok()))?;
    Some((start_ms / 1000.0, text))
}

fn find_timed_lines(value: &Value) -> Option<Vec<(f64, String)>> {
    let arr = find_timed_lyrics_array(value)?;
    let lines: Vec<(f64, String)> = arr.iter().filter_map(parse_timed_line).collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

fn yt_agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build()
}

fn yt_post(agent: &ureq::Agent, endpoint: &str, body: Value) -> Result<Value, String> {
    let url = format!("{YT_BASE}/{endpoint}?alt=json");
    let resp = agent.post(&url).set("Content-Type", "application/json").send_json(body).map_err(|e| e.to_string())?;
    resp.into_json::<Value>().map_err(|e| e.to_string())
}

fn search_song_blocking(query: &str) -> Result<Vec<YtSongCandidate>, String> {
    let agent = yt_agent();
    let body = serde_json::json!({
        "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": YT_CONTEXT_WEB_VERSION}, "user": {}},
        "query": query,
        "params": YT_SEARCH_SONGS_PARAM,
    });
    let response = yt_post(&agent, "search", body)?;
    Ok(parse_search_results(&response))
}

fn lyrics_browse_id_blocking(video_id: &str) -> Result<Option<String>, String> {
    let agent = yt_agent();
    let body = serde_json::json!({
        "context": {"client": {"clientName": "WEB_REMIX", "clientVersion": YT_CONTEXT_WEB_VERSION}, "user": {}},
        "enablePersistentPlaylistPanel": true,
        "isAudioOnly": true,
        "tunerSettingValue": "AUTOMIX_SETTING_NORMAL",
        "videoId": video_id,
        "watchEndpointMusicSupportedConfigs": {
            "watchEndpointMusicConfig": {"hasPersistentPlaylistPanel": true, "musicVideoType": "MUSIC_VIDEO_TYPE_ATV"}
        },
    });
    let response = yt_post(&agent, "next", body)?;
    Ok(parse_lyrics_browse_id(&response))
}

/// Uses the `ANDROID_MUSIC` client context, not `WEB_REMIX` -- confirmed
/// via `ytmusicapi`'s own `as_mobile()` doc comment as required
/// specifically for timestamped (not plain) lyrics.
fn timed_lyrics_blocking(browse_id: &str) -> Result<Option<Vec<(f64, String)>>, String> {
    let agent = yt_agent();
    let body = serde_json::json!({
        "context": {"client": {"clientName": "ANDROID_MUSIC", "clientVersion": YT_CONTEXT_MOBILE_VERSION}, "user": {}},
        "browseId": browse_id,
    });
    let response = yt_post(&agent, "browse", body)?;
    let lines = find_timed_lines(&response);
    // Real gap found live: this exact miss fires with zero visibility into
    // *why* -- a wrong field-name guess and "this browseId genuinely has
    // no timed lyrics" both look identical from the caller's side. Log the
    // response's real top-level shape (truncated -- these bodies can be
    // large) so the next miss is fixable from the log instead of another
    // guess at field names that may already be correct.
    if lines.is_none() {
        let dump = serde_json::to_string(&response).unwrap_or_default();
        let truncated = if dump.len() > 2000 { &dump[..2000] } else { &dump[..] };
        log::info!("timed_lyrics_blocking[{browse_id}]: no timedLyricsData found; response (truncated)={truncated}");
    }
    Ok(lines)
}

/// YouTube Music as a lyrics source -- tried after Spotify-direct fails,
/// before lrclib (see `main.rs`'s pending-fetch chain): a real
/// timed-lyrics source like Spotify, drawing from a different catalog
/// (LyricFind), not a last-resort community tier the way lrclib is.
/// `None` at any step means "let the next source have a try," never a
/// hard error -- every branch logs via `log::info!` (this app's
/// env_logger filter drops `debug!` from its own code, confirmed the
/// hard way twice already this session) so the next real run gives exact
/// evidence to fix against rather than another guess.
pub async fn ytmusic_lyrics(artist: &str, title: &str, duration_secs: f64) -> Option<CachedLyrics> {
    let query = format!("{artist} {title}");
    log::info!("ytmusic_lyrics: searching {query:?} (target duration {duration_secs:.1}s)");
    let query_for_call = query.clone();
    let candidates = match tokio::task::spawn_blocking(move || search_song_blocking(&query_for_call)).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            log::info!("ytmusic_lyrics: search failed: {e}");
            return None;
        }
        Err(e) => {
            log::info!("ytmusic_lyrics: search task panicked: {e}");
            return None;
        }
    };
    // Full candidate dump, in the real rank order YouTube returned them --
    // this is exactly the evidence a "wrong song matched" report needs:
    // whether the right video was even in the result set at all, and if
    // so, at what rank (a real bug found live picked a same-titled wrong
    // song purely on duration proximity; this log line is what would have
    // shown that immediately instead of needing a second live round).
    for (i, c) in candidates.iter().enumerate() {
        log::info!("ytmusic_lyrics: candidate[{i}] video_id={} duration={:.1}s", c.video_id, c.duration_secs);
    }
    let Some((best_rank, best)) = best_song_candidate(&candidates, duration_secs, DURATION_TOLERANCE_SECS) else {
        log::info!(
            "ytmusic_lyrics: no candidate within {DURATION_TOLERANCE_SECS}s of target duration ({} candidates)",
            candidates.len()
        );
        return None;
    };
    log::info!("ytmusic_lyrics: picked candidate[{best_rank}] video_id={}", best.video_id);
    let video_id = best.video_id.clone();

    let browse_id = {
        let video_id_for_call = video_id.clone();
        match tokio::task::spawn_blocking(move || lyrics_browse_id_blocking(&video_id_for_call)).await {
            Ok(Ok(Some(id))) => id,
            Ok(Ok(None)) => {
                log::info!("ytmusic_lyrics[{video_id}]: no lyrics tab found");
                return None;
            }
            Ok(Err(e)) => {
                log::info!("ytmusic_lyrics[{video_id}]: browseId lookup failed: {e}");
                return None;
            }
            Err(e) => {
                log::info!("ytmusic_lyrics[{video_id}]: browseId task panicked: {e}");
                return None;
            }
        }
    };

    let lines = {
        let browse_id = browse_id.clone();
        match tokio::task::spawn_blocking(move || timed_lyrics_blocking(&browse_id)).await {
            Ok(Ok(Some(lines))) => lines,
            Ok(Ok(None)) => {
                log::info!("ytmusic_lyrics[{video_id}]: no timed lyrics data found");
                return None;
            }
            Ok(Err(e)) => {
                log::info!("ytmusic_lyrics[{video_id}]: timed lyrics fetch failed: {e}");
                return None;
            }
            Err(e) => {
                log::info!("ytmusic_lyrics[{video_id}]: timed lyrics task panicked: {e}");
                return None;
            }
        }
    };
    log::info!("ytmusic_lyrics[{video_id}]: got {} synced lines", lines.len());
    Some(CachedLyrics::Synced { lines, credit: None })
}

#[cfg(test)]
mod parse_mmss_tests {
    use super::*;

    #[test]
    fn minutes_and_seconds() {
        assert_eq!(parse_mmss("3:45"), Some(225.0));
    }

    #[test]
    fn hours_minutes_and_seconds() {
        assert_eq!(parse_mmss("1:02:03"), Some(3723.0));
    }

    #[test]
    fn single_digit_seconds_still_parses() {
        assert_eq!(parse_mmss("0:05"), Some(5.0));
    }

    #[test]
    fn non_duration_text_is_none() {
        assert_eq!(parse_mmss("Song"), None);
    }

    #[test]
    fn empty_string_is_none() {
        assert_eq!(parse_mmss(""), None);
    }

    #[test]
    fn non_digit_component_is_none() {
        assert_eq!(parse_mmss("3:4a"), None);
    }

    #[test]
    fn too_many_colon_separated_parts_is_none() {
        assert_eq!(parse_mmss("1:02:03:04"), None);
    }
}

#[cfg(test)]
mod best_song_candidate_tests {
    use super::*;

    fn candidate(id: &str, secs: f64) -> YtSongCandidate {
        YtSongCandidate { video_id: id.to_string(), duration_secs: secs }
    }

    #[test]
    fn picks_the_first_in_rank_order_within_tolerance_not_the_globally_closest() {
        // "b" is a worse duration match than "c" (diff 2 vs diff 1), but
        // "b" is YouTube's higher-ranked result and both are within
        // tolerance -- rank order must win, not duration proximity. This
        // is the exact bug found live: picking the globally closest
        // duration handed a same-titled wrong song priority over the
        // real, correctly-ranked top hit.
        let candidates = vec![candidate("a", 100.0), candidate("b", 223.0), candidate("c", 224.0)];
        let best = best_song_candidate(&candidates, 225.0, 5.0);
        assert_eq!(best.map(|(rank, c)| (rank, c.video_id.as_str())), Some((1, "b")));
    }

    #[test]
    fn skips_an_earlier_out_of_tolerance_candidate_for_a_later_in_tolerance_one() {
        let candidates = vec![candidate("a", 100.0), candidate("b", 226.0)];
        let best = best_song_candidate(&candidates, 225.0, 5.0);
        assert_eq!(best.map(|(rank, c)| (rank, c.video_id.as_str())), Some((1, "b")));
    }

    #[test]
    fn rejects_every_candidate_outside_tolerance() {
        let candidates = vec![candidate("a", 100.0), candidate("b", 400.0)];
        assert_eq!(best_song_candidate(&candidates, 225.0, 5.0), None);
    }

    #[test]
    fn empty_candidates_returns_none() {
        assert_eq!(best_song_candidate(&[], 225.0, 5.0), None);
    }
}

#[cfg(test)]
mod parse_search_results_tests {
    use super::*;

    fn search_response(rows: Vec<Value>) -> Value {
        serde_json::json!({
            "contents": {
                "tabbedSearchResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "musicShelfRenderer": {"contents": rows}
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        })
    }

    fn song_row(video_id: &str, duration_text: &str) -> Value {
        serde_json::json!({
            "musicResponsiveListItemRenderer": {
                "overlay": {"musicItemThumbnailOverlayRenderer": {"content": {"musicPlayButtonRenderer": {
                    "playNavigationEndpoint": {"watchEndpoint": {"videoId": video_id}}
                }}}},
                "flexColumns": [
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [{"text": "Some Title"}]}}},
                    {"musicResponsiveListItemFlexColumnRenderer": {"text": {"runs": [
                        {"text": "Song"}, {"text": " \u{2022} "}, {"text": "Some Artist"}, {"text": " \u{2022} "}, {"text": duration_text}
                    ]}}}
                ]
            }
        })
    }

    #[test]
    fn extracts_video_id_and_duration_from_a_real_shaped_row() {
        let response = search_response(vec![song_row("abc123", "3:45")]);
        let results = parse_search_results(&response);
        assert_eq!(results, vec![YtSongCandidate { video_id: "abc123".to_string(), duration_secs: 225.0 }]);
    }

    #[test]
    fn a_row_missing_a_duration_run_is_skipped_not_fatal() {
        let mut row = song_row("abc123", "3:45");
        row["musicResponsiveListItemRenderer"]["flexColumns"][1]["musicResponsiveListItemFlexColumnRenderer"]["text"]
            ["runs"] = serde_json::json!([{"text": "Song"}]);
        let response = search_response(vec![row]);
        assert!(parse_search_results(&response).is_empty());
    }

    #[test]
    fn unexpected_top_level_shape_returns_empty_not_panic() {
        let response = serde_json::json!({"totally": "different shape"});
        assert!(parse_search_results(&response).is_empty());
    }

    #[test]
    fn multiple_rows_all_parse() {
        let response = search_response(vec![song_row("a", "1:00"), song_row("b", "2:00")]);
        let results = parse_search_results(&response);
        assert_eq!(results.len(), 2);
    }
}

#[cfg(test)]
mod parse_lyrics_browse_id_tests {
    use super::*;

    fn watch_next_response(tabs: Vec<Value>) -> Value {
        serde_json::json!({
            "contents": {
                "singleColumnMusicWatchNextResultsRenderer": {
                    "tabbedRenderer": {"watchNextTabbedResultsRenderer": {"tabs": tabs}}
                }
            }
        })
    }

    fn tab(page_type: &str, browse_id: &str) -> Value {
        serde_json::json!({
            "tabRenderer": {
                "endpoint": {
                    "browseEndpoint": {
                        "browseId": browse_id,
                        "browseEndpointContextSupportedConfigs": {
                            "browseEndpointContextMusicConfig": {"pageType": page_type}
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn finds_the_lyrics_tagged_tab_among_others() {
        let response = watch_next_response(vec![
            tab("MUSIC_PAGE_TYPE_TRACK_RELATED", "UCrelated"),
            tab("MUSIC_PAGE_TYPE_TRACK_LYRICS", "UClyrics"),
        ]);
        assert_eq!(parse_lyrics_browse_id(&response), Some("UClyrics".to_string()));
    }

    #[test]
    fn no_lyrics_tab_present_returns_none() {
        let response = watch_next_response(vec![tab("MUSIC_PAGE_TYPE_TRACK_RELATED", "UCrelated")]);
        assert_eq!(parse_lyrics_browse_id(&response), None);
    }

    #[test]
    fn unexpected_shape_returns_none_not_panic() {
        assert_eq!(parse_lyrics_browse_id(&serde_json::json!({})), None);
    }
}

#[cfg(test)]
mod find_timed_lines_tests {
    use super::*;

    #[test]
    fn finds_timed_lyrics_data_nested_arbitrarily_deep() {
        let response = serde_json::json!({
            "contents": {"elementRenderer": {"newElement": {"type": {"componentType": {"model": {
                "timedLyricsModel": {"lyricsData": {"timedLyricsData": [
                    {"lyricLine": "line one", "cueRange": {"startTimeMilliseconds": "1000"}},
                    {"lyricLine": "line two", "cueRange": {"startTimeMilliseconds": "2500"}}
                ]}}
            }}}}}}
        });
        assert_eq!(
            find_timed_lines(&response),
            Some(vec![(1.0, "line one".to_string()), (2.5, "line two".to_string())])
        );
    }

    #[test]
    fn tries_alternate_field_names_for_text_and_start_time() {
        let response = serde_json::json!({
            "timedLyricsData": [{"text": "alt line", "startTimeMs": 500}]
        });
        assert_eq!(find_timed_lines(&response), Some(vec![(0.5, "alt line".to_string())]));
    }

    #[test]
    fn empty_array_returns_none() {
        let response = serde_json::json!({"timedLyricsData": []});
        assert_eq!(find_timed_lines(&response), None);
    }

    #[test]
    fn no_timed_lyrics_data_field_anywhere_returns_none() {
        let response = serde_json::json!({"some": {"other": "shape"}});
        assert_eq!(find_timed_lines(&response), None);
    }

    #[test]
    fn an_entry_missing_both_known_text_fields_is_skipped_not_fatal() {
        let response = serde_json::json!({
            "timedLyricsData": [
                {"cueRange": {"startTimeMilliseconds": "1000"}},
                {"lyricLine": "the only real line", "cueRange": {"startTimeMilliseconds": "2000"}}
            ]
        });
        assert_eq!(find_timed_lines(&response), Some(vec![(2.0, "the only real line".to_string())]));
    }
}
