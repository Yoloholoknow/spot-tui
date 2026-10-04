// Spicy Lyrics' official developer API (`GET /v1/lyrics/{trackId}`), the primary
// lyrics source when a key is configured.
//
// Everything that doesn't touch the network is pure and unit tested against
// trimmed copies of real responses (kept inline below), because the response is
// a union whose branches are easy to get subtly wrong.

use crate::lyrics::WordSeg;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a `200` body reduces to. `Static` (untimed text) is kept distinct
/// from `Miss` so the pipeline can offer it as plain lyrics when no later
/// source finds a synced sheet.
#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    /// Timed lines (seconds, text), ascending, plus the credit line the
    /// docs require for a community sync (always present, see `credit_line`).
    Synced {
        lines: Vec<(f64, String)>,
        words: Vec<Vec<WordSeg>>,
        credit: String,
    },
    Static(String),
    Miss,
}

/// The line under the lyrics that credits where they came from. A community
/// sync (`spicy_lyrics`) names the uploader -- and the maker when there is a
/// distinct one -- because the API's docs ask for exactly that; it is never
/// returned empty for that source, even if the uploader is missing. The
/// commercial catalogues are named as the source instead.
pub fn credit_line(source: &str, uploader: Option<&str>, maker: Option<&str>) -> String {
    let named = |name: Option<&str>| {
        name.map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    };
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

/// One reduced lyric row: when it starts, the text, and (for a syllable
/// sync) its timed segments, which concatenate to exactly `text`.
struct Row {
    start: f64,
    text: String,
    words: Vec<WordSeg>,
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

    let content = body.get("Content").and_then(Value::as_array);
    let rows: Vec<Row> = match body.get("Type").and_then(Value::as_str) {
        Some("Line") => content.map(|c| c.iter().filter_map(line_row).collect()),
        Some("Syllable") => content.map(|c| c.iter().filter_map(syllable_row).collect()),
        Some("Static") => return static_text(body),
        _ => return Parsed::Miss,
    }
    .unwrap_or_default();

    let mut rows: Vec<Row> = rows
        .into_iter()
        .filter(|row| !row.text.trim().is_empty())
        .collect();
    if rows.is_empty() {
        return Parsed::Miss;
    }
    // `current_line_index` binary-searches, so order is a correctness matter.
    // Sorting whole rows keeps each line's words attached to it.
    rows.sort_by(|a, b| a.start.total_cmp(&b.start));

    let has_words = rows.iter().any(|row| !row.words.is_empty());
    let lines = rows
        .iter()
        .map(|row| (row.start, row.text.clone()))
        .collect();
    let words = if has_words {
        rows.into_iter().map(|row| row.words).collect()
    } else {
        Vec::new()
    };

    let source = body
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let attribution = body.get("UploadAttribution");
    let username = |who: &str| {
        attribution
            .and_then(|a| a.get(who))
            .and_then(|w| w.get("username"))
            .and_then(Value::as_str)
    };
    Parsed::Synced {
        lines,
        words,
        credit: credit_line(source, username("Uploader"), username("Maker")),
    }
}

/// The untimed sheet as plain text, one lyric line per line. Blank lines are
/// kept (they are stanza breaks); a sheet with no text at all is a `Miss`.
fn static_text(body: &Value) -> Parsed {
    let lines: Vec<&str> = body
        .get("Lines")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| row.get("Text").and_then(Value::as_str).unwrap_or("").trim())
                .collect()
        })
        .unwrap_or_default();
    if lines.iter().all(|line| line.is_empty()) {
        return Parsed::Miss;
    }
    Parsed::Static(lines.join("\n"))
}

fn line_row(row: &Value) -> Option<Row> {
    let start = row.get("StartTime")?.as_f64()?;
    Some(Row {
        start,
        text: row.get("Text")?.as_str()?.trim().to_owned(),
        words: Vec::new(),
    })
}

