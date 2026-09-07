//! ratatui rendering: compact side-pane layout and fullscreen layout.

use crate::lyrics::LyricLine;
use crate::api::search::TrackResult;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Duration;

/// A screen in the main-pane stack. Only the two screens that exist today
/// -- more variants land alongside the phase that actually builds them
/// (Library in Phase 2, Queue in Phase 7, Devices in Phase 8, Help in
/// Phase 9), rather than stubbing out destinations nothing can reach yet.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Screen {
    NowPlaying,
    Search,
}

/// Which persistent pane currently receives arrow keys / `Enter`, toggled
/// by `Tab`. See the design-scope plan's Navigation model.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Focus {
    Sidebar,
    Main,
}

/// The main-pane screen stack plus which persistent pane has focus.
/// `NowPlaying` is always the stack root and can never be popped past.
pub struct Nav {
    stack: Vec<Screen>,
    pub focus: Focus,
}

impl Nav {
    pub fn new() -> Self {
        Self {
            stack: vec![Screen::NowPlaying],
            focus: Focus::Sidebar,
        }
    }

    pub fn top(&self) -> &Screen {
        self.stack.last().expect("stack is never empty -- NowPlaying is the permanent root")
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// In-content drill-down: pushes a child screen, `Esc` pops one level.
    pub fn push(&mut self, screen: Screen) {
        self.stack.push(screen);
    }

    /// Pops one level. Does nothing at the root (`NowPlaying` can't be
    /// popped past) -- returns whether it actually popped.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }

    /// Sidebar-triggered navigation: collapses any in-content drill-down
    /// depth, like switching a tab -- but `NowPlaying` stays the
    /// permanent root underneath, exactly as `push`/`pop` leave it. A
    /// version of this that replaced the whole stack made `Esc` a
    /// permanent no-op after any sidebar-triggered navigation, since
    /// `pop` refuses to remove the last remaining screen -- there was
    /// nothing left under it to land on.
    pub fn goto(&mut self, screen: Screen) {
        self.stack = if screen == Screen::NowPlaying {
            vec![Screen::NowPlaying]
        } else {
            vec![Screen::NowPlaying, screen]
        };
    }

    /// `Esc`'s full behavior in the Main pane: pop one level, and if that
    /// pop lands back at the permanent root, hand focus to the Sidebar in
    /// the same keystroke -- a separate second `Esc` just to reach the
    /// Sidebar after already backing out felt like one press too many
    /// (confirmed live). Every future main-pane screen's `Esc` handler
    /// should call this rather than reimplementing the pop-then-check.
    pub fn escape(&mut self) {
        self.pop();
        if self.depth() == 1 {
            self.focus = Focus::Sidebar;
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Sidebar => Focus::Main,
            Focus::Main => Focus::Sidebar,
        };
    }
}

impl Default for Nav {
    fn default() -> Self {
        Self::new()
    }
}

/// Sidebar menu entries. Shared between rendering and key-dispatch so the
/// two can never drift. Grows alongside the phase that builds each real
/// destination (Library in Phase 2, Queue in Phase 7, Devices in Phase 8,
/// Help in Phase 9) -- only the two screens that exist today are listed.
pub const SIDEBAR_ENTRIES: &[(&str, Screen)] = &[("Now Playing", Screen::NowPlaying), ("Search", Screen::Search)];

#[cfg(test)]
mod nav_tests {
    use super::*;

    #[test]
    fn new_starts_at_now_playing_with_sidebar_focus() {
        let nav = Nav::new();
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.focus, Focus::Sidebar);
        assert_eq!(nav.depth(), 1);
    }

    #[test]
    fn push_adds_a_screen_on_top() {
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        assert_eq!(nav.top(), &Screen::Search);
        assert_eq!(nav.depth(), 2);
    }

    #[test]
    fn pop_returns_to_previous_screen() {
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        assert!(nav.pop());
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
    }

    #[test]
    fn pop_at_root_is_a_noop_and_returns_false() {
        let mut nav = Nav::new();
        assert!(!nav.pop());
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
    }

    #[test]
    fn escape_from_a_pushed_screen_pops_and_focuses_sidebar_in_one_step() {
        // Confirmed live: requiring a *second*, separate Esc just to
        // reach the Sidebar after already backing out to the root felt
        // like one press too many. One Esc should do both.
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        nav.focus = Focus::Main;
        nav.escape();
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
        assert_eq!(nav.focus, Focus::Sidebar);
    }

    #[test]
    fn escape_at_the_root_focuses_sidebar_even_with_nothing_to_pop() {
        let mut nav = Nav::new();
        nav.focus = Focus::Main;
        nav.escape();
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
        assert_eq!(nav.focus, Focus::Sidebar);
    }

