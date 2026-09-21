//! Spicy Lyrics' official developer API (`GET /v1/lyrics/{trackId}`), the
//! primary lyrics source when a key is configured (Phase 26).
//!
//! Everything in here that doesn't touch the network is pure and unit
//! tested against trimmed copies of real responses (captured live, kept
//! inline below), because the response is a union whose branches are easy
//! to get subtly wrong.

use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a `200` body reduces to. `Static` (untimed text) is kept distinct
/// from `Miss` only so a later phase can offer it as plain lyrics; today
/// both fall through to the next source.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Timed lines (seconds, text), ascending, plus the credit line the
    /// docs require for a community sync (always present, see `credit_line`).
    Synced { lines: Vec<(f64, String)>, credit: String },
    Static,
    Miss,
}

/// The line under the lyrics that credits where they came from. A community
/// sync (`spicy_lyrics`) names the uploader -- and the maker when there is a
/// distinct one -- because the API's docs ask for exactly that; it is never
/// returned empty for that source, even if the uploader is missing. The
/// commercial catalogues are named as the source instead.
pub fn credit_line(source: &str, uploader: Option<&str>, maker: Option<&str>) -> String {
    let named = |name: Option<&str>| name.map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned);
    match source {
        "spicy_lyrics" => match named(uploader) {
            Some(uploader) => {
                let mut line = format!("Spicy Lyrics \u{b7} uploaded by {uploader}");
                if let Some(maker) = named(maker).filter(|m| *m != uploader) {
                    line.push_str(&format!(" \u{b7} made by {maker}"));
                }
                line
            }
            None => "Spicy Lyrics community sync".to_string(),
        },
        "apple_music" => "Apple Music via Spicy Lyrics".to_string(),
        "spotify" => "Spotify via Spicy Lyrics".to_string(),
        _ => "via Spicy Lyrics".to_string(),
    }
}

/// Reduces a `200` body to timed lines. Anything unexpected -- an error
/// envelope, an unknown `Type`, no usable rows, invalid JSON -- is a `Miss`,
/// never a panic: the `Type` and `source` unions can grow without notice.
pub fn parse_response(bytes: &[u8]) -> Parsed {
    let Ok(root) = serde_json::from_slice::<Value>(bytes) else {
        return Parsed::Miss;
    };
    let Some(body) = root.get("Body") else {
        return Parsed::Miss;
    };
    if body.get("error").is_some() {
        return Parsed::Miss;
    }

    let rows: Vec<(f64, String)> = match body.get("Type").and_then(Value::as_str) {
        Some("Line") => body.get("Content").and_then(Value::as_array).map(|c| c.iter().filter_map(line_row).collect()),
        Some("Syllable") => {
            body.get("Content").and_then(Value::as_array).map(|c| c.iter().filter_map(syllable_row).collect())
        }
        Some("Static") => return Parsed::Static,
        _ => return Parsed::Miss,
    }
    .unwrap_or_default();

    let mut lines: Vec<(f64, String)> = rows.into_iter().filter(|(_, text)| !text.trim().is_empty()).collect();
    if lines.is_empty() {
        return Parsed::Miss;
    }
    // `current_line_index` binary-searches, so order is a correctness matter.
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));

    let source = body.get("source").and_then(Value::as_str).unwrap_or("unknown");
    let attribution = body.get("UploadAttribution");
    let username = |who: &str| attribution.and_then(|a| a.get(who)).and_then(|w| w.get("username")).and_then(Value::as_str);
    Parsed::Synced { lines, credit: credit_line(source, username("Uploader"), username("Maker")) }
}

fn line_row(row: &Value) -> Option<(f64, String)> {
    let start = row.get("StartTime")?.as_f64()?;
    Some((start, row.get("Text")?.as_str()?.trim().to_owned()))
}

/// Joins syllables into words: a space goes between two syllables unless
/// the earlier one says the next continues the same word (`try` + `na`).
fn join_syllables(group: &Value) -> Option<(f64, String)> {
    let syllables = group.get("Syllables")?.as_array()?;
    let mut text = String::new();
    for (i, syllable) in syllables.iter().enumerate() {
        text.push_str(syllable.get("Text")?.as_str()?);
        let continues = syllable.get("IsPartOfWord").and_then(Value::as_bool).unwrap_or(false);
        if !continues && i + 1 < syllables.len() {
            text.push(' ');
        }
    }
    let start = group
        .get("StartTime")
        .and_then(Value::as_f64)
        .or_else(|| syllables.first()?.get("StartTime")?.as_f64())?;
    Some((start, text.trim().to_owned()))
}

