//! ratatui rendering: compact side-pane layout and fullscreen layout.

use crate::lyrics::LyricLine;
use crate::api::search::TrackResult;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Duration;

/// A screen in the main-pane stack. More variants land alongside the
/// phase that actually builds them (Queue in Phase 7, Devices in Phase
/// 8, Help in Phase 9), rather than stubbing out destinations nothing
/// can reach yet.
#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub enum Screen {
    NowPlaying,
    Search,
    Library,
    LikedSongs,
    SavedAlbums,
    FollowedArtists,
    YourPlaylists,
    PlaylistDetail,
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
pub const SIDEBAR_ENTRIES: &[(&str, Screen)] = &[
    ("Now Playing", Screen::NowPlaying),
    ("Search", Screen::Search),
    ("Library", Screen::Library),
];

/// One row of the sidebar -- either a static menu entry or one of the
/// user's own playlists. The sidebar was always meant to list playlists
/// directly (see the design-scope plan's Navigation model diagram) so
/// they're reachable in one step, not menu -> Library -> Your Playlists;
/// that part just hadn't been built yet.
pub enum SidebarRow<'a> {
    Menu(&'static str, Screen),
    Playlist(&'a crate::api::library::PlaylistSummary),
}

/// Combines the static menu entries with the user's playlists, pinned
/// ones first (same `pinned_first`/`filtered_sorted` used by the Your
/// Playlists screen -- one shared notion of pin order, not two). Used by
/// both rendering and key-handling so they can never disagree on what
/// row N actually is.
pub fn sidebar_rows(app: &AppState) -> Vec<SidebarRow<'_>> {
    let mut rows: Vec<SidebarRow> =
        SIDEBAR_ENTRIES.iter().map(|(label, screen)| SidebarRow::Menu(label, *screen)).collect();
    if let Fetch::Ready(items) = &app.library.playlists {
        let label = |p: &crate::api::library::PlaylistSummary| p.name.clone();
        let ordered = pinned_first(filtered_sorted(items, &ListFilter::default(), &label), &app.pinned_playlists, |p| {
            p.uri.as_str()
        });
        rows.extend(ordered.into_iter().map(|(_, p)| SidebarRow::Playlist(p)));
    }
    rows
}

/// The Library home screen's 4 entries. Not part of `SIDEBAR_ENTRIES` --
/// this is a menu one level into the main-pane stack, not a persistent
/// destination.
pub const LIBRARY_ENTRIES: &[(&str, Screen)] = &[
    ("Liked Songs", Screen::LikedSongs),
    ("Saved Albums", Screen::SavedAlbums),
    ("Followed Artists", Screen::FollowedArtists),
    ("Your Playlists", Screen::YourPlaylists),
];

/// State of one asynchronously-fetched list. Every Library list (Liked
/// Songs, Saved Albums, Followed Artists, Your Playlists) needs this
/// exact shape independently and simultaneously -- backing each with its
/// own loading/error bookkeeping avoided a real race a single shared
/// `loading` flag would have: navigating away from one still-loading
/// list into another would have made the second list's own fetch
/// wrongly believe one was already in flight.
pub enum Fetch<T> {
    NotStarted,
    Loading,
    Ready(T),
    Failed(String),
}

/// In-list filter/sort, shared by every list screen. Filtering and
/// sorting are presentation-only -- they never mutate the underlying
/// fetched `Vec`, so clearing the filter or toggling sort off always
/// returns to exactly what was originally fetched, no re-fetch needed.
#[derive(Default)]
pub struct ListFilter {
    pub query: String,
    pub editing: bool,
    pub sort_alpha: bool,
}