/// One vocal group as timed segments. A space follows every syllable unless
/// it says the next continues the same word (`try` + `na`); the group's own
/// last segment gets none, so callers decide what separates groups.
/// `open`/`close` wrap the whole group (parentheses for a background).
fn group_segments(group: &Value, open: &str, close: &str) -> Option<Vec<WordSeg>> {
    let syllables = group.get("Syllables")?.as_array()?;
    let last = syllables.len().checked_sub(1)?;
    let mut segs = Vec::with_capacity(syllables.len());
    for (i, syllable) in syllables.iter().enumerate() {
        let mut text = String::new();
        if i == 0 {
            text.push_str(open);
        }
        text.push_str(syllable.get("Text")?.as_str()?);
        if i == last {
            text.push_str(close);
        }
        let continues = syllable
            .get("IsPartOfWord")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !continues && i != last {
            text.push(' ');
        }
        let start = syllable.get("StartTime")?.as_f64()?;
        let end = syllable
            .get("EndTime")
            .and_then(Value::as_f64)
            .unwrap_or(start);
        segs.push(WordSeg { text, start, end });
    }
    Some(segs)
}

fn group_start(group: &Value, segs: &[WordSeg]) -> Option<f64> {
    group
        .get("StartTime")
        .and_then(Value::as_f64)
        .or_else(|| segs.first().map(|s| s.start))
}

fn syllable_row(row: &Value) -> Option<Row> {
    let lead = row.get("Lead")?;
    let mut words = group_segments(lead, "", "")?;
    let start = group_start(lead, &words)?;
    for background in row
        .get("Background")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        // A background phrase is appended in parentheses, like the API does
        // for a Line sync; it keeps its own timing, so it lights up when it
        // is sung, not when the lead is.
        if let Some(phrase) = group_segments(background, "(", ")").filter(|p| !p.is_empty()) {
            if let Some(previous) = words.last_mut() {
                previous.text.push(' ');
            }
            words.extend(phrase);
        }
    }
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<String>()
        .trim()
        .to_owned();
    Some(Row { start, text, words })
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
    Action::BackOff(Duration::from_secs(
        retry_after_secs
            .unwrap_or(DEFAULT_BACKOFF_SECS)
            .clamp(1, 300),
    ))
}