fn syllable_row(row: &Value) -> Option<(f64, String)> {
    let (start, mut text) = join_syllables(row.get("Lead")?)?;
    for background in row.get("Background").and_then(Value::as_array).into_iter().flatten() {
        if let Some((_, phrase)) = join_syllables(background).filter(|(_, p)| !p.is_empty()) {
            text.push_str(&format!(" ({phrase})"));
        }
    }
    Some((start, text))
}

/// What to do after a response, decided from the status alone (plus the
/// stable machine-readable `error` code and `Retry-After` when present).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Parse the body.
    Use,
    /// Nothing for this track; try the next source.
    Miss,
    /// Stop calling for this long (rate limit / temporary outage).
    BackOff(Duration),
    /// The key itself is refused; stop for the rest of the session.
    Disable,
}

const DEFAULT_BACKOFF_SECS: u64 = 30;

/// `Retry-After` is the server's word, but it is clamped: 0 would mean a hot
/// loop and a day would silently disable lyrics for the whole session.
fn backoff(retry_after_secs: Option<u64>) -> Action {
    Action::BackOff(Duration::from_secs(retry_after_secs.unwrap_or(DEFAULT_BACKOFF_SECS).clamp(1, 300)))
}

pub fn classify_status(status: u16, error_code: Option<&str>, retry_after_secs: Option<u64>) -> Action {
    if error_code == Some("upstream_rate_limited") {
        return backoff(retry_after_secs);
    }
    match status {
        200 => Action::Use,
        401 | 403 => Action::Disable,
        429 | 503 => backoff(retry_after_secs),
        _ => Action::Miss,
    }
}

/// Most misses remembered per session. Bounded so a long listening session
/// can't grow it forever; past the cap it simply starts over (the cost is
/// one repeat request, never a wrong answer).
const MISS_MEMORY_CAP: usize = 2000;

/// Session-wide memory shared by every lookup task: whether the key was
/// refused, whether a rate limit is being waited out, and which tracks
/// already came back empty (so a replay doesn't spend quota again). Time is
/// passed in so this is testable without sleeping.
#[derive(Default)]
pub struct Gate {
    disabled: AtomicBool,
    inner: Mutex<GateInner>,
}

#[derive(Default)]
struct GateInner {
    blocked_until: Option<Instant>,
    misses: HashSet<String>,
}

impl Gate {
    pub fn allow(&self, track_id: &str, now: Instant) -> bool {
        if self.disabled.load(Ordering::Relaxed) {
            return false;
        }
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.blocked_until.is_none_or(|until| now >= until) && !inner.misses.contains(track_id)
    }

    pub fn record(&self, track_id: &str, action: Action, now: Instant) {
        match action {
            Action::Use => {}
            Action::Disable => self.disabled.store(true, Ordering::Relaxed),
            Action::BackOff(wait) => {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                let until = now + wait;
                // Never let a short back-off cut a longer one short.
                inner.blocked_until = Some(inner.blocked_until.map_or(until, |current| current.max(until)));
            }
            Action::Miss => {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.misses.len() >= MISS_MEMORY_CAP {
                    inner.misses.clear();
                }
                inner.misses.insert(track_id.to_owned());
            }
        }
    }
}

const BASE_URL: &str = "https://api.spicylyrics.org";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// A lyric sheet is tens of KB; this only stops a runaway body.
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;