/// Applies `filter`'s query (case-insensitive substring match against
/// `label`) and, if `sort_alpha` is set, an alphabetical-by-label sort.
/// Returns references into `items` so this never clones or reorders the
/// canonical fetched data.
/// Returns `(original_index, item)` pairs, not just items -- callers that
/// need to tell Spotify "play index N of this context" (Playlist Detail)
/// must send the index into the *real, unfiltered* playlist, never the
/// display position. Losing the original index here was a real bug
/// caught before shipping: filtering down to a few matches and pressing
/// Enter would have told Spotify to play whatever sat at that position
/// in the full, unfiltered playlist instead.
pub fn filtered_sorted<'a, T>(
    items: &'a [T],
    filter: &ListFilter,
    label: &impl Fn(&T) -> String,
) -> Vec<(usize, &'a T)> {
    let mut result: Vec<(usize, &T)> = if filter.query.is_empty() {
        items.iter().enumerate().collect()
    } else {
        let q = filter.query.to_lowercase();
        items
            .iter()
            .enumerate()
            .filter(|(_, it)| label(it).to_lowercase().contains(&q))
            .collect()
    };
    if filter.sort_alpha {
        result.sort_by_key(|(_, a)| label(a).to_lowercase());
    }
    result
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    #[derive(Clone)]
    struct Item(&'static str);

    fn label(i: &Item) -> String {
        i.0.to_string()
    }

    fn items() -> Vec<Item> {
        vec![Item("banana"), Item("Apple"), Item("cherry")]
    }

    #[test]
    fn no_filter_no_sort_preserves_original_order() {
        let items = items();
        let filter = ListFilter::default();
        let result: Vec<&str> = filtered_sorted(&items, &filter, &label).iter().map(|(_, i)| i.0).collect();
        assert_eq!(result, vec!["banana", "Apple", "cherry"]);
    }

    #[test]
    fn query_matches_case_insensitively_as_substring() {
        let items = items();
        let filter = ListFilter {
            query: "an".to_string(),
            ..Default::default()
        };
        let result: Vec<&str> = filtered_sorted(&items, &filter, &label).iter().map(|(_, i)| i.0).collect();
        assert_eq!(result, vec!["banana"]);
    }

    #[test]
    fn query_matching_nothing_returns_empty() {
        let items = items();
        let filter = ListFilter {
            query: "zzz".to_string(),
            ..Default::default()
        };
        assert!(filtered_sorted(&items, &filter, &label).is_empty());
    }

    #[test]
    fn sort_alpha_sorts_case_insensitively() {
        let items = items();
        let filter = ListFilter {
            sort_alpha: true,
            ..Default::default()
        };
        let result: Vec<&str> = filtered_sorted(&items, &filter, &label).iter().map(|(_, i)| i.0).collect();
        assert_eq!(result, vec!["Apple", "banana", "cherry"]);
    }

    #[test]
    fn filter_and_sort_combine() {
        let items = vec![Item("Zebra"), Item("apricot"), Item("azalea"), Item("banana")];
        let filter = ListFilter {
            query: "a".to_string(),
            sort_alpha: true,
            ..Default::default()
        };
        let result: Vec<&str> = filtered_sorted(&items, &filter, &label).iter().map(|(_, i)| i.0).collect();
        assert_eq!(result, vec!["apricot", "azalea", "banana", "Zebra"]);
    }

    #[test]
    fn preserves_original_index_through_filter_and_sort() {
        // The whole point of returning (index, item) pairs: a caller
        // that needs to tell Spotify "play position N of the real
        // playlist" must use the ORIGINAL index, not the position in
        // this filtered/sorted display list. Real bug caught before
        // shipping -- filtering down to a match and pressing Enter would
        // have played whatever sat at the display position in the full,
        // unfiltered playlist instead.
        let items = items(); // ["banana"(0), "Apple"(1), "cherry"(2)]
        let filter = ListFilter {
            query: "a".to_string(), // matches banana(0) and Apple(1), not cherry
            sort_alpha: true,       // display order becomes Apple, banana
            ..Default::default()
        };
        let result = filtered_sorted(&items, &filter, &label);
        assert_eq!(result.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 0]);
    }
}

/// Pins bubble to the top, stable otherwise -- preserves whatever order
/// `filtered_sorted` already produced within the pinned and unpinned
/// groups. Generic over anything with a URI to check against `pinned`
/// (playlists via `sidebar_rows`/Your Playlists, and tracks within
/// Playlist Detail -- the second real caller that justified genericizing
/// this rather than hardcoding it to `PlaylistSummary`).
pub fn pinned_first<'a, T>(
    mut items: Vec<(usize, &'a T)>,
    pinned: &std::collections::HashSet<String>,
    uri_of: impl Fn(&T) -> &str,
) -> Vec<(usize, &'a T)> {
    items.sort_by_key(|(_, it)| !pinned.contains(uri_of(it)));
    items
}