pub fn classify_status(
    status: u16,
    error_code: Option<&str>,
    retry_after_secs: Option<u64>,
) -> Action {
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
                inner.blocked_until = Some(
                    inner
                        .blocked_until
                        .map_or(until, |current| current.max(until)),
                );
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
fn fetch_blocking(
    agent: &ureq::Agent,
    key: &crate::config::ApiKey,
    track_id: &str,
) -> Result<Raw, String> {
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
    let retry_after_secs = response
        .header("Retry-After")
        .and_then(|v| v.trim().parse().ok());
    let mut body = Vec::new();
    response
        .into_reader()
        .take(MAX_BODY_BYTES)
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    Ok(Raw {
        status,
        retry_after_secs,
        body,
    })
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

/// The result a caller can use: timed lines, their word timing (parallel to
/// `lines`, empty when the sync is line-level only), and the credit to show.
#[derive(Debug, Clone, PartialEq)]
pub struct SpicyLyrics {
    pub lines: Vec<(f64, String)>,
    pub words: Vec<Vec<WordSeg>>,
    pub credit: String,
}

/// What a lookup found: a synced sheet, untimed text worth showing only if
/// nothing better turns up, or nothing.
#[derive(Debug, Clone, PartialEq)]
pub enum SpicyAnswer {
    Synced(SpicyLyrics),
    Plain(String),
    Nothing,
}

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
        Self {
            key,
            agent: ureq::AgentBuilder::new().timeout(HTTP_TIMEOUT).build(),
            gate: Arc::new(Gate::default()),
        }
    }

    /// `Some` only for a usable *synced* result; every other outcome is `None`
    /// and the caller moves on to the next source.
    #[cfg(test)]
    pub async fn lyrics(&self, track_id: &str) -> Option<SpicyLyrics> {
        match self.answer(track_id).await {
            SpicyAnswer::Synced(lyrics) => Some(lyrics),
            SpicyAnswer::Plain(_) | SpicyAnswer::Nothing => None,
        }
    }

    /// Like `lyrics`, but also hands back untimed text when that is all
    /// Spicy has, so the caller can fall back to it. Every branch logs at
    /// `info!` (this app's filter drops `debug!` from its own code) -- with
    /// the track id and status code, never the key.
    pub async fn answer(&self, track_id: &str) -> SpicyAnswer {
        if !is_valid_track_id(track_id) {
            log::info!(
                "spicy_lyrics: not a track id, skipped ({} chars)",
                track_id.len()
            );
            return SpicyAnswer::Nothing;
        }
        if !self.gate.allow(track_id, Instant::now()) {
            log::info!(
                "spicy_lyrics[{track_id}]: skipped (disabled, backing off, or already a miss this session)"
            );
            return SpicyAnswer::Nothing;
        }

        let (agent, key, id) = (self.agent.clone(), self.key.clone(), track_id.to_owned());
        let raw = match tokio::task::spawn_blocking(move || fetch_blocking(&agent, &key, &id)).await
        {
            Ok(Ok(raw)) => raw,
            // Transient: fall through this time, but don't remember it as a miss.
            Ok(Err(e)) => {
                log::info!("spicy_lyrics[{track_id}]: request failed: {e}");
                return SpicyAnswer::Nothing;
            }
            Err(e) => {
                log::info!("spicy_lyrics[{track_id}]: lookup task failed: {e}");
                return SpicyAnswer::Nothing;
            }
        };

        let code = error_code(&raw.body);
        let action = classify_status(raw.status, code.as_deref(), raw.retry_after_secs);
        let now = Instant::now();
        match action {
            Action::Use => match parse_response(&raw.body) {
                Parsed::Synced {
                    lines,
                    words,
                    credit,
                } => {
                    log::info!(
                        "spicy_lyrics[{track_id}]: got {} synced lines, {} with word timing ({credit})",
                        lines.len(),
                        words.iter().filter(|w| !w.is_empty()).count()
                    );
                    return SpicyAnswer::Synced(SpicyLyrics {
                        lines,
                        words,
                        credit,
                    });
                }
                Parsed::Static(text) => {
                    log::info!(
                        "spicy_lyrics[{track_id}]: only untimed lyrics available, keeping them as a fallback"
                    );
                    self.gate.record(track_id, Action::Miss, now);
                    return SpicyAnswer::Plain(text);
                }
                Parsed::Miss => {
                    log::info!(
                        "spicy_lyrics[{track_id}]: 200 but no usable lines, falling through"
                    );
                    self.gate.record(track_id, Action::Miss, now);
                }
            },
            Action::Miss => {
                log::info!(
                    "spicy_lyrics[{track_id}]: no lyrics (status {}, {})",
                    raw.status,
                    code.as_deref().unwrap_or("no code")
                );
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
        SpicyAnswer::Nothing
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
            Parsed::Synced { lines, credit, .. } => (lines, credit),
            other => panic!("expected Synced, got {other:?}"),
        }
    }

    fn words_of(json: &str) -> Vec<Vec<WordSeg>> {
        match parse_response(json.as_bytes()) {
            Parsed::Synced { words, .. } => words,
            other => panic!("expected Synced, got {other:?}"),
        }
    }

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg {
            text: text.to_string(),
            start,
            end,
        }
    }

    #[test]
    fn a_syllable_row_keeps_every_syllable_with_its_own_timing() {
        let words = words_of(SYLLABLE_COMMUNITY);
        assert_eq!(
            words[0],
            vec![
                seg("I ", 27.395, 27.549),
                seg("been ", 27.549, 27.74),
                seg("try", 27.74, 27.908),
                seg("na ", 27.908, 28.077),
                seg("call", 28.077, 28.96),
            ]
        );
    }

    #[test]
    fn a_background_vocal_becomes_its_own_timed_segment_inside_parentheses() {
        let words = words_of(SYLLABLE_COMMUNITY);
        assert_eq!(
            words[1],
            vec![seg("Hey ", 109.0, 109.4), seg("(Oh)", 109.528, 110.068)]
        );
    }

    #[test]
    fn every_line_s_segments_concatenate_to_exactly_that_line_s_text() {
        // The invariant the renderer relies on: styling per segment must
        // never change what is drawn or how it wraps.
        let (lines, _) = synced(SYLLABLE_COMMUNITY);
        let words = words_of(SYLLABLE_COMMUNITY);
        assert_eq!(words.len(), lines.len());
        for (line, segs) in lines.iter().zip(&words) {
            assert_eq!(
                segs.iter().map(|w| w.text.as_str()).collect::<String>(),
                line.1
            );
        }
    }

    #[test]
    fn a_line_type_sync_has_no_word_timing() {
        assert!(words_of(LINE_APPLE).is_empty());
    }

    #[test]
    fn words_stay_aligned_with_their_lines_after_sorting_and_filtering() {
        let json = r#"{"Body":{"Type":"Syllable","source":"spicy_lyrics","Content":[
            {"Type":"Vocal","Lead":{"Syllables":[{"Text":"second","IsPartOfWord":false,"StartTime":9.0,"EndTime":9.5}],"StartTime":9.0}},
            {"Type":"Vocal","Lead":{"Syllables":[{"Text":" ","IsPartOfWord":false,"StartTime":5.0,"EndTime":5.5}],"StartTime":5.0}},
            {"Type":"Vocal","Lead":{"Syllables":[{"Text":"first","IsPartOfWord":false,"StartTime":1.0,"EndTime":1.5}],"StartTime":1.0}}]}}"#;
        let (lines, _) = synced(json);
        let words = words_of(json);
        assert_eq!(
            lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(words[0], vec![seg("first", 1.0, 1.5)]);
        assert_eq!(words[1], vec![seg("second", 9.0, 9.5)]);
    }

    #[test]
    fn several_background_groups_are_separated_by_a_space() {
        let json = r#"{"Body":{"Type":"Syllable","source":"spicy_lyrics","Content":[
            {"Type":"Vocal","Lead":{"Syllables":[{"Text":"Hey","IsPartOfWord":false,"StartTime":1.0,"EndTime":1.5}],"StartTime":1.0},
             "Background":[
               {"Syllables":[{"Text":"oh","IsPartOfWord":false,"StartTime":1.6,"EndTime":1.8}],"StartTime":1.6},
               {"Syllables":[{"Text":"yeah","IsPartOfWord":false,"StartTime":2.0,"EndTime":2.4}],"StartTime":2.0}]}]}}"#;
        let (lines, _) = synced(json);
        assert_eq!(lines[0].1, "Hey (oh) (yeah)");
        let words = words_of(json);
        assert_eq!(
            words[0].iter().map(|w| w.text.as_str()).collect::<String>(),
            "Hey (oh) (yeah)"
        );
    }

    #[test]
    fn line_type_maps_text_and_start_time_in_seconds() {
        let (lines, _) = synced(LINE_APPLE);
        assert_eq!(
            lines,
            vec![
                (1.163, "Never thought I'd get this lucky".to_string()),
                (5.967, "Keep me from the cold".to_string())
            ]
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
        assert_eq!(
            lines.iter().map(|l| l.1.as_str()).collect::<Vec<_>>(),
            vec!["first", "second"]
        );
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
        let json =
            r#"{"Body":{"Type":"Static","source":"spotify","Lines":[{"Text":"a"},{"Text":"b"}]}}"#;
        assert_eq!(
            parse_response(json.as_bytes()),
            Parsed::Static("a\nb".to_string())
        );
    }

    #[test]
    fn static_lyrics_keep_stanza_breaks() {
        let json =
            r#"{"Body":{"Type":"Static","Lines":[{"Text":"a"},{"Text":""},{"Text":" b "}]}}"#;
        assert_eq!(
            parse_response(json.as_bytes()),
            Parsed::Static("a\n\nb".to_string())
        );
    }

    #[test]
    fn static_lyrics_with_no_text_are_a_miss() {
        let json = r#"{"Body":{"Type":"Static","Lines":[{"Text":" "}]}}"#;
        assert_eq!(parse_response(json.as_bytes()), Parsed::Miss);
        let json = r#"{"Body":{"Type":"Static"}}"#;
        assert_eq!(parse_response(json.as_bytes()), Parsed::Miss);
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
        assert_eq!(
            synced(SYLLABLE_COMMUNITY).1,
            "Spicy Lyrics \u{b7} uploaded by Arashii"
        );
    }
}