    #[test]
    fn escape_one_level_deep_of_a_taller_stack_does_not_yet_touch_focus() {
        // Only hands control to the Sidebar once Main is fully backed
        // out of, not on every intermediate pop.
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        nav.push(Screen::Search);
        nav.focus = Focus::Main;
        nav.escape();
        assert_eq!(nav.depth(), 2);
        assert_eq!(nav.focus, Focus::Main);
    }

    #[test]
    fn goto_collapses_any_drill_down_depth_but_keeps_now_playing_at_the_root() {
        // NowPlaying must never be evicted from the stack -- it's the
        // permanent root Esc can always land on. A `goto` that replaced
        // the whole stack (the original, buggy implementation) made Esc
        // a permanent no-op after any sidebar-triggered navigation: real
        // bug, reported live ("esc doesn't work, stuck in search").
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        nav.push(Screen::Search);
        nav.goto(Screen::Search);
        assert_eq!(nav.depth(), 2);
        assert_eq!(nav.top(), &Screen::Search);
    }

    #[test]
    fn goto_now_playing_collapses_all_the_way_to_the_root() {
        let mut nav = Nav::new();
        nav.push(Screen::Search);
        nav.goto(Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
        assert_eq!(nav.top(), &Screen::NowPlaying);
    }

    #[test]
    fn esc_after_sidebar_triggered_goto_returns_to_now_playing_not_stuck() {
        // The exact repro: enter Search via `goto` (as the Sidebar and
        // the global `/` shortcut both do), then Esc once -- must land on
        // NowPlaying, not stay stuck on Search with pop() being a no-op.
        let mut nav = Nav::new();
        nav.goto(Screen::Search);
        assert!(nav.pop());
        assert_eq!(nav.top(), &Screen::NowPlaying);
        assert_eq!(nav.depth(), 1);
    }

    #[test]
    fn toggle_focus_flips_between_sidebar_and_main() {
        let mut nav = Nav::new();
        assert_eq!(nav.focus, Focus::Sidebar);
        nav.toggle_focus();
        assert_eq!(nav.focus, Focus::Main);
        nav.toggle_focus();
        assert_eq!(nav.focus, Focus::Sidebar);
    }
}

pub struct SearchState {
    pub query: String,
    pub results: Vec<TrackResult>,
    pub selected: usize,
    pub searching: bool,
    pub client_ready: bool,
    /// Distinct from "searched, zero matches" -- a real error (rate
    /// limit, network, auth) gets surfaced instead of silently looking
    /// like an empty result set.
    pub error: Option<String>,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            results: Vec::new(),
            selected: 0,
            searching: false,
            client_ready: false,
            error: None,
        }
    }
}

/// The one deliberate accent color (progress bar fill + current lyric
/// line). Everything else stays default/dim -- restraint per
/// fable-ui-design: one bold moment, not color everywhere. Indexed
/// (not RGB) so it renders correctly over plain tmux-256color, not just
/// true-color terminals.
const ACCENT: Color = Color::Indexed(35); // a spotify-adjacent green