pub struct LibraryState {
    pub home_selected: usize,
    pub liked_songs: Fetch<Vec<TrackResult>>,
    pub saved_albums: Fetch<Vec<crate::api::library::SavedAlbumSummary>>,
    pub followed_artists: Fetch<Vec<crate::api::library::FollowedArtist>>,
    pub playlists: Fetch<Vec<crate::api::library::PlaylistSummary>>,
    pub liked_songs_selected: usize,
    pub saved_albums_selected: usize,
    pub followed_artists_selected: usize,
    pub playlists_selected: usize,
    pub liked_songs_filter: ListFilter,
    pub saved_albums_filter: ListFilter,
    pub followed_artists_filter: ListFilter,
    pub playlists_filter: ListFilter,
}

impl LibraryState {
    pub fn new() -> Self {
        Self {
            home_selected: 0,
            liked_songs: Fetch::NotStarted,
            saved_albums: Fetch::NotStarted,
            followed_artists: Fetch::NotStarted,
            playlists: Fetch::NotStarted,
            liked_songs_selected: 0,
            saved_albums_selected: 0,
            followed_artists_selected: 0,
            playlists_selected: 0,
            liked_songs_filter: ListFilter::default(),
            saved_albums_filter: ListFilter::default(),
            followed_artists_filter: ListFilter::default(),
            playlists_filter: ListFilter::default(),
        }
    }
}

impl Default for LibraryState {
    fn default() -> Self {
        Self::new()
    }
}

/// Which playlist `Screen::PlaylistDetail` is currently showing, and its
/// fetch state. A single `Option` rather than a per-playlist cache --
/// only one can be on top of the stack at a time, matching `Nav`'s own
/// one-at-a-time `top()`. Fetch results carry the playlist's URI so a
/// stale in-flight fetch from a playlist the user has since backed out
/// of can't overwrite whichever one is showing now.
pub struct PlaylistDetailState {
    pub playlist: crate::api::library::PlaylistSummary,
    pub tracks: Fetch<Vec<TrackResult>>,
    pub selected: usize,
    pub filter: ListFilter,
}

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
    pub track_album: Option<String>,
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
    pub library: LibraryState,
    pub playlist_detail: Option<PlaylistDetailState>,
    pub pinned_playlists: std::collections::HashSet<String>,
    pub pinned_tracks: std::collections::HashSet<String>,
}

/// Persisted scroll offsets, one per list, threaded through `render`
/// alongside `&AppState` rather than living inside it. Rebuilding a fresh
/// `ListState` every frame (offset always 0) was the original, buggy
/// approach: ratatui's own "keep selection visible" clamp then re-pins
/// the highlight to the bottom edge of the viewport on *every* render
/// once you've scrolled past one screenful, regardless of which
/// direction you're actually moving -- reported live as "stays at the
/// bottom even when scrolling up." Persisting `ListState.offset` across
/// frames lets ratatui's real algorithm work as designed: the viewport
/// only moves once the selection would leave it, not on every keystroke.
/// A separate top-level struct (not new fields nested inside `AppState`)
/// because rendering needs to mutate exactly one of these at a time while
/// reading many *other* fields of `AppState` immutably in the same call --
/// nesting them inside `AppState` itself would fight the borrow checker
/// over a mutable borrow of one field colliding with an immutable borrow
/// of its parent struct.
#[derive(Default)]
pub struct ScrollState {
    pub sidebar: ListState,
    pub search: ListState,
    pub liked_songs: ListState,
    pub saved_albums: ListState,
    pub followed_artists: ListState,
    pub playlists: ListState,
    pub playlist_detail: ListState,
}