/// A Spotify track id: 22 base62 characters. Checked before it goes into a
/// URL path, so nothing that could alter the path can ever be sent.
pub fn is_valid_track_id(id: &str) -> bool {
    id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The stable machine-readable `error` code in an error envelope, if any.
pub fn error_code(body: &[u8]) -> Option<String> {
    let root: Value = serde_json::from_slice(body).ok()?;
    root.get("Body")?.get("error")?.as_str().map(str::to_owned)
}

/// One raw response, before any decision is made about it.
struct Raw {
    status: u16,
    retry_after_secs: Option<u64>,
    body: Vec<u8>,
}

/// Blocking request. ureq reports 4xx/5xx as an `Err(Status)`, but the body
/// and `Retry-After` of those are exactly what the decision needs, so both
/// arms are folded into one `Raw`. Only a transport failure is an `Err`.
fn fetch_blocking(agent: &ureq::Agent, key: &crate::config::ApiKey, track_id: &str) -> Result<Raw, String> {
    use std::io::Read;
    let response = match agent
        .get(&format!("{BASE_URL}/v1/lyrics/{track_id}"))
        .set("Authorization", &format!("Bearer {}", key.expose()))
        .call()
    {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Err(e.without_url_or_key()),
    };
    let status = response.status();
    let retry_after_secs = response.header("Retry-After").and_then(|v| v.trim().parse().ok());
    let mut body = Vec::new();
    response.into_reader().take(MAX_BODY_BYTES).read_to_end(&mut body).map_err(|e| e.to_string())?;
    Ok(Raw { status, retry_after_secs, body })
}

trait TransportErrorText {
    fn without_url_or_key(&self) -> String;
}

impl TransportErrorText for ureq::Error {
    /// The text of a transport error, for the log. ureq's own `Display`
    /// can include the request URL; the URL here holds no secret (the key
    /// is a header), but keeping the log to the cause alone is one less
    /// thing to audit.
    fn without_url_or_key(&self) -> String {
        match self {
            ureq::Error::Transport(t) => t.kind().to_string(),
            other => other.to_string(),
        }
    }
}

/// The result a caller can use: timed lines and the credit to show.
pub type SpicyLyrics = (Vec<(f64, String)>, String);

/// Cheap to clone: the agent and gate are shared, so every per-track task
/// sees the same rate-limit and key-refused state.
#[derive(Clone)]
pub struct SpicyClient {
    key: crate::config::ApiKey,
    agent: ureq::Agent,
    gate: Arc<Gate>,
}

impl SpicyClient {
    pub fn new(key: crate::config::ApiKey) -> Self {
        Self { key, agent: ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build(), gate: Arc::new(Gate::default()) }
    }

    /// `Some` only for a usable *synced* result; every other outcome is `None`
    /// and the caller moves on to the next source. Every branch logs at
    /// `info!` (this app's filter drops `debug!` from its own code) -- with
    /// the track id and status code, never the key.
    pub async fn lyrics(&self, track_id: &str) -> Option<SpicyLyrics> {
        if !is_valid_track_id(track_id) {
            log::info!("spicy_lyrics: not a track id, skipped ({} chars)", track_id.len());
            return None;
        }
        if !self.gate.allow(track_id, Instant::now()) {
            log::info!("spicy_lyrics[{track_id}]: skipped (disabled, backing off, or already a miss this session)");
            return None;
        }

        let (agent, key, id) = (self.agent.clone(), self.key.clone(), track_id.to_owned());
        let raw = match tokio::task::spawn_blocking(move || fetch_blocking(&agent, &key, &id)).await {
            Ok(Ok(raw)) => raw,
            // Transient: fall through this time, but don't remember it as a miss.
            Ok(Err(e)) => {
                log::info!("spicy_lyrics[{track_id}]: request failed: {e}");
                return None;
            }
            Err(e) => {
                log::info!("spicy_lyrics[{track_id}]: lookup task failed: {e}");
                return None;
            }
        };

        let code = error_code(&raw.body);
        let action = classify_status(raw.status, code.as_deref(), raw.retry_after_secs);
        let now = Instant::now();
        match action {
            Action::Use => match parse_response(&raw.body) {
                Parsed::Synced { lines, credit } => {
                    log::info!("spicy_lyrics[{track_id}]: got {} synced lines ({credit})", lines.len());
                    return Some((lines, credit));
                }
                Parsed::Static => {
                    log::info!("spicy_lyrics[{track_id}]: only untimed lyrics available, falling through");
                    self.gate.record(track_id, Action::Miss, now);
                }
                Parsed::Miss => {
                    log::info!("spicy_lyrics[{track_id}]: 200 but no usable lines, falling through");
                    self.gate.record(track_id, Action::Miss, now);
                }
            },
            Action::Miss => {
                log::info!("spicy_lyrics[{track_id}]: no lyrics (status {}, {})", raw.status, code.as_deref().unwrap_or("no code"));
                self.gate.record(track_id, Action::Miss, now);
            }
            Action::BackOff(wait) => {
                log::warn!(
                    "spicy_lyrics: status {} ({}), backing off {}s",
                    raw.status,
                    code.as_deref().unwrap_or("no code"),
                    wait.as_secs()
                );
                self.gate.record(track_id, action, now);
            }
            Action::Disable => {
                log::warn!(
                    "spicy_lyrics: key refused (status {}, {}); skipping Spicy Lyrics for the rest of this session",
                    raw.status,
                    code.as_deref().unwrap_or("no code")
                );
                self.gate.record(track_id, action, now);
            }
        }
        None
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    /// `Type: Line`, `source: apple_music` -- real rows from "neon skies".
    const LINE_APPLE: &str = r#"{"Body":{"Type":"Line","SongWriters":["x"],"StartTime":1.163,"EndTime":149.321,
        "id":"7knSngLX3gWTH8ch4Y5aGr","source":"apple_music","Content":[
        {"Type":"Vocal","OppositeAligned":false,"Text":"Never thought I'd get this lucky","StartTime":1.163,"EndTime":4.671},
        {"Type":"Vocal","OppositeAligned":false,"Text":"Keep me from the cold","StartTime":5.967,"EndTime":9.204}]},
        "Status":200,"Type":"object"}"#;

    /// `Type: Syllable`, `source: spicy_lyrics` -- real rows from "Blinding
    /// Lights": "try"+"na" is one word, and the second row has a background.
    const SYLLABLE_COMMUNITY: &str = r#"{"Body":{"Type":"Syllable","StartTime":27.395,"EndTime":194.87,
        "id":"0VjIjW4GlUZAMYd2vXMi3b","source":"spicy_lyrics",
        "UploadAttribution":{"Uploader":{"id":"622852916092469278","username":"Arashii",
            "url":"https://spicylyrics.org/uid/622852916092469278","hasProfileBanner":true}},
        "Content":[
        {"Type":"Vocal","OppositeAligned":false,"Lead":{"Syllables":[
            {"Text":"I","IsPartOfWord":false,"StartTime":27.395,"EndTime":27.549},
            {"Text":"been","IsPartOfWord":false,"StartTime":27.549,"EndTime":27.74},
            {"Text":"try","IsPartOfWord":true,"StartTime":27.74,"EndTime":27.908},
            {"Text":"na","IsPartOfWord":false,"StartTime":27.908,"EndTime":28.077},
            {"Text":"call","IsPartOfWord":false,"StartTime":28.077,"EndTime":28.96}],
            "StartTime":27.395,"EndTime":28.96}},
        {"Type":"Vocal","OppositeAligned":false,"Lead":{"Syllables":[
            {"Text":"Hey","IsPartOfWord":false,"StartTime":109.0,"EndTime":109.4}],
            "StartTime":109.0,"EndTime":109.4},
         "Background":[{"Syllables":[{"Text":"Oh","IsPartOfWord":false,"StartTime":109.528,"EndTime":110.068}],
            "StartTime":109.528,"EndTime":110.068}]}]},"Status":200,"Type":"object"}"#;

    fn synced(json: &str) -> (Vec<(f64, String)>, String) {
        match parse_response(json.as_bytes()) {
            Parsed::Synced { lines, credit } => (lines, credit),
            other => panic!("expected Synced, got {other:?}"),
        }
    }

    #[test]
    fn line_type_maps_text_and_start_time_in_seconds() {
        let (lines, _) = synced(LINE_APPLE);
        assert_eq!(
            lines,
            vec![(1.163, "Never thought I'd get this lucky".to_string()), (5.967, "Keep me from the cold".to_string())]
        );
    }

    #[test]
    fn syllables_join_into_words_only_where_the_api_says_they_continue() {
        let (lines, _) = synced(SYLLABLE_COMMUNITY);
        assert_eq!(lines[0], (27.395, "I been tryna call".to_string()));
    }

    #[test]
    fn a_background_vocal_is_appended_in_parentheses_like_the_line_type_does() {
        let (lines, _) = synced(SYLLABLE_COMMUNITY);
        assert_eq!(lines[1], (109.0, "Hey (Oh)".to_string()));
    }

    #[test]
    fn rows_come_out_sorted_by_time_so_the_binary_search_stays_valid() {
        let json = r#"{"Body":{"Type":"Line","source":"apple_music","Content":[
            {"Type":"Vocal","Text":"second","StartTime":9.0},
            {"Type":"Vocal","Text":"first","StartTime":1.0}]}}"#;
        let (lines, _) = synced(json);
        assert_eq!(lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(), vec!["first", "second"]);
    }

    #[test]
    fn blank_rows_are_dropped() {
        let json = r#"{"Body":{"Type":"Line","source":"apple_music","Content":[
            {"Type":"Vocal","Text":"   ","StartTime":1.0},
            {"Type":"Vocal","Text":"real","StartTime":2.0}]}}"#;
        assert_eq!(synced(json).0, vec![(2.0, "real".to_string())]);
    }

    #[test]
    fn static_lyrics_are_not_synced() {
        let json = r#"{"Body":{"Type":"Static","source":"spotify","Lines":[{"Text":"a"},{"Text":"b"}]}}"#;
        assert_eq!(parse_response(json.as_bytes()), Parsed::Static);
    }

    #[test]
    fn an_unknown_type_is_a_miss_not_a_panic() {
        let json = r#"{"Body":{"Type":"Karaoke3D","source":"spotify","Content":[]}}"#;
        assert_eq!(parse_response(json.as_bytes()), Parsed::Miss);
    }

    #[test]
    fn an_error_envelope_is_a_miss() {
        let json = r#"{"Body":{"error":"lyrics_not_found","message":"No lyrics are available for that track."},"Status":404,"Type":"object"}"#;
        assert_eq!(parse_response(json.as_bytes()), Parsed::Miss);
    }

    #[test]
    fn empty_content_and_malformed_json_are_misses() {
        let empty = r#"{"Body":{"Type":"Line","source":"spotify","Content":[]}}"#;
        assert_eq!(parse_response(empty.as_bytes()), Parsed::Miss);
        assert_eq!(parse_response(b"<html>502</html>"), Parsed::Miss);
        assert_eq!(parse_response(b""), Parsed::Miss);
    }

    #[test]
    fn a_row_with_no_usable_time_is_skipped_but_the_rest_survive() {
        let json = r#"{"Body":{"Type":"Line","source":"apple_music","Content":[
            {"Type":"Vocal","Text":"no time"},
            {"Type":"Vocal","Text":"ok","StartTime":3.5}]}}"#;
        assert_eq!(synced(json).0, vec![(3.5, "ok".to_string())]);
    }

    #[test]
    fn the_credit_rides_along_with_the_result() {
        assert_eq!(synced(LINE_APPLE).1, "Apple Music via Spicy Lyrics");
        assert_eq!(synced(SYLLABLE_COMMUNITY).1, "Spicy Lyrics \u{b7} uploaded by Arashii");
    }
}