/// `mm:ss`, minutes uncapped (a >59min track just shows e.g. "61:05"
/// rather than growing an hours field nobody needs here).
fn format_mmss(d: Duration) -> String {
    let total_secs = d.as_secs();
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// Truncates to at most `max` characters, appending an ellipsis if
/// anything was cut. Operates on chars, not bytes, so it's safe on
/// multi-byte UTF-8 (this library has CJK titles). Note: this counts
/// characters, not terminal display columns -- a CJK title truncated to
/// `max` chars can still render wider than `max` columns, since each such
/// glyph is double-width. Acceptable approximation for a header line;
/// revisit with `unicode-width` if it ever looks wrong in practice.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn format_mmss_pads_single_digit_seconds() {
        assert_eq!(format_mmss(Duration::from_secs(5)), "0:05");
    }

    #[test]
    fn format_mmss_zero() {
        assert_eq!(format_mmss(Duration::ZERO), "0:00");
    }

    #[test]
    fn format_mmss_rolls_minutes() {
        assert_eq!(format_mmss(Duration::from_secs(65)), "1:05");
    }

    #[test]
    fn format_mmss_uncapped_minutes_past_an_hour() {
        assert_eq!(format_mmss(Duration::from_secs(3661)), "61:01");
    }

    #[test]
    fn truncate_short_string_is_unchanged() {
        assert_eq!(truncate_ellipsis("abc", 10), "abc");
    }

    #[test]
    fn truncate_exact_length_boundary_is_unchanged() {
        assert_eq!(truncate_ellipsis("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_long_ascii_string_gets_ellipsis() {
        // 4 chars kept + ellipsis = 5 chars total, matching `max`.
        assert_eq!(truncate_ellipsis("abcdefgh", 5), "abcd\u{2026}");
        assert_eq!(truncate_ellipsis("abcdefgh", 5).chars().count(), 5);
    }

    #[test]
    fn truncate_is_char_boundary_safe_on_multibyte_text() {
        // Real title from the library this was tested against -- must not
        // panic by slicing mid-codepoint.
        let title = "友人A君を私の伴奏者に任命しま";
        let result = truncate_ellipsis(title, 5);
        assert_eq!(result.chars().count(), 5);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_max_zero_is_empty() {
        assert_eq!(truncate_ellipsis("anything", 0), "");
    }
}

pub enum LyricsState {
    Idle,
    /// Distinct from `Idle`: the Spotify Connect session ended
    /// unexpectedly (network drop, laptop sleep, etc.) after having been
    /// alive. There's no auto-reconnect yet, so this is a dead end --
    /// restart the process. Shown separately so a real drop is never
    /// mistaken for "just hasn't connected yet".
    SessionEnded,
    Loading,
    Synced(Vec<LyricLine>),
    Plain(String),
    Instrumental,
    NotFound,
}

pub struct AppState {
    pub track_title: Option<String>,
    pub track_artist: Option<String>,
    pub lyrics: LyricsState,
    pub current_line: Option<usize>,
    pub fullscreen: bool,
    pub context_lines: usize,
    /// `None` = no track loaded yet (device is connected regardless --
    /// this doesn't mean the Connect session is down). `Some(true)`
    /// = playing, `Some(false)` = paused. Distinguishing these explicitly
    /// (icon + frozen-vs-advancing gauge) is what closes the "is this
    /// broken or just paused" gap a real report ran into.
    pub playing: Option<bool>,
    pub position: Duration,
    pub duration: Duration,
    /// Raw librespot volume (0..=u16::MAX), updated from `VolumeChanged`
    /// events -- reflects changes from any source, not just our own
    /// up/down keys (e.g. adjusting it from the phone shows up here too).
    pub volume: u16,
    pub nav: Nav,
    pub sidebar_sel: usize,
    pub search: SearchState,
}

pub fn render(frame: &mut Frame, app: &AppState) {
    if app.fullscreen && *app.nav.top() == Screen::NowPlaying {
        render_fullscreen(frame, app);
        return;
    }

    let area = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(2), Constraint::Length(1)])
        .split(area);
    let (body_area, playbar_area, status_area) = (outer[0], outer[1], outer[2]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(1)])
        .split(body_area);
    let (sidebar_area, main_area) = (body[0], body[1]);

    render_sidebar(frame, app, sidebar_area);
    match app.nav.top() {
        Screen::Search => render_search(frame, app, main_area),
        Screen::NowPlaying => render_compact(frame, app, main_area),
    }
    render_playbar(frame, app, playbar_area);
    render_status(frame, app, status_area);
}

fn render_sidebar(frame: &mut Frame, app: &AppState, area: Rect) {
    let items: Vec<ListItem> = SIDEBAR_ENTRIES
        .iter()
        .enumerate()
        .map(|(i, (label, screen))| {
            let is_open = app.nav.depth() == 1 && app.nav.top() == screen;
            let is_cursor = app.nav.focus == Focus::Sidebar && i == app.sidebar_sel;
            let mut style = Style::default();
            if is_open {
                style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
            }
            if is_cursor {
                style = style.add_modifier(Modifier::REVERSED);
            }
            ListItem::new(*label).style(style)
        })
        .collect();
    let border_style = if app.nav.focus == Focus::Sidebar {
        Style::default().fg(ACCENT)
    } else {
        Style::default()
    };
    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::RIGHT).border_style(border_style)),
        area,
    );
}

fn render_playbar(frame: &mut Frame, app: &AppState, area: Rect) {
    let title_room = (area.width as usize).saturating_sub(28);
    let text = format!(
        "{} {}   {}   {}",
        playing_icon(app),
        header(app, title_room),
        time_readout(app),
        volume_readout(app),
    );
    frame.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_status(frame: &mut Frame, app: &AppState, area: Rect) {
    let text = format!("stack depth {} \u{2014} Tab switch pane, Esc back", app.nav.depth());
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(Color::DarkGray)), area);
}