pub fn render(frame: &mut Frame, app: &AppState, scroll: &mut ScrollState) {
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

    render_sidebar(frame, app, &mut scroll.sidebar, sidebar_area);

    let main_block = Block::default()
        .borders(Borders::TOP)
        .border_style(focus_border_style(app.nav.focus == Focus::Main));
    let main_area = main_block.inner(main_area);
    frame.render_widget(main_block, body[1]);

    match app.nav.top() {
        Screen::Search => render_search(frame, app, &mut scroll.search, main_area),
        Screen::NowPlaying => render_compact(frame, app, main_area),
        Screen::Library => render_library_home(frame, app, main_area),
        Screen::LikedSongs => render_list_screen(
            frame,
            main_area,
            "Liked Songs",
            &app.library.liked_songs,
            &app.library.liked_songs_filter,
            app.library.liked_songs_selected,
            &mut scroll.liked_songs,
            |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title),
        ),
        Screen::SavedAlbums => render_list_screen(
            frame,
            main_area,
            "Saved Albums",
            &app.library.saved_albums,
            &app.library.saved_albums_filter,
            app.library.saved_albums_selected,
            &mut scroll.saved_albums,
            |a: &crate::api::library::SavedAlbumSummary| format!("{} \u{2014} {}", a.name, a.artist),
        ),
        Screen::FollowedArtists => render_list_screen(
            frame,
            main_area,
            "Followed Artists",
            &app.library.followed_artists,
            &app.library.followed_artists_filter,
            app.library.followed_artists_selected,
            &mut scroll.followed_artists,
            |a: &crate::api::library::FollowedArtist| a.name.clone(),
        ),
        Screen::YourPlaylists => render_your_playlists(frame, app, &mut scroll.playlists, main_area),
        Screen::PlaylistDetail => render_playlist_detail(frame, app, &mut scroll.playlist_detail, main_area),
    }
    render_playbar(frame, app, playbar_area);
    render_status(frame, app, status_area);
}