#[cfg(test)]
mod credit_tests {
    use super::*;

    #[test]
    fn a_community_sync_credits_the_uploader() {
        assert_eq!(credit_line("spicy_lyrics", Some("Arashii"), None), "Spicy Lyrics \u{b7} uploaded by Arashii");
    }

    #[test]
    fn a_distinct_maker_is_credited_too() {
        assert_eq!(
            credit_line("spicy_lyrics", Some("Arashii"), Some("Kaito")),
            "Spicy Lyrics \u{b7} uploaded by Arashii \u{b7} made by Kaito"
        );
    }

    #[test]
    fn the_same_person_as_uploader_and_maker_is_named_once() {
        assert_eq!(
            credit_line("spicy_lyrics", Some("Arashii"), Some("Arashii")),
            "Spicy Lyrics \u{b7} uploaded by Arashii"
        );
    }

    #[test]
    fn a_community_sync_is_never_left_without_a_credit_even_if_the_uploader_is_missing() {
        assert_eq!(credit_line("spicy_lyrics", None, None), "Spicy Lyrics community sync");
        assert_eq!(credit_line("spicy_lyrics", Some("  "), None), "Spicy Lyrics community sync");
    }

    #[test]
    fn commercial_catalogues_are_named_as_the_source() {
        assert_eq!(credit_line("apple_music", None, None), "Apple Music via Spicy Lyrics");
        assert_eq!(credit_line("spotify", None, None), "Spotify via Spicy Lyrics");
    }