#[cfg(test)]
mod credit_tests {
    use super::*;

    #[test]
    fn a_community_sync_credits_the_uploader() {
        assert_eq!(
            credit_line("spicy_lyrics", Some("Arashii"), None),
            "Spicy Lyrics \u{b7} uploaded by Arashii"
        );
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
        assert_eq!(
            credit_line("spicy_lyrics", None, None),
            "Spicy Lyrics community sync"
        );
        assert_eq!(
            credit_line("spicy_lyrics", Some("  "), None),
            "Spicy Lyrics community sync"
        );
    }

    #[test]
    fn commercial_catalogues_are_named_as_the_source() {
        assert_eq!(
            credit_line("apple_music", None, None),
            "Apple Music via Spicy Lyrics"
        );
        assert_eq!(
            credit_line("spotify", None, None),
            "Spotify via Spicy Lyrics"
        );
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
        assert_eq!(
            classify_status(404, Some("lyrics_not_found"), None),
            Action::Miss
        );
        assert_eq!(
            classify_status(400, Some("invalid_track_id"), None),
            Action::Miss
        );
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
            assert_eq!(
                classify_status(status, Some(code), None),
                Action::Disable,
                "{status} {code}"
            );
        }
    }

    #[test]
    fn rate_limits_back_off_for_retry_after() {
        assert_eq!(
            classify_status(429, Some("rate_limited"), Some(20)),
            Action::BackOff(secs(20))
        );
        assert_eq!(
            classify_status(503, Some("server_busy"), Some(7)),
            Action::BackOff(secs(7))
        );
    }

    #[test]
    fn retry_after_is_clamped_and_defaulted() {
        assert_eq!(
            classify_status(429, None, Some(0)),
            Action::BackOff(secs(1))
        );
        assert_eq!(
            classify_status(429, None, Some(86_400)),
            Action::BackOff(secs(300))
        );
        assert_eq!(classify_status(429, None, None), Action::BackOff(secs(30)));
    }

    #[test]
    fn an_upstream_rate_limit_backs_off_whatever_the_status() {
        assert_eq!(
            classify_status(502, Some("upstream_rate_limited"), None),
            Action::BackOff(secs(30))
        );
    }

    #[test]
    fn server_errors_and_oddities_are_misses_not_backoffs() {
        assert_eq!(
            classify_status(500, Some("internal_error"), None),
            Action::Miss
        );
        assert_eq!(
            classify_status(502, Some("upstream_error"), None),
            Action::Miss
        );
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
        let key = std::env::var(SPICY_LYRICS_KEY_ENV)
            .expect("set SPICY_LYRICS_API_KEY to run the live tests");
        SpicyClient::new(ApiKey::new(key))
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_track_spotify_404s_on_comes_back_synced_from_apple_music() {
        let SpicyLyrics {
            lines,
            words,
            credit,
        } = client().lyrics(NEON_SKIES).await.expect("synced lyrics");
        assert!(lines.len() > 30, "got {} lines", lines.len());
        assert!(lines.windows(2).all(|w| w[0].0 <= w[1].0), "not ascending");
        assert_eq!(credit, "Apple Music via Spicy Lyrics");
        assert!(words.is_empty(), "a Line-level sync has no word timing");
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_community_word_level_sync_is_reduced_to_lines_with_the_uploader_credited() {
        let SpicyLyrics {
            lines,
            words,
            credit,
        } = client()
            .lyrics(BLINDING_LIGHTS)
            .await
            .expect("synced lyrics");
        assert!(lines.len() > 20, "got {} lines", lines.len());
        assert!(
            credit.starts_with("Spicy Lyrics \u{b7} uploaded by "),
            "credit was {credit:?}"
        );
        assert!(
            lines.iter().any(|(_, t)| t.contains("tryna")),
            "syllables were not joined into words"
        );
        // Word timing is kept, one entry per line, and always re-forms the line.
        assert_eq!(words.len(), lines.len());
        for ((_, text), segs) in lines.iter().zip(&words) {
            assert_eq!(
                &segs.iter().map(|w| w.text.as_str()).collect::<String>(),
                text
            );
            assert!(
                segs.iter().all(|w| w.end >= w.start),
                "a segment ends before it starts"
            );
        }
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_track_with_no_lyrics_is_a_miss_and_is_remembered() {
        let c = client();
        assert!(c.lyrics(NO_LYRICS).await.is_none());
        assert!(
            !c.gate.allow(NO_LYRICS, Instant::now()),
            "a repeat should not spend quota again"
        );
        assert!(
            c.gate.allow(NEON_SKIES, Instant::now()),
            "other tracks are unaffected"
        );
    }

    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn a_refused_key_disables_the_source_without_breaking_anything() {
        let c = SpicyClient::new(ApiKey::new(
            "sl_sk_not_a_real_key_00000000000000000000000000",
        ));
        assert!(c.lyrics(NEON_SKIES).await.is_none());
        assert!(
            !c.gate.allow(BLINDING_LIGHTS, Instant::now()),
            "the whole source should be off now"
        );
    }

    /// Real J-pop, K-pop and C-pop sheets through the romanizer: the word
    /// timing must re-form each romanized line, and the whole thing (including
    /// the one-time Japanese dictionary load) must be quick enough to run when
    /// the toggle is pressed. Run with `--nocapture` to read the output.
    #[tokio::test]
    #[ignore = "live network; needs SPICY_LYRICS_API_KEY"]
    async fn real_cjk_sheets_romanize_quickly_and_re_time_cleanly() {
        use crate::lyrics::LyricLine;
        use std::time::Duration;

        let c = client();
        let tracks = [
            (
                "7ovUcF5uHTBRzUpB6ZOmvt",
                "J-pop  \u{30a2}\u{30a4}\u{30c9}\u{30eb}",
            ),
            ("03UrZgTINDqvnUMbbIMhql", "K-pop  Gangnam Style"),
            ("0Q5VnK2DYzRyfqQRJuUtvi", "K-pop  LOVE DIVE"),
            (
                "2tqF9MPNdYdJU70U0ULO23",
                "C-pop  \u{544a}\u{767d}\u{6c23}\u{7403}",
            ),
        ];
        for (id, name) in tracks {
            let SpicyLyrics { lines, words, .. } = c.lyrics(id).await.expect("synced lyrics");
            let sheet: Vec<LyricLine> = lines
                .iter()
                .enumerate()
                .map(|(i, (start, text))| LyricLine {
                    timestamp: Duration::from_secs_f64(*start),
                    text: text.clone(),
                    words: words.get(i).cloned().unwrap_or_default(),
                })
                .collect();
            let started = std::time::Instant::now();
            let romanized = crate::lyrics::romanize::romanize_lyric_lines(&sheet);
            let took = started.elapsed();
            let with_text = romanized.iter().flatten().count();
            println!(
                "=== {name}: {} lines, {with_text} romanized, {took:?}",
                sheet.len()
            );
            for (line, roman) in sheet
                .iter()
                .zip(&romanized)
                .filter(|(_, r)| r.is_some())
                .take(5)
            {
                println!("  {}\n    -> {}", line.text, roman.as_ref().unwrap().text);
            }
            // Long vowels are the part most worth eyeballing on real lyrics.
            let has_macron = |text: &str| {
                text.chars()
                    .any(|c| "\u{101}\u{12b}\u{16b}\u{113}\u{14d}".contains(c))
            };
            for (line, roman) in sheet
                .iter()
                .zip(&romanized)
                .filter(|(_, r)| r.as_ref().is_some_and(|r| has_macron(&r.text)))
                .take(4)
            {
                println!(
                    "  [long vowel] {}\n    -> {}",
                    line.text,
                    roman.as_ref().unwrap().text
                );
            }
            for (line, roman) in sheet.iter().zip(&romanized) {
                if let Some(roman) = roman.as_ref().filter(|r| !r.words.is_empty()) {
                    assert_eq!(
                        roman
                            .words
                            .iter()
                            .map(|w| w.text.as_str())
                            .collect::<String>(),
                        roman.text
                    );
                    assert!(!line.words.is_empty());
                }
            }
            assert!(
                with_text * 2 > sheet.len(),
                "under half the lines were romanized for {name}"
            );
            assert!(
                took < Duration::from_secs(10),
                "romanizing {name} took {took:?}"
            );
        }
    }
}