fn render_library_home(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new("Library").style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    let items: Vec<ListItem> = LIBRARY_ENTRIES
        .iter()
        .enumerate()
        .map(|(i, (label, _))| {
            if i == app.library.home_selected {
                ListItem::new(*label).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(*label)
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.library.home_selected));
    frame.render_stateful_widget(List::new(items), chunks[1], &mut state);
}

/// Not the shared `render_list_screen`, for the same reason Your
/// Playlists isn't: pinned tracks (spot-tui's own local-only substitute,
/// separate from pinned playlists -- see `pins.rs`) bubble to the top and
/// get a marker glyph, a concept the generic 4-screen renderer doesn't
/// know about. Reuses `pinned_first`/`filtered_sorted` exactly as Your
/// Playlists does, just keyed on `app.pinned_tracks` instead.
fn render_playlist_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(pd) = &app.playlist_detail else {
        frame.render_widget(Paragraph::new("no playlist selected"), area);
        return;
    };
    let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(filter_header(&pd.playlist.name, &pd.filter))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match &pd.tracks {
        Fetch::NotStarted | Fetch::Loading => {
            frame.render_widget(Paragraph::new("loading\u{2026}"), chunks[1]);
        }
        Fetch::Failed(e) => {
            frame.render_widget(Paragraph::new(format!("failed to load: {e}")), chunks[1]);
        }
        Fetch::Ready(items) => {
            let display = pinned_first(
                filtered_sorted(items, &pd.filter, &label),
                &app.pinned_tracks,
                |t| t.uri.as_str(),
            );
            let pin_label = |t: &TrackResult| {
                let marker = if app.pinned_tracks.contains(&t.uri) { "* " } else { "  " };
                format!("{marker}{}", label(t))
            };
            render_display_list(
                frame,
                chunks[1],
                &display,
                pd.selected,
                &pin_label,
                pd.filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// Your Playlists' own renderer, not the shared `render_list_screen`:
/// pinned playlists (spot-tui's own local-only substitute for Spotify's
/// pinning, which the public Web API doesn't expose at all) bubble to
/// the top and get a marker glyph -- a concept none of the other 4 list
/// screens have.
fn render_your_playlists(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let label =
        |p: &crate::api::library::PlaylistSummary| format!("{} ({} tracks)", p.name, p.track_count);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(filter_header("Your Playlists", &app.library.playlists_filter))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match &app.library.playlists {
        Fetch::NotStarted | Fetch::Loading => {
            frame.render_widget(Paragraph::new("loading\u{2026}"), chunks[1]);
        }
        Fetch::Failed(e) => {
            frame.render_widget(Paragraph::new(format!("failed to load: {e}")), chunks[1]);
        }
        Fetch::Ready(items) => {
            let display = pinned_first(
                filtered_sorted(items, &app.library.playlists_filter, &label),
                &app.pinned_playlists,
                |p| p.uri.as_str(),
            );
            let pin_label = |p: &crate::api::library::PlaylistSummary| {
                let marker = if app.pinned_playlists.contains(&p.uri) { "* " } else { "  " };
                format!("{marker}{}", label(p))
            };
            render_display_list(
                frame,
                chunks[1],
                &display,
                app.library.playlists_selected,
                &pin_label,
                app.library.playlists_filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// Renders one of the 4 uniform fetched-list screens (Liked Songs, Saved
/// Albums, Followed Artists, Playlist Detail tracks). Your Playlists gets
/// its own renderer instead -- it's the one list with an extra per-item
/// concept (pinning) this generic version has no notion of.
fn render_list_screen<T>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    fetch: &Fetch<Vec<T>>,
    filter: &ListFilter,
    selected: usize,
    list_state: &mut ListState,
    label: impl Fn(&T) -> String,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(filter_header(title, filter)).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    match fetch {
        Fetch::NotStarted | Fetch::Loading => {
            frame.render_widget(Paragraph::new("loading\u{2026}"), chunks[1]);
        }
        Fetch::Failed(e) => {
            frame.render_widget(Paragraph::new(format!("failed to load: {e}")), chunks[1]);
        }
        Fetch::Ready(items) => {
            let display = filtered_sorted(items, filter, &label);
            render_display_list(
                frame,
                chunks[1],
                &display,
                selected,
                &label,
                filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// `/` (start typing a filter) shows the live query with a cursor, same
/// convention as the global Search screen's own query line. Otherwise
/// shows whatever filter/sort is currently applied, if any.
fn filter_header(title: &str, filter: &ListFilter) -> String {
    if filter.editing {
        format!("{title}  /{}\u{2588}", filter.query)
    } else if !filter.query.is_empty() || filter.sort_alpha {
        let mut parts = Vec::new();
        if !filter.query.is_empty() {
            parts.push(format!("filter: \"{}\"", filter.query));
        }
        if filter.sort_alpha {
            parts.push("A-Z".to_string());
        }
        format!("{title}  ({})", parts.join(", "))
    } else {
        title.to_string()
    }
}

fn render_display_list<T>(
    frame: &mut Frame,
    area: Rect,
    display: &[(usize, &T)],
    selected: usize,
    label: &impl Fn(&T) -> String,
    filter_empty: bool,
    list_state: &mut ListState,
) {
    if display.is_empty() {
        let msg = if filter_empty { "(empty)" } else { "(no matches)" };
        frame.render_widget(Paragraph::new(msg), area);
        return;
    }
    let list_items: Vec<ListItem> = display
        .iter()
        .enumerate()
        .map(|(i, (_, it))| {
            let text = label(it);
            if i == selected {
                ListItem::new(text).style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(text)
            }
        })
        .collect();
    // Mutates just `.selected`, keeping whatever `.offset` this list_state
    // already had from the previous frame -- ratatui only moves the
    // offset if `selected` would otherwise fall outside the current
    // viewport, exactly the "only scroll at the edges" behavior a plain
    // `ListState::default()` (offset reset to 0 every frame) broke.
    list_state.select(Some(selected));
    frame.render_stateful_widget(List::new(list_items), area, list_state);
}

fn render_sidebar(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let rows = sidebar_rows(app);
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 1);
    let mut saw_playlists_header = false;
    for (i, row) in rows.iter().enumerate() {
        if matches!(row, SidebarRow::Playlist(_)) && !saw_playlists_header {
            items.push(ListItem::new("PLAYLISTS").style(Style::default().fg(Color::DarkGray)));
            saw_playlists_header = true;
        }
        // "Is this what's currently showing in Main" -- checked against
        // `top()` alone, not stack depth. `goto()` always keeps NowPlaying
        // at the bottom of the stack (see Nav::goto), so anything reached
        // via the Sidebar sits at depth 2, never depth 1 -- a lingering
        // `depth() == 1` check here (from before that invariant existed)
        // meant this could only ever light up for Now Playing itself,
        // reported live as no visible "which item am I in" indicator at
        // all once you'd navigated anywhere else.
        let (text, is_open) = match row {
            SidebarRow::Menu(label, screen) => (label.to_string(), app.nav.top() == screen),
            SidebarRow::Playlist(p) => {
                let marker = if app.pinned_playlists.contains(&p.uri) { "* " } else { "  " };
                let is_open = *app.nav.top() == Screen::PlaylistDetail
                    && app.playlist_detail.as_ref().is_some_and(|pd| pd.playlist.uri == p.uri);
                (format!("{marker}{}", p.name), is_open)
            }
        };
        let is_cursor = app.nav.focus == Focus::Sidebar && i == app.sidebar_sel;
        let mut style = Style::default();
        if is_open {
            style = style.fg(ACCENT).add_modifier(Modifier::BOLD);
        }
        if is_cursor {
            style = style.add_modifier(Modifier::REVERSED);
        }
        items.push(ListItem::new(text).style(style));
    }
    // The "PLAYLISTS" header takes up one visual row that `sidebar_sel`
    // (an index into logical rows: menu entries + playlists, no header)
    // doesn't know about -- shift the on-screen selection down by one
    // once the cursor is actually on a playlist row, past where the
    // header was inserted.
    let header_offset = if saw_playlists_header && app.sidebar_sel >= SIDEBAR_ENTRIES.len() { 1 } else { 0 };
    list_state.select(Some(app.sidebar_sel + header_offset));
    frame.render_stateful_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::RIGHT | Borders::TOP)
                .border_style(focus_border_style(app.nav.focus == Focus::Sidebar)),
        ),
        area,
        list_state,
    );
}

/// Accent when this pane currently has focus, dim otherwise -- always
/// present (never fully absent) so nothing changes size or jumps when
/// `Tab` toggles which pane it is. Only one pane is ever accented at a
/// time: the sidebar's vertical divider previously changing color was
/// easy to miss as the sole focus cue; this gives Main pane an equally
/// visible signal of its own, reported live as missing entirely.
fn focus_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    }
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

fn render_search(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
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
        vec![Line::from(if !app.search.client_ready {
            "search not ready yet (loading Spotify auth\u{2026})".to_string()
        } else if app.search.query.is_empty() {
            "type a query, then Enter to search, Esc to cancel".to_string()
        } else {
            // Distinct from the empty-query message on purpose: this is
            // the state reported live as "have to click enter first and
            // then scroll" -- clarifying *why* up/down do nothing yet
            // (there's a real Web API call to make, not a local list to
            // narrow) rather than leaving it looking broken or identical
            // to having typed nothing at all.
            "press Enter to search \u{2014} this hits Spotify directly, not a live filter like Library's /".to_string()
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
    list_state.select(Some(app.search.selected));
    frame.render_stateful_widget(List::new(items), chunks[1], list_state);
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

const ART_MIN_WIDTH: u16 = 14;
const ART_MAX_WIDTH: u16 = 26;

fn hash_bytes(s: &str) -> u32 {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    h
}

/// A deterministic placeholder for real album art (the design-scope
/// plan's own Non-goal: real bitmap art needs the `ratatui-image` crate
/// plus a terminal graphics protocol, tracked but not scheduled).
/// `Color::Indexed`, not `Rgb`, matching `ACCENT`'s own choice above --
/// renders correctly on plain 256-color terminals, not just truecolor
/// ones. Kept to the middle of the 6-step color cube's range (1..=4 per
/// channel, out of 0..=5) so it reads as "colorful art," not a
/// near-black or near-white cube corner that would wash out the
/// monogram text sitting on top of it.
fn art_color(artist: &str, album: &str) -> Color {
    let h = hash_bytes(&format!("{artist}{album}"));
    let r = 1 + (h % 4) as u16;
    let g = 1 + ((h / 4) % 4) as u16;
    let b = 1 + ((h / 16) % 4) as u16;
    Color::Indexed((16 + 36 * r + 6 * g + b) as u8)
}

fn monogram(artist: &str, album: &str) -> String {
    let first_upper = |s: &str| s.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    format!("{}{}", first_upper(artist), first_upper(album))
}

fn render_art_block(frame: &mut Frame, artist: &str, album: &str, area: Rect) {
    let bg = art_color(artist, album);
    frame.render_widget(Block::default().style(Style::default().bg(bg)), area);
    if area.height == 0 || area.width == 0 {
        return;
    }
    let text_style = Style::default().bg(bg).fg(Color::White).add_modifier(Modifier::BOLD);
    let top_pad = area.height / 2;
    let mut lines: Vec<Line> = (0..top_pad).map(|_| Line::from("")).collect();
    lines.push(Line::from(monogram(artist, album)));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center).style(text_style), area);
}

fn render_compact(frame: &mut Frame, app: &AppState, area: Rect) {
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_now_playing_hero(frame, app, artist, title, area),
        _ => render_now_playing_idle(frame, app, area),
    }
}

/// The unglamorous state, designed on its own terms rather than as a
/// stripped-down hero: nothing is loaded yet, so there's nothing to
/// depict art for -- showing a colorful block anyway would be a lie
/// about there being a track, not a placeholder for one.
fn render_now_playing_idle(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(header(app, area.width as usize)).style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(Paragraph::new(body_lines(app)).wrap(Wrap { trim: true }), chunks[2]);
}

/// Art block + larger title/transport on one row, lyrics given real room
/// below -- the hero treatment validated in the browser mockup, ported
/// into the real terminal for the first time. `hero_height` scales with
/// the pane but stays capped: this is a glance screen, not the whole
/// app, and lyrics still need to be the dominant use of vertical space
/// (calibrating density to what this screen is actually for, not
/// maximizing decoration).
fn render_now_playing_hero(frame: &mut Frame, app: &AppState, artist: &str, title: &str, area: Rect) {
    let hero_height = (area.height / 2).clamp(6, 11).min(area.height);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(hero_height), Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    // Below ART_MIN_WIDTH the block would crush the monogram illegibly --
    // in a narrow pane, skip the art entirely rather than render
    // something unreadable just to say there's art.
    let art_width = (area.width / 4).clamp(ART_MIN_WIDTH, ART_MAX_WIDTH);
    let show_art = area.width >= art_width + 24;

    let hero_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_art {
            vec![Constraint::Length(art_width), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1)]
        })
        .split(outer[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    let meta_area = if show_art {
        render_art_block(frame, artist, album, hero_cols[0]);
        hero_cols[1]
    } else {
        hero_cols[0]
    };

    let meta_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // spacer
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(1), // gauge
        ])
        .split(meta_area);

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, meta_area.width as usize))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        meta_chunks[1],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, meta_area.width as usize))
            .style(Style::default().fg(Color::DarkGray)),
        meta_chunks[2],
    );
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app))),
        meta_chunks[4],
    );
    frame.render_widget(progress_gauge(app), meta_chunks[5]);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    frame.render_widget(Paragraph::new(body_lines(app)).wrap(Wrap { trim: true }), outer[2]);
}