    #[test]
    fn an_unknown_source_still_credits_the_service() {
        assert_eq!(credit_line("unknown", None, None), "via Spicy Lyrics");
        assert_eq!(credit_line("something_new", None, None), "via Spicy Lyrics");
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn a_200_is_used() {
        assert_eq!(classify_status(200, None, None), Action::Use);
    }

    #[test]
    fn no_lyrics_and_bad_ids_are_plain_misses() {
        assert_eq!(classify_status(404, Some("lyrics_not_found"), None), Action::Miss);
        assert_eq!(classify_status(400, Some("invalid_track_id"), None), Action::Miss);
    }

    #[test]
    fn a_bad_or_revoked_key_disables_the_source_for_the_session() {
        for (status, code) in [
            (401, "key_not_found"),
            (401, "missing_authorization"),
            (401, "key_revoked"),
            (403, "application_paused"),
            (403, "origin_not_allowed"),
        ] {
            assert_eq!(classify_status(status, Some(code), None), Action::Disable, "{status} {code}");
        }
    }

    #[test]
    fn rate_limits_back_off_for_retry_after() {
        assert_eq!(classify_status(429, Some("rate_limited"), Some(20)), Action::BackOff(secs(20)));
        assert_eq!(classify_status(503, Some("server_busy"), Some(7)), Action::BackOff(secs(7)));
    }

    #[test]
    fn retry_after_is_clamped_and_defaulted() {
        assert_eq!(classify_status(429, None, Some(0)), Action::BackOff(secs(1)));
        assert_eq!(classify_status(429, None, Some(86_400)), Action::BackOff(secs(300)));
        assert_eq!(classify_status(429, None, None), Action::BackOff(secs(30)));
    }

    #[test]
    fn an_upstream_rate_limit_backs_off_whatever_the_status() {
        assert_eq!(classify_status(502, Some("upstream_rate_limited"), None), Action::BackOff(secs(30)));
    }

    #[test]
    fn server_errors_and_oddities_are_misses_not_backoffs() {
        assert_eq!(classify_status(500, Some("internal_error"), None), Action::Miss);
        assert_eq!(classify_status(502, Some("upstream_error"), None), Action::Miss);
        assert_eq!(classify_status(418, None, None), Action::Miss);
        assert_eq!(classify_status(301, None, None), Action::Miss);
    }

    #[test]
    fn a_fresh_gate_lets_everything_through() {
        let gate = Gate::default();
        assert!(gate.allow("trackA", Instant::now()));
    }

    #[test]
    fn a_miss_only_blocks_that_track() {
        let gate = Gate::default();
        let now = Instant::now();
        gate.record("trackA", Action::Miss, now);
        assert!(!gate.allow("trackA", now));
        assert!(gate.allow("trackB", now));
    }

    #[test]
    fn disable_blocks_everything_permanently() {
        let gate = Gate::default();
        let now = Instant::now();
        gate.record("trackA", Action::Disable, now);
        assert!(!gate.allow("trackB", now));
        assert!(!gate.allow("trackB", now + secs(100_000)));
    }

    #[test]
    fn backoff_blocks_everything_until_it_expires() {
        let gate = Gate::default();
        let now = Instant::now();
        gate.record("trackA", Action::BackOff(secs(30)), now);
        assert!(!gate.allow("trackB", now + secs(29)));
        assert!(gate.allow("trackB", now + secs(31)));
    }

    #[test]
    fn a_shorter_backoff_never_shortens_a_longer_one() {
        let gate = Gate::default();
        let now = Instant::now();
        gate.record("a", Action::BackOff(secs(120)), now);
        gate.record("b", Action::BackOff(secs(5)), now);
        assert!(!gate.allow("c", now + secs(60)));
    }

    #[test]
    fn use_records_nothing() {
        let gate = Gate::default();
        let now = Instant::now();
        gate.record("trackA", Action::Use, now);
        assert!(gate.allow("trackA", now));
    }

    #[test]
    fn the_miss_memory_is_bounded() {
        let gate = Gate::default();
        let now = Instant::now();
        for i in 0..(MISS_MEMORY_CAP + 5) {
            gate.record(&format!("track{i}"), Action::Miss, now);
        }
        // Past the cap it starts over rather than growing without limit.
        assert!(gate.allow("track0", now));
        assert!(!gate.allow(&format!("track{}", MISS_MEMORY_CAP + 4), now));
    }
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn a_track_id_is_exactly_22_base62_characters() {
        assert!(is_valid_track_id("7knSngLX3gWTH8ch4Y5aGr"));
        assert!(!is_valid_track_id("7knSngLX3gWTH8ch4Y5aG"), "21 chars");
        assert!(!is_valid_track_id("7knSngLX3gWTH8ch4Y5aGrX"), "23 chars");
        assert!(!is_valid_track_id(""));
    }

    #[test]
    fn nothing_that_could_alter_the_url_path_gets_through() {
        assert!(!is_valid_track_id("7knSngLX3gWTH8ch4Y5a/."));
        assert!(!is_valid_track_id("../../etc/passwd-padding"));
        assert!(!is_valid_track_id("7knSngLX3gWTH8ch4Y5a?"));
        assert!(!is_valid_track_id("7knSngLX3gWTH8ch4Y5a "));
    }

    #[test]
    fn the_error_code_is_read_from_the_envelope() {
        let body = br#"{"Body":{"error":"key_not_found","message":"No key matches that hash."},"Status":401,"Type":"object"}"#;
        assert_eq!(error_code(body).as_deref(), Some("key_not_found"));
    }

    #[test]
    fn a_body_without_a_code_yields_none() {
        assert_eq!(error_code(br#"{"Body":{"Type":"Line"}}"#), None);
        assert_eq!(error_code(b"<html>bad gateway</html>"), None);
        assert_eq!(error_code(b""), None);
    }
}

/// Live checks against the real API. `#[ignore]`d so `cargo test` never
/// touches the network or needs a key; run with
/// `SPICY_LYRICS_API_KEY=... cargo test spicy_live -- --ignored --test-threads=1`.
#[cfg(test)]
mod spicy_live {
    use super::*;
    use crate::config::{ApiKey, SPICY_LYRICS_KEY_ENV};

    const NEON_SKIES: &str = "7knSngLX3gWTH8ch4Y5aGr"; // Spotify 404s, Apple Music via Spicy has it
    const BLINDING_LIGHTS: &str = "0VjIjW4GlUZAMYd2vXMi3b"; // community word-level sync
    const NO_LYRICS: &str = "1EoThnDm6kQfB2idIfR30n"; // 404 lyrics_not_found

    fn client() -> SpicyClient {
        let key = std::env::var(SPICY_LYRICS_KEY_ENV).expect("set SPICY_LYRICS_API_KEY to run the live tests");
        SpicyClient::new(ApiKey::new(key))
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_track_spotify_404s_on_comes_back_synced_from_apple_music() {
        let (lines, credit) = client().lyrics(NEON_SKIES).await.expect("synced lyrics");
        assert!(lines.len() > 30, "got {} lines", lines.len());
        assert!(lines.windows(2).all(|w| w[0].0 <= w[1].0), "not ascending");
        assert_eq!(credit, "Apple Music via Spicy Lyrics");
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_community_word_level_sync_is_reduced_to_lines_with_the_uploader_credited() {
        let (lines, credit) = client().lyrics(BLINDING_LIGHTS).await.expect("synced lyrics");
        assert!(lines.len() > 20, "got {} lines", lines.len());
        assert!(credit.starts_with("Spicy Lyrics \u{b7} uploaded by "), "credit was {credit:?}");
        assert!(lines.iter().any(|(_, t)| t.contains("tryna")), "syllables were not joined into words");
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_track_with_no_lyrics_is_a_miss_and_is_remembered() {
        let c = client();
        assert!(c.lyrics(NO_LYRICS).await.is_none());
        assert!(!c.gate.allow(NO_LYRICS, Instant::now()), "a repeat should not spend quota again");
        assert!(c.gate.allow(NEON_SKIES, Instant::now()), "other tracks are unaffected");
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_refused_key_disables_the_source_without_breaking_anything() {
        let c = SpicyClient::new(ApiKey::new("sl_sk_not_a_real_key_00000000000000000000000000"));
        assert!(c.lyrics(NEON_SKIES).await.is_none());
        assert!(!c.gate.allow(BLINDING_LIGHTS, Instant::now()), "the whole source should be off now");
    }
}