fn render_search(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    let query_line = format!("/ {}\u{2588}", app.search.query); // trailing block = cursor
    frame.render_widget(
        Paragraph::new(query_line).block(Block::default().borders(Borders::ALL).title("search")),
        chunks[0],
    );

    let body = if app.search.searching {
        vec![Line::from("searching\u{2026}")]
    } else if let Some(err) = &app.search.error {
        vec![Line::from(format!("search failed: {err}"))]
    } else if app.search.results.is_empty() {
        vec![Line::from(if app.search.client_ready {
            "type a query, Enter to search, Esc to cancel".to_string()
        } else {
            "search not ready yet (loading Spotify auth\u{2026})".to_string()
        })]
    } else {
        vec![]
    };

    if !body.is_empty() {
        frame.render_widget(Paragraph::new(body), chunks[1]);
        return;
    }

    let items: Vec<ListItem> = app
        .search
        .results
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let text = format!("{} \u{2014} {} [{}]", t.artist, t.title, t.album);
            if i == app.search.selected {
                ListItem::new(text).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(text)
            }
        })
        .collect();
    frame.render_widget(List::new(items), chunks[1]);
}

fn header(app: &AppState, max_chars: usize) -> String {
    match (&app.track_artist, &app.track_title) {
        (Some(a), Some(t)) => truncate_ellipsis(&format!("{a} \u{2014} {t}"), max_chars),
        _ => "ready \u{2014} press / to search\u{2026}".to_string(),
    }
}

fn playing_icon(app: &AppState) -> &'static str {
    match app.playing {
        Some(true) => "\u{25b6}",  // ▶
        Some(false) => "\u{23f8}", // ⏸
        None => "\u{22ef}",        // ⋯ (no track loaded yet)
    }
}

fn time_readout(app: &AppState) -> String {
    format!("{} / {}", format_mmss(app.position), format_mmss(app.duration))
}

/// Plain text, not an emoji/icon: an emoji here would often be
/// double-width and throw off column alignment in the header row,
/// unlike the single-width play/pause glyphs used elsewhere.
fn volume_readout(app: &AppState) -> String {
    let percent = (app.volume as u32 * 100) / u16::MAX as u32;
    format!("vol {percent}%")
}

fn progress_ratio(app: &AppState) -> f64 {
    if app.duration.is_zero() {
        return 0.0;
    }
    (app.position.as_secs_f64() / app.duration.as_secs_f64()).clamp(0.0, 1.0)
}

fn progress_gauge(app: &AppState) -> Gauge<'static> {
    let color = if app.playing == Some(false) {
        Color::DarkGray // frozen/paused reads as visually "asleep"
    } else {
        ACCENT
    };
    Gauge::default()
        .gauge_style(Style::default().fg(color))
        .label("")
        .ratio(progress_ratio(app))
}

fn body_lines(app: &AppState) -> Vec<Line<'static>> {
    match &app.lyrics {
        LyricsState::Idle => vec![Line::from("ready \u{2014} press / to search\u{2026}")],
        LyricsState::SessionEnded => vec![
            Line::from("session disconnected"),
            Line::from("restart spot-tui to reconnect"),
        ],
        LyricsState::Loading => vec![Line::from("fetching lyrics\u{2026}")],
        LyricsState::Instrumental => vec![Line::from("\u{266a} instrumental")],
        LyricsState::NotFound => vec![Line::from("no lyrics found")],
        LyricsState::Plain(text) => {
            vec![Line::from("(unsynced)")]
                .into_iter()
                .chain(text.lines().map(|l| Line::from(l.to_string())))
                .collect()
        }
        LyricsState::Synced(lines) => {
            if lines.is_empty() {
                return vec![Line::from("no lyrics found")];
            }
            let current = app.current_line.unwrap_or(0);
            let start = current.saturating_sub(app.context_lines);
            let end = (current + app.context_lines + 1).min(lines.len());
            (start..end)
                .map(|i| {
                    let text = lines[i].text.clone();
                    let text = if text.is_empty() { "\u{266a}".to_string() } else { text };
                    if i == current {
                        Line::from(Span::styled(
                            text,
                            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                        ))
                    } else {
                        Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
                    }
                })
                .collect()
        }
    }
}

fn render_compact(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title/artist
            Constraint::Length(1), // icon + time
            Constraint::Length(1), // progress gauge
            Constraint::Min(1),    // lyrics
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(header(app, area.width as usize))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app))),
        chunks[1],
    );
    frame.render_widget(progress_gauge(app), chunks[2]);

    frame.render_widget(
        Paragraph::new(body_lines(app))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::TOP)),
        chunks[3],
    );
}

fn render_fullscreen(frame: &mut Frame, app: &AppState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title/artist
            Constraint::Length(1), // icon + time
            Constraint::Length(1), // progress gauge
            Constraint::Length(1), // spacer
            Constraint::Min(1),    // lyrics
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(header(app, area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
            .alignment(Alignment::Center),
        chunks[1],
    );
    frame.render_widget(progress_gauge(app), chunks[2]);

    let lines: Vec<Line> = body_lines(app)
        .into_iter()
        .map(|l| {
            Line::from(
                l.spans
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.add_modifier(Modifier::BOLD)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        chunks[4],
    );
}