fn render_fullscreen(frame: &mut Frame, app: &AppState) {
    let area = frame.area();
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_fullscreen_hero(frame, app, artist, title, area),
        _ => render_fullscreen_idle(frame, app, area),
    }
}

/// Same reasoning as `render_now_playing_idle`: nothing loaded, nothing
/// to depict art for.
fn render_fullscreen_idle(frame: &mut Frame, app: &AppState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(header(app, area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[0],
    );
    frame.render_widget(
        Paragraph::new(body_lines(app)).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        chunks[1],
    );
}

/// The most immersive treatment: a large, centered art block above the
/// same bold-centered title/transport/lyrics fullscreen already had.
fn render_fullscreen_hero(frame: &mut Frame, app: &AppState, artist: &str, title: &str, area: Rect) {
    let art_height = (area.height / 3).clamp(8, 16);
    let art_width = (art_height * 2).clamp(20, 44);
    let show_art = area.height > art_height + 8 && area.width > art_width + 4;

    let text_rows = [
        Constraint::Length(1), // title
        Constraint::Length(1), // icon + time + vol
        Constraint::Length(1), // progress gauge
        Constraint::Length(1), // spacer
        Constraint::Min(1),    // lyrics
    ];
    let chunks = if show_art {
        let mut c = vec![Constraint::Length(art_height), Constraint::Length(1)];
        c.extend(text_rows);
        Layout::default().direction(Direction::Vertical).constraints(c).split(area)
    } else {
        Layout::default().direction(Direction::Vertical).constraints(text_rows).split(area)
    };
    let base = if show_art { 2 } else { 0 };

    if show_art {
        let album = app.track_album.as_deref().unwrap_or(title);
        let art_row = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(art_width), Constraint::Min(1)])
            .split(chunks[0]);
        render_art_block(frame, artist, album, art_row[1]);
    }

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        chunks[base],
    );
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
            .alignment(Alignment::Center),
        chunks[base + 1],
    );
    frame.render_widget(progress_gauge(app), chunks[base + 2]);

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
        Paragraph::new(lines).alignment(Alignment::Center).wrap(Wrap { trim: true }),
        chunks[base + 4],
    );
}
