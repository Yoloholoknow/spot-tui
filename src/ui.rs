//! ratatui rendering: compact side-pane layout and fullscreen layout.

use crate::lyrics::LyricLine;
use crate::api::search::TrackResult;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Wrap};
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
    Help,
    Queue,
    Devices,
    ArtistDetail,
    AlbumDetail,
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
    ("Liked Songs", Screen::LikedSongs),
    ("Queue", Screen::Queue),
    ("Devices", Screen::Devices),
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
    /// Character position within `query`, same convention as
    /// `SearchState::cursor` -- Left/Right move it mid-string. Two other
    /// designs were tried and rejected live: Left/Right as pane
    /// navigation (nav.escape()/open) while typing felt like it silently
    /// kicked you out of the filter; a no-op felt like the arrows were
    /// just broken. Real in-text cursor movement, matching Search, is
    /// the one that actually reads as "working."
    pub cursor: usize,
    pub editing: bool,
    pub sort_alpha: bool,
}

impl ListFilter {
    /// Enters edit mode with the cursor placed after whatever query text
    /// is already there (matching a normal text field regaining focus),
    /// not reset to the start.
    pub fn start_editing(&mut self) {
        self.editing = true;
        self.cursor = self.query.chars().count();
    }

    /// Exits edit mode AND clears the query -- distinct from just setting
    /// `editing = false` (which keeps whatever was typed applied).
    /// Reported live: Esc while filtering only stopped editing, leaving
    /// the narrowed view in place with no way to actually cancel back to
    /// the full list short of backspacing everything by hand.
    pub fn cancel_editing(&mut self) {
        self.editing = false;
        self.query.clear();
        self.cursor = 0;
    }

    pub fn insert_at_cursor(&mut self, c: char) {
        text_insert_at_cursor(&mut self.query, &mut self.cursor, c);
    }

    pub fn backspace_at_cursor(&mut self) {
        text_backspace_at_cursor(&mut self.query, &mut self.cursor);
    }

    pub fn cursor_left(&mut self) {
        text_cursor_left(&mut self.cursor);
    }

    pub fn cursor_right(&mut self) {
        text_cursor_right(&self.query, &mut self.cursor);
    }
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

/// Playlist reorder move-mode (Phase 6): `Up`/`Down` relocate the
/// selected item one slot at a time, entirely locally -- no network call
/// per keystroke, only once on confirm. Returns the item's new selected
/// index (unchanged, a no-op, at either end of the list).
pub fn move_item_up<T>(items: &mut [T], selected: usize) -> usize {
    if selected == 0 {
        return selected;
    }
    items.swap(selected, selected - 1);
    selected - 1
}

pub fn move_item_down<T>(items: &mut [T], selected: usize) -> usize {
    if selected + 1 >= items.len() {
        return selected;
    }
    items.swap(selected, selected + 1);
    selected + 1
}

/// Repeat as the three states a person actually cycles through (off /
/// the whole album or playlist / this one song), collapsed from the
/// player's two independent booleans (`repeating_context`,
/// `repeating_track`) -- four flag combinations, but only three of them
/// mean anything different to a listener.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepeatMode {
    #[default]
    Off,
    Context,
    Track,
}

impl RepeatMode {
    /// The track flag wins: repeat-one is on whenever it's set, whether or
    /// not the context flag came along with it.
    pub fn from_flags(context: bool, track: bool) -> Self {
        if track {
            RepeatMode::Track
        } else if context {
            RepeatMode::Context
        } else {
            RepeatMode::Off
        }
    }

    pub fn next(self) -> Self {
        match self {
            RepeatMode::Off => RepeatMode::Context,
            RepeatMode::Context => RepeatMode::Track,
            RepeatMode::Track => RepeatMode::Off,
        }
    }

    /// `(repeat context, repeat track)`. Repeat-one sets both, matching how
    /// Spotify's own clients report it.
    pub fn flags(self) -> (bool, bool) {
        match self {
            RepeatMode::Off => (false, false),
            RepeatMode::Context => (true, false),
            RepeatMode::Track => (true, true),
        }
    }

    pub fn status_label(self) -> &'static str {
        match self {
            RepeatMode::Off => "off",
            RepeatMode::Context => "album/playlist",
            RepeatMode::Track => "this song",
        }
    }
}

/// The three states the `z` key cycles through, mirroring Spotify's own
/// shuffle button: off, shuffle, smart shuffle. Smart shuffle is shuffle plus
/// a `context_enhancement` mode, so it always implies shuffle is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShuffleMode {
    #[default]
    Off,
    On,
    Smart,
}

impl ShuffleMode {
    pub fn from_flags(shuffle: bool, smart: bool) -> Self {
        match (shuffle, smart) {
            (false, _) => Self::Off,
            (true, false) => Self::On,
            (true, true) => Self::Smart,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::On,
            Self::On => Self::Smart,
            Self::Smart => Self::Off,
        }
    }

    pub fn shuffle(self) -> bool {
        self != Self::Off
    }

    /// Status-bar text after switching to this mode. Smart shuffle says
    /// plainly what spot-tui can't do: the recommended tracks Spotify's own
    /// apps mix in are not added when spot-tui is the playing device.
    pub fn status_label(self) -> &'static str {
        match self {
            Self::Off => "shuffle off",
            Self::On => "shuffle on",
            Self::Smart => "smart shuffle on (no recommended songs are added from spot-tui)",
        }
    }
}

/// The playbar's always-visible shuffle and repeat toggles, as Spotify
/// shows them: every glyph is drawn in every state, and the `bool` says
/// whether that one is currently on (accent) or off (dim) -- the caller
/// does the coloring. Repeat's slot is a fixed two cells wide (`↻ ` for off
/// and album/playlist, `↻1` for this song) so the readout never changes
/// width and the track title's truncation point doesn't jump around as
/// modes change. Smart shuffle is a third shuffle state, not a separate
/// toggle, so its sparkle sits in the one-cell gap between the two toggles
/// (a blank when off) instead of widening the readout; it only lights while
/// shuffle itself is on.
pub fn playback_modes(shuffle: bool, smart: bool, repeat: RepeatMode) -> [(&'static str, bool); 3] {
    let repeat_glyph = match repeat {
        RepeatMode::Track => "\u{21bb}1",
        RepeatMode::Off | RepeatMode::Context => "\u{21bb} ",
    };
    let smart = shuffle && smart;
    [
        ("\u{21c4}", shuffle),
        (if smart { "\u{2726}" } else { " " }, smart),
        (repeat_glyph, repeat != RepeatMode::Off),
    ]
}

/// Cells `playback_modes` always occupies: shuffle (1), the smart-shuffle
/// gap (1), repeat (2).
const PLAYBACK_MODES_WIDTH: usize = 4;

/// Validates a typed 1-based "move to position" answer against a list of
/// `len` items and returns the 0-based index to splice to. Positions are
/// 1-based on purpose -- that's how the list reads on screen and how a
/// person counts -- so the range in the error message is too. Whole,
/// positive integers only: no signs, decimals, or embedded spaces.
pub fn parse_move_position(input: &str, len: usize) -> Result<usize, String> {
    let range_error = || format!("enter a position from 1 to {len}");
    let position: usize = input.trim().parse().map_err(|_| range_error())?;
    if position == 0 || position > len {
        return Err(range_error());
    }
    Ok(position - 1)
}

#[cfg(test)]
mod parse_move_position_tests {
    use super::*;

    #[test]
    fn a_one_based_position_becomes_a_zero_based_index() {
        assert_eq!(parse_move_position("3", 5), Ok(2));
    }

    #[test]
    fn both_ends_of_the_range_are_valid() {
        assert_eq!(parse_move_position("1", 5), Ok(0));
        assert_eq!(parse_move_position("5", 5), Ok(4));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(parse_move_position("  4 ", 5), Ok(3));
    }

    #[test]
    fn zero_and_past_the_end_are_rejected_with_the_real_range() {
        assert_eq!(parse_move_position("0", 5), Err("enter a position from 1 to 5".to_string()));
        assert_eq!(parse_move_position("6", 5), Err("enter a position from 1 to 5".to_string()));
    }

    #[test]
    fn non_numbers_are_rejected() {
        for bad in ["", "abc", "-2", "2.5", "1 2"] {
            assert!(parse_move_position(bad, 5).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn an_empty_list_has_no_valid_position() {
        assert!(parse_move_position("1", 0).is_err());
    }
}

/// Walks the item at `selected` back to `target` one adjacent swap at a
/// time. Used to cancel move-mode (`Esc`): since only one item has
/// actually been relocated -- an insertion-sort-style move, not
/// independent per-item swaps -- this exactly restores the original
/// arrangement without needing a full snapshot of the list to revert to.
pub fn move_item_to<T>(items: &mut [T], mut selected: usize, target: usize) -> usize {
    while selected > target {
        selected = move_item_up(items, selected);
    }
    while selected < target {
        selected = move_item_down(items, selected);
    }
    selected
}

#[cfg(test)]
mod move_item_tests {
    use super::*;

    #[test]
    fn move_up_swaps_with_the_previous_slot() {
        let mut items = vec!['a', 'b', 'c'];
        let selected = move_item_up(&mut items, 1);
        assert_eq!(items, vec!['b', 'a', 'c']);
        assert_eq!(selected, 0);
    }

    #[test]
    fn move_up_at_the_top_is_a_noop() {
        let mut items = vec!['a', 'b', 'c'];
        let selected = move_item_up(&mut items, 0);
        assert_eq!(items, vec!['a', 'b', 'c']);
        assert_eq!(selected, 0);
    }

    #[test]
    fn move_down_swaps_with_the_next_slot() {
        let mut items = vec!['a', 'b', 'c'];
        let selected = move_item_down(&mut items, 1);
        assert_eq!(items, vec!['a', 'c', 'b']);
        assert_eq!(selected, 2);
    }

    #[test]
    fn move_down_at_the_bottom_is_a_noop() {
        let mut items = vec!['a', 'b', 'c'];
        let selected = move_item_down(&mut items, 2);
        assert_eq!(items, vec!['a', 'b', 'c']);
        assert_eq!(selected, 2);
    }

    #[test]
    fn move_item_to_walks_down_to_a_later_target() {
        let mut items = vec!['a', 'b', 'c', 'd'];
        let selected = move_item_to(&mut items, 0, 2);
        assert_eq!(items, vec!['b', 'c', 'a', 'd']);
        assert_eq!(selected, 2);
    }

    #[test]
    fn move_item_to_walks_up_to_an_earlier_target() {
        let mut items = vec!['a', 'b', 'c', 'd'];
        let selected = move_item_to(&mut items, 3, 1);
        assert_eq!(items, vec!['a', 'd', 'b', 'c']);
        assert_eq!(selected, 1);
    }

    #[test]
    fn round_trip_through_move_item_to_restores_the_original_order() {
        // This is the actual cancel-move-mode use case: move an item
        // partway, then walk it straight back to where it started.
        let mut items = vec!['a', 'b', 'c', 'd', 'e'];
        let original = items.clone();
        let start = 1;
        let mut selected = start;
        selected = move_item_down(&mut items, selected);
        selected = move_item_down(&mut items, selected);
        assert_ne!(items, original);
        let selected = move_item_to(&mut items, selected, start);
        assert_eq!(items, original);
        assert_eq!(selected, start);
    }
}

/// The Connect queue (Phase 7). Kept separate from `LibraryState` --
/// unlike Liked Songs/Saved Albums/etc, this reflects live playback
/// state that changes on its own even when this app hasn't done
/// anything (the current track finishes, another device skips ahead),
/// so it's periodically refetched while visible rather than fetched once
/// and cached indefinitely -- see `main.rs`'s own polling logic.
pub struct QueueState {
    pub fetch: Fetch<crate::api::queue::QueueSummary>,
    pub selected: usize,
}

impl QueueState {
    pub fn new() -> Self {
        Self { fetch: Fetch::NotStarted, selected: 0 }
    }
}

impl Default for QueueState {
    fn default() -> Self {
        Self::new()
    }
}

/// Connect devices (Phase 8). Fetched once on entry (like the Library
/// lists), not periodically like `QueueState` -- a device coming online
/// or offline is a discrete, comparatively rare event, not something
/// changing every few seconds during normal use. `r` refetches manually.
pub struct DevicesState {
    pub fetch: Fetch<Vec<crate::api::devices::DeviceSummary>>,
    pub selected: usize,
}

impl DevicesState {
    pub fn new() -> Self {
        Self { fetch: Fetch::NotStarted, selected: 0 }
    }
}

impl Default for DevicesState {
    fn default() -> Self {
        Self::new()
    }
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
    /// `Some(start_index)` while move-mode (Phase 6, `m`) is active --
    /// the index the moving track started at, so confirming (`Enter`)
    /// knows the net displacement regardless of how many times it moved
    /// up and down in between.
    pub move_mode: Option<usize>,
}

/// Phase 9, read-only, no filter/sort/pin concept -- just enough state
/// to show one artist's albums and remember which real artist this is,
/// so a stale in-flight fetch from an artist backed out of can't
/// overwrite whichever one is showing now (same guard idiom
/// `PlaylistDetailState` already uses).
pub struct ArtistDetailState {
    pub artist_uri: String,
    pub detail: Fetch<crate::api::artist::ArtistDetail>,
    pub selected: usize,
}

pub struct AlbumDetailState {
    pub album_uri: String,
    pub detail: Fetch<crate::api::album::AlbumDetail>,
    pub selected: usize,
}

// Phase 5's transient overlays (name prompt, yes/no confirm, playlist
// picker) live as sibling `Option<_>` fields on `AppState` rather than
// new `Screen` stack variants -- `Screen::Help`'s own addition (the most
// recent precedent) touched 8+ separate call sites (the exhaustive
// `render` match, the exhaustive Main-focus `match key.code`, and a
// `nav.push` binding hand-added to every one of 8 screens individually,
// each re-implementing the global keys by hand since there's no shared
// fallthrough). None of that is right for something transient anyway --
// a yes/no confirm isn't a destination with its own `Esc`-back semantics,
// it's a gate on top of wherever the user already was. One interception
// point at the very top of the key loop (same place `Tab` is already
// intercepted) and one draw call at the end of `render` covers all
// three, and the screen underneath is untouched -- its list position,
// filter, nav depth all just resume once the overlay closes.

/// A single-line text prompt (playlist name, for now). The third caller
/// of the shared `text_*` cursor functions, after `SearchState` and
/// `ListFilter` -- not yet a big enough win to unify all three into one
/// shared struct, but that stays a documented option rather than a
/// to-do.
pub struct TextPrompt {
    pub title: String,
    pub query: String,
    pub cursor: usize,
    pub action: TextPromptAction,
}

pub enum TextPromptAction {
    CreatePlaylist,
    RenamePlaylist(crate::api::library::PlaylistSummary),
    /// Playlist Detail's move-mode `g`: a purely local splice, no network
    /// call -- unlike the other two, this needs no Spotify client at all.
    MoveToPosition,
}

impl TextPrompt {
    /// `initial` pre-seeds the field (rename needs the current name) with
    /// the cursor placed after it, matching a normal text field regaining
    /// focus -- an empty `initial` (create) just starts at 0, same thing.
    pub fn new(title: impl Into<String>, initial: impl Into<String>, action: TextPromptAction) -> Self {
        let query: String = initial.into();
        let cursor = query.chars().count();
        Self { title: title.into(), query, cursor, action }
    }

    pub fn insert_at_cursor(&mut self, c: char) {
        text_insert_at_cursor(&mut self.query, &mut self.cursor, c);
    }

    pub fn backspace_at_cursor(&mut self) {
        text_backspace_at_cursor(&mut self.query, &mut self.cursor);
    }

    pub fn cursor_left(&mut self) {
        text_cursor_left(&mut self.cursor);
    }

    pub fn cursor_right(&mut self) {
        text_cursor_right(&self.query, &mut self.cursor);
    }
}

/// A yes/no gate in front of a destructive action -- `d` always routes
/// through this, no exceptions, matching the plan's own standing rule.
pub struct PendingConfirm {
    pub message: String,
    pub action: ConfirmAction,
}

pub enum ConfirmAction {
    DeletePlaylist(crate::api::library::PlaylistSummary),
    RemoveTrack { playlist_uri: String, track_uri: String, occurrences: usize },
    /// Confirmed past the "this playlist already has this track" warning
    /// -- adds it anyway, the exact same call `TrackAdded`'s normal path
    /// uses, just reached from the confirm overlay instead of directly.
    AddTrackAnyway { playlist_uri: String, track_uri: String },
    /// `q`, when `Config::confirm_quit` is on -- the one confirm action
    /// that doesn't mutate anything, just tells the main loop to actually
    /// exit once confirmed.
    Quit,
    // Reported live: liking/following/saving fires immediately (matches
    // `a` = add-to-playlist, which never confirms either), but the
    // reverse -- unlike/unfollow/unsave, all reached from the screen
    // that *is* the owning list -- confirms first, same standing rule
    // "d"/"Shift+D" already established for anything that removes an
    // item from a list you're looking straight at.
    UnlikeTrack { track_uri: String },
    UnfollowArtist { artist_uri: String },
    UnsaveAlbum { album_uri: String },
}

/// Derived from the variant rather than stored as its own field on
/// `PendingConfirm` -- severity is a deterministic fact about *which*
/// action this is, not independent state, so there's nothing to keep in
/// sync at each of the 4 construction call sites.
enum ConfirmSeverity {
    Danger,
    Warn,
    Neutral,
}

impl ConfirmAction {
    fn severity(&self) -> ConfirmSeverity {
        match self {
            ConfirmAction::DeletePlaylist(_) | ConfirmAction::RemoveTrack { .. } => ConfirmSeverity::Danger,
            // Warn, not Danger -- unlike RemoveTrack/DeletePlaylist,
            // undoing any of these is one more keypress away (like/
            // follow/save again), not a real, harder-to-recover loss.
            ConfirmAction::AddTrackAnyway { .. }
            | ConfirmAction::UnlikeTrack { .. }
            | ConfirmAction::UnfollowArtist { .. }
            | ConfirmAction::UnsaveAlbum { .. } => ConfirmSeverity::Warn,
            ConfirmAction::Quit => ConfirmSeverity::Neutral,
        }
    }
}

/// The add-to-playlist picker (`a`). Lists `app.library.playlists`,
/// pinned-first -- same ordering Your Playlists and the Sidebar already
/// use. No in-picker filter/sort in this pass; the list is short enough
/// that it isn't missed yet (see the design-scope plan's Phase 5
/// non-goals).
pub struct PlaylistPicker {
    pub track_uri: String,
    pub selected: usize,
    /// Always live -- unlike the list screens' `/`-to-start-editing
    /// convention, the picker has no other letter-key action competing
    /// for space, so every printable character narrows it immediately,
    /// no explicit "start editing" step needed. `editing`/`sort_alpha`
    /// go unused here; reusing `ListFilter` wholesale (rather than a
    /// bespoke query+cursor pair) is what gets `filtered_sorted` for
    /// free. Reported live as needed once a real account had enough
    /// playlists that scrolling to find one by hand was real friction --
    /// originally scoped out on the assumption the list would stay
    /// short.
    pub filter: ListFilter,
}

/// Phase 12: the global quick-jump palette (`Ctrl+P`). Flattens playlists,
/// liked tracks, followed artists, saved albums, devices, and every
/// fixed nav destination into one searchable list. No stored/memoized
/// entry list here, deliberately -- like `PlaylistPicker`, `selected`
/// and `filter` are the only state; the actual entry pool is recomputed
/// live from `AppState` on every keystroke (`quick_jump_entries`), so a
/// background fetch completing while this is open is picked up for free
/// on the very next render with no invalidation logic to get wrong.
pub struct QuickJump {
    pub filter: ListFilter,
    pub selected: usize,
}

/// What a quick-jump entry activates. Carries the whole item (not just a
/// URI) so activation never needs a second lookup back into `AppState`
/// after the overlay that found it has already closed.
#[derive(Clone)]
pub enum QuickJumpKind {
    Screen(Screen),
    Playlist(crate::api::library::PlaylistSummary),
    Track(TrackResult),
    Artist { uri: String },
    Album { uri: String },
    Device(crate::api::devices::DeviceSummary),
}

#[derive(Clone)]
pub struct QuickJumpEntry {
    /// Category-prefixed display+search text (e.g. "[Playlist] Chill
    /// vibes") -- the prefix keeps a mixed-kind list scannable and does
    /// not interfere with substring matching against the real name.
    pub label: String,
    pub kind: QuickJumpKind,
}

/// Every fixed nav destination a "place to go" -- every fieldless
/// `Screen` variant that isn't itself a drill-down target reached only
/// via a specific track/artist/album/playlist.
const QUICK_JUMP_SCREENS: &[(&str, Screen)] = &[
    ("Now Playing", Screen::NowPlaying),
    ("Search", Screen::Search),
    ("Library", Screen::Library),
    ("Liked Songs", Screen::LikedSongs),
    ("Saved Albums", Screen::SavedAlbums),
    ("Followed Artists", Screen::FollowedArtists),
    ("Your Playlists", Screen::YourPlaylists),
    ("Queue", Screen::Queue),
    ("Devices", Screen::Devices),
    ("Help", Screen::Help),
];

/// Builds the flattened, filterable pool quick jump searches. Deliberately
/// category-ordered (screens, then playlists, artists, albums, devices,
/// tracks last) rather than scored -- there's no numeric relevance score
/// with plain substring matching, so build order *is* the display order.
/// When `filter.query` is empty, returns only the 10 fixed screen
/// entries and skips building the dynamic categories entirely -- cheap
/// by construction the instant the overlay opens, not just capped at
/// display time; the dynamic pool (which can be hundreds to thousands of
/// items on a real account) is only ever built once the user has actually
/// started typing.
pub fn quick_jump_entries(app: &AppState, filter: &ListFilter) -> Vec<QuickJumpEntry> {
    let mut entries: Vec<QuickJumpEntry> = QUICK_JUMP_SCREENS
        .iter()
        .map(|(label, screen)| QuickJumpEntry { label: format!("[Go] {label}"), kind: QuickJumpKind::Screen(*screen) })
        .collect();
    if filter.query.is_empty() {
        return entries;
    }
    if let Fetch::Ready(items) = &app.library.playlists {
        entries.extend(
            items
                .iter()
                .map(|p| QuickJumpEntry { label: format!("[Playlist] {}", p.name), kind: QuickJumpKind::Playlist(p.clone()) }),
        );
    }
    if let Fetch::Ready(items) = &app.library.followed_artists {
        entries.extend(items.iter().map(|a| QuickJumpEntry {
            label: format!("[Artist] {}", a.name),
            kind: QuickJumpKind::Artist { uri: a.uri.clone() },
        }));
    }
    if let Fetch::Ready(items) = &app.library.saved_albums {
        entries.extend(items.iter().map(|a| QuickJumpEntry {
            label: format!("[Album] {} \u{2014} {}", a.name, a.artist),
            kind: QuickJumpKind::Album { uri: a.uri.clone() },
        }));
    }
    if let Fetch::Ready(items) = &app.devices.fetch {
        entries.extend(
            items.iter().map(|d| QuickJumpEntry { label: format!("[Device] {}", d.name), kind: QuickJumpKind::Device(d.clone()) }),
        );
    }
    if let Fetch::Ready(items) = &app.library.liked_songs {
        entries.extend(items.iter().map(|t| QuickJumpEntry {
            label: format!("[Track] {} \u{2014} {}", t.artist, t.title),
            kind: QuickJumpKind::Track(t.clone()),
        }));
    }
    entries
}

#[cfg(test)]
mod text_prompt_tests {
    use super::*;

    #[test]
    fn new_with_empty_initial_starts_at_cursor_zero() {
        let p = TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist);
        assert_eq!(p.query, "");
        assert_eq!(p.cursor, 0);
    }

    #[test]
    fn new_with_an_initial_value_places_cursor_after_it() {
        // Rename pre-seeds the field with the current name -- the cursor
        // should land at the end, matching a normal text field regaining
        // focus, not reset to the start.
        let p = TextPrompt::new(
            "Rename playlist",
            "old name",
            TextPromptAction::RenamePlaylist(crate::api::library::PlaylistSummary {
                uri: "spotify:playlist:x".to_string(),
                name: "old name".to_string(),
                track_count: 3,
            }),
        );
        assert_eq!(p.query, "old name");
        assert_eq!(p.cursor, 8);
    }

    #[test]
    fn insert_backspace_and_arrows_operate_at_the_cursor() {
        let mut p = TextPrompt::new("New playlist name", "", TextPromptAction::CreatePlaylist);
        p.insert_at_cursor('a');
        p.insert_at_cursor('c');
        p.cursor_left();
        p.insert_at_cursor('b');
        assert_eq!(p.query, "abc");
        assert_eq!(p.cursor, 2);
        p.cursor_right();
        assert_eq!(p.cursor, 3); // clamped at the end
        p.backspace_at_cursor();
        assert_eq!(p.query, "ab");
    }
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
    /// Character position (not byte offset -- safe on multi-byte UTF-8
    /// query text), where the next typed character is inserted. Reported
    /// live as a real gap: query editing was always append-at-end /
    /// remove-from-end, so fixing a typo mid-query meant backspacing
    /// everything after it and retyping, rather than moving the cursor
    /// there directly.
    pub cursor: usize,
    pub results: Vec<TrackResult>,
    pub selected: usize,
    pub searching: bool,
    pub client_ready: bool,
    /// Distinct from "searched, zero matches" -- a real error (rate
    /// limit, network, auth) gets surfaced instead of silently looking
    /// like an empty result set.
    pub error: Option<String>,
}

/// Char-boundary-safe cursor editing, shared by every text-input field in
/// the app (Search's query, and every list screen's `/` filter box --
/// duplicating this a second time for `ListFilter` is what justified
/// pulling it out here instead of leaving it as a `SearchState`-only
/// method). Cursor is a character index, not a byte offset, so this stays
/// correct on multi-byte UTF-8 text.
fn text_char_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Inserts `c` at `*cursor` and advances it by one character.
fn text_insert_at_cursor(s: &mut String, cursor: &mut usize, c: char) {
    let byte_pos = text_char_byte_offset(s, *cursor);
    s.insert(byte_pos, c);
    *cursor += 1;
}

/// Deletes the character immediately before `*cursor`, if any -- not
/// always the last character in the string.
fn text_backspace_at_cursor(s: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = text_char_byte_offset(s, *cursor - 1);
    let end = text_char_byte_offset(s, *cursor);
    s.replace_range(start..end, "");
    *cursor -= 1;
}

fn text_cursor_left(cursor: &mut usize) {
    *cursor = cursor.saturating_sub(1);
}

fn text_cursor_right(s: &str, cursor: &mut usize) {
    *cursor = (*cursor + 1).min(s.chars().count());
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            cursor: 0,
            results: Vec::new(),
            selected: 0,
            searching: false,
            client_ready: false,
            error: None,
        }
    }

    /// Inserts `c` at the cursor and advances it by one character.
    pub fn insert_at_cursor(&mut self, c: char) {
        text_insert_at_cursor(&mut self.query, &mut self.cursor, c);
    }

    /// Deletes the character immediately before the cursor, if any --
    /// not always the last character in the query.
    pub fn backspace_at_cursor(&mut self) {
        text_backspace_at_cursor(&mut self.query, &mut self.cursor);
    }

    pub fn cursor_left(&mut self) {
        text_cursor_left(&mut self.cursor);
    }

    pub fn cursor_right(&mut self) {
        text_cursor_right(&self.query, &mut self.cursor);
    }
}

#[cfg(test)]
mod search_cursor_tests {
    use super::*;

    #[test]
    fn insert_at_cursor_appends_when_cursor_at_end() {
        let mut s = SearchState::new();
        s.query = "abc".to_string();
        s.cursor = 3;
        s.insert_at_cursor('x');
        assert_eq!(s.query, "abcx");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn insert_at_cursor_inserts_in_the_middle() {
        let mut s = SearchState::new();
        s.query = "ac".to_string();
        s.cursor = 1;
        s.insert_at_cursor('b');
        assert_eq!(s.query, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_at_cursor_removes_char_before_cursor_not_always_the_last() {
        let mut s = SearchState::new();
        s.query = "abc".to_string();
        s.cursor = 2; // between 'b' and 'c'
        s.backspace_at_cursor();
        assert_eq!(s.query, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn backspace_at_cursor_zero_is_a_noop() {
        let mut s = SearchState::new();
        s.query = "abc".to_string();
        s.cursor = 0;
        s.backspace_at_cursor();
        assert_eq!(s.query, "abc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn cursor_left_and_right_clamp_at_bounds() {
        let mut s = SearchState::new();
        s.query = "ab".to_string();
        s.cursor = 0;
        s.cursor_left();
        assert_eq!(s.cursor, 0);
        s.cursor = 2;
        s.cursor_right();
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_and_backspace_are_char_boundary_safe_on_multibyte_text() {
        // Same real title this codebase's truncate_ellipsis test already
        // uses -- must not panic by slicing mid-codepoint. Chars are
        // 0:友 1:人 2:A 3:君, so cursor=2 sits immediately before 'A'.
        let mut s = SearchState::new();
        s.query = "友人A君".to_string();
        s.cursor = 2;
        s.insert_at_cursor('X');
        assert_eq!(s.query, "友人XA君");
        s.backspace_at_cursor();
        assert_eq!(s.query, "友人A君");
    }
}

#[cfg(test)]
mod filter_cursor_tests {
    use super::*;

    // The char-boundary-safe editing itself is already covered by
    // search_cursor_tests (same shared text_* functions underneath) --
    // these cover what's actually specific to ListFilter: start_editing's
    // cursor placement, insert/backspace/left/right routing through it,
    // and cancel_editing actually clearing the query (not just toggling
    // `editing` off).

    #[test]
    fn start_editing_places_cursor_after_existing_query_not_at_the_start() {
        let mut f = ListFilter { query: "abc".to_string(), ..Default::default() };
        f.start_editing();
        assert!(f.editing);
        assert_eq!(f.cursor, 3);
    }

    #[test]
    fn start_editing_on_an_empty_query_leaves_cursor_at_zero() {
        let mut f = ListFilter::default();
        f.start_editing();
        assert_eq!(f.cursor, 0);
    }

    #[test]
    fn insert_backspace_and_arrows_operate_at_the_cursor() {
        let mut f = ListFilter { query: "ac".to_string(), ..Default::default() };
        f.start_editing(); // cursor -> 2, end of "ac"
        f.cursor_left(); // cursor -> 1, between 'a' and 'c'
        f.insert_at_cursor('b');
        assert_eq!(f.query, "abc");
        assert_eq!(f.cursor, 2);
        f.cursor_right();
        assert_eq!(f.cursor, 3); // clamped at the end
        f.backspace_at_cursor();
        assert_eq!(f.query, "ab");
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn cancel_editing_clears_the_query_not_just_the_editing_flag() {
        let mut f = ListFilter { query: "abc".to_string(), ..Default::default() };
        f.start_editing();
        f.cancel_editing();
        assert!(!f.editing);
        assert!(f.query.is_empty());
        assert_eq!(f.cursor, 0);
    }
}

/// The one deliberate accent color (progress bar fill + current lyric
/// line). Everything else stays default/dim -- restraint per
/// fable-ui-design: one bold moment, not color everywhere. Indexed
/// (not RGB) so it renders correctly over plain tmux-256color, not just
/// true-color terminals.
const ACCENT: Color = Color::Indexed(35); // a spotify-adjacent green
/// Confirm-overlay severity tier for a heads-up that's easy to undo
/// (e.g. adding a duplicate track) -- distinct from `Color::Red`
/// (irreversible: delete playlist, remove track) so the border color
/// alone signals how carefully to read the message before answering,
/// instead of every confirm using the same red regardless of stakes.
const WARN: Color = Color::Indexed(214); // amber, same 256-color-safe reasoning as ACCENT
/// Irreversible/failed -- the severity tier above WARN. Named rather than
/// a new value: this is the exact `Color::Red` the error box, the Danger
/// confirm tier, and the status line already used literally, so naming
/// it changes zero pixels and makes the next call site that needs it
/// obvious rather than another bare `Color::Red`.
const DANGER: Color = Color::Red;
/// Secondary/dim text: captions, meta facts beside a title, empty-state
/// copy. `Color::DarkGray`, matching the dozen call sites that already
/// reach for it literally. Deliberately not a second, dimmer tone
/// matching the mockup's `--text-dim`/`--text-faint` split -- both
/// terminal equivalents are theme-remapped colors (in many palettes
/// they'd be indistinguishable or inverted), so this ports the
/// hierarchy (primary vs. secondary), not the literal two-step scale.
const DIM: Color = Color::DarkGray;

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
    /// Set alongside the three fields above, at the same `PlayerEvent::
    /// TrackChanged` handler -- lets `render_art` know which track a
    /// cached `StatefulProtocol` cover image actually belongs to, since
    /// artist+title alone isn't a reliable cache key (two different
    /// tracks can share both).
    pub current_track_uri: Option<String>,
    /// What list/screen the currently-playing track was started from --
    /// "Liked Songs", a playlist's name, "Search", an album's name. Set
    /// at every existing "start playback" call site alongside the
    /// `LoadRequest`/`spirc.load` call, never a new one. Display-only:
    /// `n`/`p` skip within whatever context Spotify itself is already
    /// using and don't touch this field, so it correctly persists across
    /// a skip and only changes on the next deliberate play action.
    pub context_label: Option<String>,
    /// Kept in step with the player via `ShuffleChanged`/`RepeatChanged`
    /// events (which librespot also emits once at connect with the real
    /// resumed state, and when another device changes them), plus an
    /// optimistic update at the keypress so a quick second press cycles
    /// from the state just requested rather than a stale one.
    pub shuffle: bool,
    /// Smart shuffle: shuffle with Spotify's recommendations mixed in. A
    /// third state of `shuffle`, read from librespot's connect state, and
    /// only ever true while `shuffle` is.
    pub smart_shuffle: bool,
    pub repeat: RepeatMode,
    pub lyrics: LyricsState,
    /// "Where these lyrics came from" (e.g. `Apple Music via Spicy Lyrics`),
    /// shown dim under the lyrics. `Some` only for a result whose source
    /// asks to be credited; cleared with `lyrics` on every track change.
    pub lyrics_credit: Option<String>,
    /// Show lyrics romanized (`t`). Global: applies to every track with
    /// Japanese, Chinese or Korean lyrics until toggled off.
    pub romanize_lyrics: bool,
    /// The current sheet's romanization, in step with its lines, once it has
    /// been computed off the render thread (`None` until then, and for a
    /// sheet with nothing to romanize). Cleared with `lyrics` on track change.
    pub romanized_lines: Option<Vec<Option<crate::romanize::RomanLine>>>,
    pub current_line: Option<usize>,
    pub fullscreen: bool,
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
    pub queue: QueueState,
    pub devices: DevicesState,
    pub playlist_detail: Option<PlaylistDetailState>,
    pub artist_detail: Option<ArtistDetailState>,
    pub album_detail: Option<AlbumDetailState>,
    pub pinned_playlists: std::collections::HashSet<String>,
    pub pinned_tracks: std::collections::HashSet<String>,
    /// Which playlists are already known to contain which tracks --
    /// populated only from track lists this app fetched for some other
    /// reason (opening Playlist Detail, the picker's own pre-add
    /// duplicate check), plus an optimistic write-through whenever a
    /// track is actually added to or removed from a playlist through
    /// this app (the same "trust a local update, not a refetch, to
    /// reflect a just-completed write" lesson `bump_track_count` already
    /// established -- Spotify's own write-propagation lag proved a
    /// refetch unreliable for exactly this once already this session).
    /// Deliberately incomplete: a missing entry means "not known," never
    /// "confirmed absent" -- the picker's marker only ever makes a
    /// positive claim. Never consulted for the real duplicate check
    /// before an add, which stays a live fetch; a stale cache saying
    /// "already in it" must never suppress a legitimate add. Session-
    /// only, not persisted to disk (unlike pins).
    pub playlist_membership: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Phase 5's transient overlays -- see the doc comment above
    /// `TextPrompt` for why these are sibling `Option`s here rather than
    /// `Screen` stack variants. Checked in this order (confirm gates
    /// hardest, a picker is "just" a list): only one is ever `Some` at a
    /// time in practice, but the order matters if that invariant is ever
    /// violated by a future bug -- confirm should always win.
    pub pending_confirm: Option<PendingConfirm>,
    pub text_prompt: Option<TextPrompt>,
    pub playlist_picker: Option<PlaylistPicker>,
    /// Phase 12's global quick-jump palette (`Ctrl+P`) -- a fourth
    /// sibling overlay at the same tier as the three above.
    pub quick_jump: Option<QuickJump>,
    /// Transient (message, is_error) shown in the status line, cleared on
    /// the next keypress. Every Phase 5 mutation's result -- success or
    /// failure -- surfaces here; there was no general status/toast field
    /// before this phase; every prior error surface was feature-specific
    /// (`SearchState::error`, `Fetch::Failed`).
    pub status: Option<(String, bool)>,
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
    pub playlist_picker: ListState,
    pub quick_jump: ListState,
    pub queue: ListState,
    pub devices: ListState,
    pub artist_detail: ListState,
    pub album_detail: ListState,
    /// Help's scroll offset in rendered rows -- a plain `u16` for
    /// `Paragraph::scroll`, not a `ListState`: Help is a reference the
    /// user scans, not a list they navigate item by item (the mockup's
    /// own reasoning for giving it a two-column layout with no
    /// per-row selection at all). Clamped inside `render_help` against
    /// the real rendered height, so the key handler in `main.rs` can
    /// increment/decrement blindly without knowing the content size.
    pub help: u16,
}

/// Real album art via a terminal graphics protocol, threaded through
/// `render` the same way `ScrollState` is and for the same reason: it's
/// mutable render-side cache, not application state, and nesting it
/// inside `AppState` would fight the borrow checker the same way
/// `ScrollState`'s own doc comment already explains.
///
/// `picker` is populated once at startup (`main.rs`) if the terminal's
/// capability query (possibly overridden -- see that call site) reports
/// a real graphics protocol; `None` means "no real protocol available or
/// detection failed," in which case `render_art` always uses the hashed
/// placeholder and never touches the fields below at all.
///
/// `cover_image` holds the currently-playing track's *decoded* cover
/// (cheap to keep, no network/decode cost to reuse) plus the track uri
/// it belongs to, set once per track (`main.rs`, off the render path).
/// `sized_covers` is a small cache of already resize-encoded
/// `StatefulProtocol`s, one per distinct `(track uri, width, height)`
/// this app has actually rendered at -- built lazily in `render_art`,
/// not eagerly. This two-level design (decode once, encode once per
/// size) exists because a single shared `StatefulProtocol` re-encodes,
/// and on Kitty fully *re-transmits*, the whole image every time its
/// render `Rect`'s cell size changes (confirmed by reading
/// `ratatui-image`'s own Kitty protocol source) -- since the compact
/// hero and the fullscreen layouts use different art sizes by design,
/// a single shared protocol meant every `f` toggle forced a full
/// re-transmit, reported live as visible lag and display corruption
/// under rapid toggling. Caching one encoded protocol per size actually
/// seen means toggling between a stable, already-visited set of sizes
/// (the normal case) never re-triggers that cost after the first visit
/// to each size.
#[derive(Default)]
pub struct ImageState {
    pub picker: Option<ratatui_image::picker::Picker>,
    pub cover_image: Option<(String, image::DynamicImage)>,
    pub sized_covers: Vec<(String, u16, u16, ratatui_image::protocol::StatefulProtocol)>,
    /// One-shot, whole-process-lifetime retransmit: armed the first time
    /// this run builds any sized cover at all (in practice, the boot
    /// track's cover), fired once `STARTUP_RETRANSMIT_DELAY` later by
    /// clearing the entire cache so the very next render misses and
    /// rebuilds+retransmits fresh -- exactly what manually skipping a
    /// track and back already does to "fix" a blank cover, just
    /// automatic. A near-identical mechanism was tried once before this
    /// session at a 700ms delay and reverted: that gap was still short
    /// enough to land while Ghostty's own kitty image-compositing state
    /// from the *first* transmission was still settling, and a second
    /// full transmission landing in that window corrupted every
    /// subsequent cover for the rest of the session (the same trigger
    /// Phase 18 already found once, for a different cause). Reattempted
    /// here at a real multi-second delay specifically because that's
    /// the property that makes a *manual* skip-then-back safe -- by the
    /// time a human notices and acts, real seconds have passed, not
    /// milliseconds. The exact minimum safe gap isn't independently
    /// confirmed; this is a live experiment against real hardware, not
    /// a proven fix -- if blank art recurs, the delay needs widening
    /// further; if pixelation/corruption recurs instead, the gap is
    /// still too short and this needs reverting again.
    pub startup_retransmit_at: Option<std::time::Instant>,
    pub startup_retransmit_done: bool,
}

/// How long to wait, after this process's very first cover-art
/// transmission, before automatically clearing the cache and
/// retransmitting once -- see `ImageState::startup_retransmit_at`'s own
/// doc comment for why this specific value is a judgment call, not a
/// derived or confirmed-safe number.
const STARTUP_RETRANSMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(2000);

/// How many distinct `(track, size)` encoded protocols `ImageState`
/// keeps at once -- comfortably more than the handful of distinct art
/// sizes one session realistically produces (compact, fullscreen
/// two-pane, fullscreen narrow-stacked), so eviction is rare in
/// practice, not a tight budget being constantly hit.
const SIZED_COVER_CACHE_CAP: usize = 4;

const STARTUP_SPINNER: [char; 10] = ['\u{280B}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283C}', '\u{2834}', '\u{2826}', '\u{2827}', '\u{2807}', '\u{280F}'];

/// The very first thing drawn on a cold start, while `connect_spirc()` is
/// still resolving in the background -- previously this window was a
/// blank alternate-screen with zero feedback (the whole render loop was
/// blocked behind `connect_spirc().await`, which takes several real
/// seconds: AP resolution, auth, first track load). Reported live as a
/// separate, related bug: the very first album-art render after a cold
/// start would show completely blank (skipping to another track and back
/// fixed it), most likely a real terminal-side race -- Ghostty's own
/// kitty-graphics subsystem not yet ready for the first image placement
/// immediately after entering the alternate screen. Showing this
/// animation for a guaranteed minimum duration (`main.rs`'s
/// `STARTUP_MIN_VISIBLE`) turns an unexplained blank wait into a
/// deliberate, visible one, and gives that subsystem a real window to
/// finish initializing before the first real frame (with real album art)
/// ever gets drawn.
pub fn render_startup(frame: &mut Frame, tick: usize) {
    let area = frame.area();
    let spinner = STARTUP_SPINNER[tick % STARTUP_SPINNER.len()];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new("spot-tui").alignment(Alignment::Center).style(Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(format!("{spinner} connecting to spotify\u{2026}"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        rows[2],
    );
}

pub fn render(frame: &mut Frame, app: &AppState, scroll: &mut ScrollState, images: &mut ImageState) {
    if app.fullscreen && *app.nav.top() == Screen::NowPlaying {
        render_fullscreen(frame, app, images);
        render_overlays(frame, app, &mut scroll.playlist_picker, &mut scroll.quick_jump);
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
        Screen::NowPlaying => render_compact(frame, app, images, main_area),
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
        Screen::Help => render_help(frame, main_area, &mut scroll.help),
        Screen::Queue => render_queue(frame, app, &mut scroll.queue, main_area),
        Screen::Devices => render_devices(frame, app, &mut scroll.devices, main_area),
        Screen::ArtistDetail => render_artist_detail(frame, app, &mut scroll.artist_detail, main_area),
        Screen::AlbumDetail => render_album_detail(frame, app, &mut scroll.album_detail, main_area),
    }
    render_playbar(frame, app, playbar_area);
    render_status(frame, app, status_area);
    render_overlays(frame, app, &mut scroll.playlist_picker, &mut scroll.quick_jump);
}

/// Draws whichever overlay is active (at most one in practice -- see the
/// field order comment on `AppState`) centered on top of whatever's
/// already been drawn this frame, fullscreen included. Called last
/// specifically so it paints over everything else.
fn render_overlays(
    frame: &mut Frame,
    app: &AppState,
    picker_list_state: &mut ListState,
    quick_jump_list_state: &mut ListState,
) {
    if let Some(confirm) = &app.pending_confirm {
        render_confirm_overlay(frame, confirm);
    } else if let Some(prompt) = &app.text_prompt {
        render_text_prompt_overlay(frame, prompt);
    } else if let Some(picker) = &app.playlist_picker {
        render_playlist_picker_overlay(frame, app, picker, picker_list_state);
    } else if let Some(qj) = &app.quick_jump {
        render_quick_jump_overlay(frame, app, qj, quick_jump_list_state);
    }
}

/// A `Rect` of `width` x `height` centered within `area`, clamped so it
/// never exceeds `area` on a narrow/short terminal.
fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

// Every overlay used its own bare width/height literals (40x13, 50x16,
// 50x3, and confirm's own self-sizing) with nothing shared -- one width
// for the two list-shaped overlays (picker, quick jump) so they read as
// one system, and a named horizontal-padding amount applied to all four.
const OVERLAY_LIST_WIDTH: u16 = 50;
const OVERLAY_LIST_HEIGHT: u16 = 16;
const OVERLAY_PROMPT_WIDTH: u16 = 50;
const OVERLAY_PROMPT_HEIGHT: u16 = 3; // 1 content row + 2 borders -- no vertical padding
const OVERLAY_CONFIRM_MIN_WIDTH: u16 = 24;
const OVERLAY_CONFIRM_MAX_WIDTH: u16 = 70;
/// Horizontal-only: a blank row costs real percentage height in a
/// 13-16-row list overlay for no benefit the border doesn't already
/// give; horizontal has a real payoff since content otherwise sits flush
/// against the border everywhere else in the app doesn't.
const OVERLAY_PAD_X: u16 = 1;
/// Borders (2) + horizontal padding (2x `OVERLAY_PAD_X`) -- everything
/// between an overlay's outer width and its usable text width. The
/// confirm overlay predicts its own wrapped height by hand rather than
/// going through `Block::inner` (the other three overlays get padding
/// subtracted for free), so this constant must stay the single source
/// of truth for both its width-clamp formula and the width it feeds to
/// `wrapped_line_count` -- if vertical padding is ever added, the `+ 2`
/// in `render_confirm_overlay`'s height formula must become `+ 4` at the
/// same time, or long messages clip again.
const OVERLAY_CHROME_X: u16 = 2 + 2 * OVERLAY_PAD_X;

fn render_text_prompt_overlay(frame: &mut Frame, prompt: &TextPrompt) {
    let area = centered_rect(frame.area(), OVERLAY_PROMPT_WIDTH, OVERLAY_PROMPT_HEIGHT);
    frame.render_widget(Clear, area);
    let text = cursor_text(&prompt.query, prompt.cursor);
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .padding(Padding::horizontal(OVERLAY_PAD_X))
                .title(prompt.title.clone()),
        ),
        area,
    );
}

/// Greedy word-wrap into the actual line strings, matching `Paragraph`'s
/// own `Wrap` behavior closely enough to predict it -- there's no way to
/// ask ratatui how many lines (or which lines) a `Paragraph` will wrap to
/// before rendering it, so both count and content are predicted here
/// separately. A word longer than `width` still gets its own line rather
/// than being split mid-word, matching `Wrap`'s own behavior.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn wrapped_line_count(text: &str, width: u16) -> u16 {
    (wrap_words(text, width as usize).len() as u16).max(1)
}

#[cfg(test)]
mod wrap_words_tests {
    use super::*;

    #[test]
    fn fits_on_one_line() {
        assert_eq!(wrap_words("Quit spot-tui? y/n", 60), vec!["Quit spot-tui? y/n".to_string()]);
    }

    #[test]
    fn wraps_into_the_exact_pieces() {
        let text = "aaaa aaaa aaaa aaaa aaaa";
        assert_eq!(
            wrap_words(text, 10),
            vec!["aaaa aaaa".to_string(), "aaaa aaaa".to_string(), "aaaa".to_string()]
        );
    }

    #[test]
    fn a_word_longer_than_the_width_gets_its_own_line_not_split() {
        assert_eq!(wrap_words("supercalifragilisticexpialidocious", 10), vec!["supercalifragilisticexpialidocious".to_string()]);
    }

    #[test]
    fn empty_text_is_one_empty_line_not_zero() {
        assert_eq!(wrap_words("", 20), vec![String::new()]);
    }
}

/// Wraps a `Fetch::Failed` message in a bordered box instead of a bare
/// line of text -- designed as its own state (per fable-ui-design), not
/// a stripped-down list. A real 400 was this project's single most-
/// repeated live bug class; it deserves to be legible, not just present.
fn render_fetch_error(frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(format!("failed to load: {message}"))
            .style(Style::default().fg(DANGER))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(DANGER))),
        area,
    );
}

/// The app's one loading state -- previously inlined separately at every
/// call site with three different treatments (bare default-color text,
/// bare text in the body while a bold "loading…" sat in the header row,
/// or nothing styled at all). One dim, lowercase, wordless-except-the-
/// ellipsis line, matching `render_fetch_error`'s own restraint.
fn render_loading(frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new("loading\u{2026}").style(Style::default().fg(DIM)), area);
}

/// A designed empty state: a dim headline naming what's absent, then an
/// optional fainter-in-spirit (same DIM color, second line) hint naming
/// the actual key that fixes it. `hint` is `None` where no key genuinely
/// applies -- never a fabricated "press X" that would be a lie at that
/// call site. Lowercase throughout, matching the mockup's own copy
/// convention (`ui.rs`'s existing "no matches"/"nothing here yet" now
/// route through this instead of being bare unstyled strings).
fn render_empty_state(frame: &mut Frame, area: Rect, headline: &str, hint: Option<&str>) {
    let mut lines = vec![Line::from(Span::styled(headline.to_string(), Style::default().fg(DIM)))];
    if let Some(hint) = hint {
        lines.push(Line::from(Span::styled(hint.to_string(), Style::default().fg(DIM))));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn render_confirm_overlay(frame: &mut Frame, confirm: &PendingConfirm) {
    let frame_area = frame.area();
    // Fixed at a max of 60 cols with no wrapping originally -- fine for
    // short messages ("Quit spot-tui? y/n") but the newer, longer ones
    // (duplicate-track warnings naming both the track and the playlist)
    // ran off both edges of the box with no way to read the rest,
    // reported live as "completely cutoff." Now wraps, and the box grows
    // to fit however many lines that takes instead of assuming one.
    let max_width = frame_area.width.saturating_sub(4).clamp(OVERLAY_CONFIRM_MIN_WIDTH, OVERLAY_CONFIRM_MAX_WIDTH);
    let width = (confirm.message.chars().count() as u16 + 4).clamp(OVERLAY_CONFIRM_MIN_WIDTH, max_width);
    let inner_width = width.saturating_sub(OVERLAY_CHROME_X);
    let height = (wrapped_line_count(&confirm.message, inner_width) + 2).min(frame_area.height);
    let area = centered_rect(frame_area, width, height);
    let color = match confirm.action.severity() {
        ConfirmSeverity::Danger => DANGER,
        ConfirmSeverity::Warn => WARN,
        ConfirmSeverity::Neutral => ACCENT,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(confirm.message.clone())
            .alignment(Alignment::Center)
            .style(Style::default().fg(color))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color))
                    .padding(Padding::horizontal(OVERLAY_PAD_X)),
            ),
        area,
    );
}

#[cfg(test)]
mod confirm_overlay_tests {
    use super::*;

    #[test]
    fn short_message_fits_on_one_line() {
        assert_eq!(wrapped_line_count("Quit spot-tui? y/n", 60), 1);
    }

    #[test]
    fn long_message_wraps_to_the_expected_number_of_lines() {
        // 5 words of 4 chars each ("aaaa" x5) at width 10 fits "aaaa aaaa"
        // (9 chars) per line, one word per line beyond that -- 3 lines.
        let text = "aaaa aaaa aaaa aaaa aaaa";
        assert_eq!(wrapped_line_count(text, 10), 3);
    }

    #[test]
    fn a_word_longer_than_the_width_still_counts_as_one_line() {
        assert_eq!(wrapped_line_count("supercalifragilisticexpialidocious", 10), 1);
    }

    #[test]
    fn empty_message_is_one_line_not_zero() {
        assert_eq!(wrapped_line_count("", 20), 1);
    }
}

/// Reuses `render_display_list` (the same helper every other list in the
/// app already uses) specifically for its `ListState`-backed scrolling --
/// the picker's first version built its rows as a plain `Paragraph`,
/// which never scrolls at all, so a playlist past the visible height was
/// simply unreachable (reported live).
/// Whether `playlist_uri` is known to already contain `track_uri`, per
/// `AppState::playlist_membership`'s own doc comment on why "unknown" is
/// a real, distinct third answer here, not just "no" -- an incomplete
/// cache must never claim a track is confirmed absent from a playlist
/// nobody's looked inside yet this session.
fn playlist_has_track(
    membership: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    playlist_uri: &str,
    track_uri: &str,
) -> bool {
    membership.get(playlist_uri).is_some_and(|tracks| tracks.contains(track_uri))
}

#[cfg(test)]
mod playlist_has_track_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn known_member_returns_true() {
        let mut membership = HashMap::new();
        membership.insert("p1".to_string(), HashSet::from(["t1".to_string()]));
        assert!(playlist_has_track(&membership, "p1", "t1"));
    }

    #[test]
    fn known_playlist_without_this_track_returns_false_not_a_confirmed_claim() {
        let mut membership = HashMap::new();
        membership.insert("p1".to_string(), HashSet::from(["t2".to_string()]));
        assert!(!playlist_has_track(&membership, "p1", "t1"));
    }

    #[test]
    fn never_checked_playlist_returns_false() {
        let membership = HashMap::new();
        assert!(!playlist_has_track(&membership, "p1", "t1"));
    }
}

/// Draws the shared shape both list-style overlays (picker, quick jump)
/// use -- `Clear`, a bordered+padded block with a title, and the live
/// filter line with a real mid-string cursor (previously trailing-only
/// on both, which actively lied: both key handlers already support real
/// `Left`/`Right` cursor movement) -- and hands back the `Rect` the
/// caller should draw its own genuinely-different body into.
/// Deliberately stops at the chrome: the picker has real `Fetch`-state
/// arms of its own and quick jump doesn't, so pushing that into a shared
/// enum would be more machinery than the duplication it removes.
fn filter_overlay_body(frame: &mut Frame, title: &str, filter: &ListFilter) -> Rect {
    let area = centered_rect(frame.area(), OVERLAY_LIST_WIDTH, OVERLAY_LIST_HEIGHT);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::horizontal(OVERLAY_PAD_X))
        .title(title.to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    frame.render_widget(Paragraph::new(format!("/{}", cursor_text(&filter.query, filter.cursor))), chunks[0]);
    chunks[1]
}

fn render_playlist_picker_overlay(
    frame: &mut Frame,
    app: &AppState,
    picker: &PlaylistPicker,
    list_state: &mut ListState,
) {
    let body = filter_overlay_body(frame, "Add to playlist", &picker.filter);

    let label = |p: &crate::api::library::PlaylistSummary| p.name.clone();
    match &app.library.playlists {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, body);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, body, e);
        }
        Fetch::Ready(items) if items.is_empty() => {
            render_empty_state(frame, body, "no playlists yet", Some("press c to create one"));
        }
        Fetch::Ready(items) => {
            let ordered =
                pinned_first(filtered_sorted(items, &picker.filter, &label), &app.pinned_playlists, |p| p.uri.as_str());
            // Two fixed marker columns, never overlapping: pin first
            // (unchanged), membership second. The membership marker only
            // ever makes a *positive* claim -- "known absent" and "never
            // checked" both render blank, so an incomplete cache can
            // never state something false. Plain text, not a styled
            // `Span` -- `render_display_list` already colors the whole
            // selected row ACCENT+BOLD, so this inherits that for free
            // instead of needing its own color (and risking the same
            // pin-marker-vs-selection clash already fixed once this
            // session by making selection win).
            let pin_label = |p: &crate::api::library::PlaylistSummary| {
                let pin = if app.pinned_playlists.contains(&p.uri) { "*" } else { " " };
                let member =
                    if playlist_has_track(&app.playlist_membership, &p.uri, &picker.track_uri) { "\u{2713}" } else { " " };
                format!("{pin}{member} {}", label(p))
            };
            render_display_list(
                frame,
                body,
                &ordered,
                picker.selected,
                &pin_label,
                picker.filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// Phase 12's quick-jump palette. Built on `filter_overlay_body` -- the
/// same shared chrome `render_playlist_picker_overlay` uses -- over the
/// flattened, heterogeneous pool `quick_jump_entries` builds fresh from
/// live `AppState` every render, so a background fetch (eager-triggered
/// on open) landing while this is open shows up on the very next frame
/// with no extra plumbing.
fn render_quick_jump_overlay(frame: &mut Frame, app: &AppState, qj: &QuickJump, list_state: &mut ListState) {
    let body = filter_overlay_body(frame, "Quick jump", &qj.filter);
    let entries = quick_jump_entries(app, &qj.filter);
    let label = |e: &QuickJumpEntry| e.label.clone();
    let display = filtered_sorted(&entries, &qj.filter, &label);
    render_display_list(frame, body, &display, qj.selected, &label, qj.filter.query.is_empty(), list_state);
}

/// Fixed key column, in cells -- the one place this screen deliberately
/// breaks the app's spacing grid, same exception the mockup's own 108px
/// `.help-key` column makes and for the same reason: an aligned key
/// column is what makes a dense reference actually scannable.
const HELP_KEY_WIDTH: usize = 14;
/// Below this main-area width, Help renders as a single scrolling
/// column instead of two -- two columns need roughly a 118-column
/// terminal once the 22-column sidebar is accounted for, and unlike the
/// browser mockup (no narrow case to worry about), this app has to
/// handle a genuinely narrow terminal without the content becoming
/// unreadable.
const HELP_TWO_COLUMN_MIN_WIDTH: u16 = 96;

/// Keep this in sync as new keys get wired -- Phase 4's whole point was
/// moving Help to right after this session's current point in the build
/// rather than writing it once at the end from a settled keybind table,
/// so each later phase's own "done" should include updating this.
/// `Search` is deliberately excluded from opening Help via `?` (it's the
/// one screen where every printable character, `?` included, has to
/// reach the query box), which is also why the reference below doesn't
/// claim `?` works "from literally anywhere."
const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("Tab", "switch focus between Sidebar and Main"),
            ("Esc", "back one level; at the root, focus moves to Sidebar"),
            ("?", "this screen (not while typing in Search or a filter)"),
            (
                "q",
                "quit -- asks \"Quit spot-tui? y/n\" first by default; set confirm_quit = false in config.toml for immediate quit (not while typing in Search)",
            ),
            (
                "Ctrl+C",
                "quit immediately, never confirms -- a harder interrupt than q, by convention (not while typing in Search)",
            ),
            ("Space / n / p / + / -", "play-pause / next / previous / volume -- works from any screen, including while browsing a list, not just Now Playing (not while typing in Search)"),
            ("/", "jump to Search (Sidebar, Now Playing) or open a list's filter"),
            ("l", "jump to Library"),
            (
                "f",
                "fullscreen Now Playing/lyrics -- jumps there from anywhere; toggles off if already there",
            ),
            (
                "Ctrl+P",
                "quick jump -- search any playlist/liked track/artist/album/device/screen by name and jump straight to it (works even mid-query on Search; press again to close)",
            ),
            (
                "t",
                "toggle romanized lyrics: Japanese, Chinese and Korean lyrics shown in Latin letters instead of their own script (kanji read in context, pinyin with tone marks, Revised Romanization for Korean). Works from any screen (not while typing in Search or a filter); the word-by-word highlight keeps moving. Works for unsynced lyrics too (without the word-by-word highlight, which needs synced lyrics). Set romanize_lyrics = true in config.toml to start with it on",
            ),
            (
                "z",
                "cycle shuffle like Spotify's own button: off, then shuffle, then smart shuffle (\u{2726} on the playbar), then off. Works from any screen (not while typing in Search or a filter). Shuffle stays on when you start a different playlist or album. Smart shuffle sets Spotify's mode but spot-tui can't add the recommended songs itself",
            ),
            (
                "Shift+R",
                "cycle repeat: off, then the whole album/playlist, then this one song, then off. The playbar always shows \u{21c4} (shuffle) and \u{21bb} (repeat, \u{21bb}1 for this song) -- bright when on, dim when off",
            ),
        ],
    ),
    (
        "Now Playing",
        &[
            ("\u{2190} / \u{2192}", "seek \u{00b1}5s"),
            ("\u{2191} / \u{2193}", "volume (same as +/-)"),
        ],
    ),
    (
        "Sidebar (Now Playing, Search, Library, Liked Songs, Queue, Devices, then your playlists)",
        &[
            ("\u{2191} / \u{2193}", "move cursor"),
            ("Enter / \u{2192}", "open the selected entry or playlist"),
            ("Esc", "does nothing further here -- already at the root"),
            ("Shift+P", "pin / unpin the selected playlist"),
        ],
    ),
    (
        "Library lists (Liked Songs, Saved Albums, Followed Artists, Your Playlists, Playlist Detail)",
        &[
            ("\u{2191} / \u{2193}", "move selection"),
            ("Enter", "play the selected track"),
            ("Enter / \u{2192}", "open (Your Playlists \u{2192} Playlist Detail only)"),
            ("\u{2190}", "back to Sidebar (same as Esc)"),
            ("/", "open this list's filter (live-narrows as you type)"),
            (
                "Esc",
                "if a filter is applied (even after Enter, not actively typing), clears it first; press again to leave",
            ),
            ("o", "toggle alphabetical sort"),
            ("Shift+P", "pin / unpin (Your Playlists, Playlist Detail)"),
        ],
    ),
    (
        "Playlist CRUD",
        &[
            ("c", "create a new playlist -- works from any screen except Search"),
            ("r", "rename -- Your Playlists: the selected playlist; Playlist Detail: the open playlist"),
            (
                "d",
                "remove, always confirms first -- Your Playlists: delete the playlist; Playlist Detail: remove the selected track",
            ),
            (
                "Shift+D",
                "delete the open playlist itself, always confirms first -- Playlist Detail only",
            ),
            (
                "a",
                "add a track to a playlist (opens the picker) -- Liked Songs, Playlist Detail, and Now Playing (the currently playing track)",
            ),
            (
                "(duplicate check)",
                "the picker warns and asks first if the target playlist already has that track, rather than silently adding a second copy",
            ),
            (
                "m",
                "reorder tracks (Playlist Detail only) -- requires no filter/sort active; pinned tracks are fine",
            ),
        ],
    ),
    (
        "Like, Follow, Save",
        &[
            (
                "Shift+L",
                "like/unlike the selected (or currently playing) track -- Playlist Detail/Queue/Album Detail/Now Playing always like; Liked Songs always unlike, and confirms first (Ctrl+Up from Search, since every letter there has to reach the query box)",
            ),
            (
                "Shift+F",
                "follow/unfollow -- Artist Detail always follows; Followed Artists always unfollows and confirms first",
            ),
            (
                "s",
                "save/unsave the album -- Album Detail always saves; Saved Albums always unsaves and confirms first",
            ),
        ],
    ),
    (
        "Queue",
        &[
            ("\u{2191} / \u{2193}", "move selection among what's up next"),
            (
                "a",
                "add the selected queued track to a playlist -- the public Web API has no remove or reorder for the queue itself",
            ),
            (
                "Shift+Q",
                "add the selected track to the queue -- from Liked Songs, Playlist Detail, or Album Detail (Alt+\u{2193} from Search, where every letter types into the query box)",
            ),
            ("refreshes", "automatically every 5s while this screen is open"),
        ],
    ),
    (
        "Devices",
        &[
            ("\u{2191} / \u{2193}", "move selection"),
            ("Enter", "transfer playback here (keeps current play/pause state)"),
            ("r", "refresh the device list"),
        ],
    ),
    (
        "Artist Detail (Followed Artists, or `v`/Ctrl+\u{2192} from a track)",
        &[
            ("\u{2191} / \u{2193}", "move selection among the artist's albums"),
            ("Enter / \u{2192}", "open the selected album"),
            (
                "(no top tracks)",
                "Spotify removed the artist-top-tracks endpoint -- not something this app is choosing to skip",
            ),
        ],
    ),
    (
        "Album Detail (Saved Albums, an artist's album list, or `v` from a track)",
        &[
            ("\u{2191} / \u{2193}", "move selection among the album's tracks"),
            ("Enter", "play the album as context, starting from the selected track"),
            ("a", "add the selected track to a playlist"),
            ("v", "view this album's artist"),
        ],
    ),
    (
        "View an item's artist/album",
        &[
            (
                "v",
                "open the selected track's album -- Liked Songs, Playlist Detail, Queue (on Album Detail, opens the album's own artist instead -- there's no separate album to open from inside one)",
            ),
            ("Shift+V", "open the selected track's artist -- Liked Songs, Playlist Detail, Queue"),
            (
                "Ctrl+\u{2192} / Alt+\u{2192}",
                "open the selected result's album / artist -- Search only (plain letters all type into the query box there)",
            ),
            (
                "Ctrl+\u{2193}",
                "add the selected result to a playlist -- Search only, opens the picker without needing to play or leave first",
            ),
            ("Enter / \u{2192}", "open the album from Saved Albums; open the artist from Followed Artists"),
        ],
    ),
    (
        "Move mode (Playlist Detail, after `m`)",
        &[
            ("\u{2191} / \u{2193}", "relocate the track one slot at a time, locally -- no network call per keystroke"),
            ("g", "jump the track straight to a typed position (1 = top) instead of nudging it slot by slot -- still local, still confirmed or cancelled with Enter/Esc afterward"),
            ("Enter", "confirm -- one reorder call for the net displacement"),
            ("Esc", "cancel -- walks the track back to where it started"),
        ],
    ),
    (
        "Prompt / confirm / picker overlays",
        &[
            ("Enter", "prompt: submit. picker: add to the selected playlist. confirm: same as y"),
            ("y / n", "confirm: y does it, n cancels"),
            ("Esc", "cancel and close, no exceptions"),
            ("\u{2190} / \u{2192}", "prompt: move the cursor within the text. picker: move the cursor within its filter"),
            ("\u{2191} / \u{2193}", "picker: move the selected playlist"),
            (
                "any letter/number",
                "picker: narrows the list by name -- always live, no separate key to start typing",
            ),
            ("Backspace", "picker: delete the character before the cursor in its filter"),
        ],
    ),
    (
        "While typing (a filter, or Search's query)",
        &[
            ("\u{2191} / \u{2193}", "move the highlighted track (filters only -- keeps working while typing)"),
            ("\u{2190} / \u{2192}", "move the cursor within the text"),
            ("Backspace", "delete the character before the cursor"),
            ("Enter", "commit (Search: run the search; filters: stop editing, keep the narrowed list)"),
            ("Esc", "Search: back. Filters: stop editing AND clear the filter back to the full list"),
        ],
    ),
    (
        "Help screen (this one)",
        &[
            ("\u{2191} / \u{2193}", "scroll one row"),
            ("PageUp / PageDown", "scroll ten rows"),
        ],
    ),
];

/// One section's rendered lines: an ACCENT+BOLD title, then one row per
/// binding with the key padded to `HELP_KEY_WIDTH` and the description
/// in `DIM`, hand-wrapped (not `Paragraph`'s own `Wrap`) with a hanging
/// indent so continuation lines stay under the description column
/// instead of resetting to column 0. Hand-wrapping is also what makes
/// the rendered height exactly `lines.len()`, which is what makes the
/// scroll clamp in `render_help` correct.
fn help_section_lines(title: &str, rows: &[(&str, &str)], width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        title.to_string(),
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    let desc_width = width.saturating_sub(HELP_KEY_WIDTH + 1).max(1);
    let indent = " ".repeat(HELP_KEY_WIDTH + 1);
    for (key, desc) in rows {
        let wrapped = wrap_words(desc, desc_width);
        for (i, piece) in wrapped.into_iter().enumerate() {
            if i == 0 {
                lines.push(Line::from(vec![
                    Span::raw(format!("{key:<HELP_KEY_WIDTH$} ")),
                    Span::styled(piece, Style::default().fg(DIM)),
                ]));
            } else {
                lines.push(Line::from(vec![Span::raw(indent.clone()), Span::styled(piece, Style::default().fg(DIM))]));
            }
        }
    }
    lines
}

/// Index of the first section that starts column 2, chosen so the two
/// columns come out as close to equal real rendered height as possible
/// -- sections range from 2 to 10 rows each, so the mockup's own
/// `ceil(section_count / 2)` split (an even *section* count) is not an
/// even *height* split. Pure and greedy: keep adding sections to column
/// 1 until doing so would reach or pass half the total height.
fn help_column_split(section_heights: &[usize]) -> usize {
    let total: usize = section_heights.iter().sum();
    let target = total / 2;
    let mut running = 0;
    for (i, h) in section_heights.iter().enumerate() {
        running += h;
        if running >= target {
            return i + 1;
        }
    }
    section_heights.len()
}

#[cfg(test)]
mod help_column_split_tests {
    use super::*;

    #[test]
    fn balances_by_real_height_not_section_count() {
        // total=21, half=10.5 -- splitting after the 3rd section (15 vs
        // 6) is closer to even than after the 2nd (5 vs 16) despite
        // being an uneven *section* count either way.
        assert_eq!(help_column_split(&[2, 3, 10, 4, 2]), 3);
    }

    #[test]
    fn even_heights_split_down_the_middle() {
        assert_eq!(help_column_split(&[5, 5, 5, 5]), 2);
    }

    #[test]
    fn no_sections_is_a_no_op_split() {
        assert_eq!(help_column_split(&[]), 0);
    }

    #[test]
    fn a_single_section_all_goes_in_column_one() {
        assert_eq!(help_column_split(&[5]), 1);
    }
}

fn render_help(frame: &mut Frame, area: Rect, offset: &mut u16) {
    let shell =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(area);
    frame.render_widget(Paragraph::new(screen_header_line("Keybinds", None)), shell[0]);
    let body_area = shell[1];

    if body_area.width < HELP_TWO_COLUMN_MIN_WIDTH {
        let width = body_area.width as usize;
        let mut lines: Vec<Line> = Vec::new();
        for &(title, rows) in HELP_SECTIONS {
            if !lines.is_empty() {
                lines.push(Line::from(""));
            }
            lines.extend(help_section_lines(title, rows, width));
        }
        let max_offset = (lines.len() as u16).saturating_sub(body_area.height);
        *offset = (*offset).min(max_offset);
        frame.render_widget(Paragraph::new(lines).scroll((*offset, 0)), body_area);
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(4), Constraint::Min(1)])
        .split(body_area);
    let col_width = cols[0].width as usize;

    let section_lines: Vec<Vec<Line>> =
        HELP_SECTIONS.iter().map(|&(title, rows)| help_section_lines(title, rows, col_width)).collect();
    let section_heights: Vec<usize> = section_lines.iter().map(Vec::len).collect();
    let split = help_column_split(&section_heights);

    let build_column = |sections: &[Vec<Line<'static>>]| -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for lines in sections {
            if !out.is_empty() {
                out.push(Line::from(""));
            }
            out.extend(lines.iter().cloned());
        }
        out
    };
    let col1 = build_column(&section_lines[..split]);
    let col2 = build_column(&section_lines[split..]);
    let tallest = col1.len().max(col2.len()) as u16;
    let max_offset = tallest.saturating_sub(body_area.height);
    *offset = (*offset).min(max_offset);

    frame.render_widget(Paragraph::new(col1).scroll((*offset, 0)), cols[0]);
    frame.render_widget(Paragraph::new(col2).scroll((*offset, 0)), cols[2]);
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
    if pd.move_mode.is_some() {
        // Key hints moved to the status bar (`render_status`'s own
        // move-mode takeover) -- this row is `Constraint::Length(1)` with
        // no wrap, so the hint text was already silently truncated on a
        // narrow terminal; three distinct weights (name, badge, nothing
        // else) read more clearly than one undifferentiated ACCENT+BOLD
        // line ever did.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(pd.playlist.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled("MOVE MODE", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
            ])),
            chunks[0],
        );
    } else {
        frame.render_widget(
            Paragraph::new(filter_header(&pd.playlist.name, &pd.filter))
                .style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );
    }
    match &pd.tracks {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) => {
            // Move-mode intentionally does NOT bubble pinned tracks to the
            // top here, even though every other rendering of this list
            // does -- pinned_first is what breaks the display-position ==
            // real-array-position identity move-mode depends on. Skipping
            // it during the move keeps that identity exact regardless of
            // what's pinned, rather than blocking reorder whenever
            // anything in the playlist happens to be pinned.
            let natural = filtered_sorted(items, &pd.filter, &label);
            if pd.move_mode.is_some() {
                // The moving row gets a WARN `\u{2192}` marker independent
                // of selection (the second caller of `render_display_list_lines`,
                // after Devices) -- previously this row was visually
                // identical to any other selected row. `pd.selected` is a
                // real index into `natural` here (move mode's whole point
                // is keeping display position == real array position), so
                // the moving track's URI is looked up once, not per-row.
                let moving_uri = natural.get(pd.selected).map(|(_, t)| t.uri.as_str());
                let move_line = |t: &TrackResult| {
                    let marker = if Some(t.uri.as_str()) == moving_uri { "\u{2192} " } else { "  " };
                    Line::from(vec![Span::styled(marker, Style::default().fg(WARN)), Span::raw(label(t))])
                };
                render_display_list_lines(
                    frame,
                    chunks[1],
                    &natural,
                    pd.selected,
                    &move_line,
                    pd.filter.query.is_empty(),
                    list_state,
                );
            } else {
                let display = pinned_first(natural, &app.pinned_tracks, |t| t.uri.as_str());
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
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
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

/// The Connect queue (Phase 7): a static "currently playing" caption
/// above a plain list of what's up next. No filter/sort/pin concept --
/// unlike every other list screen, this one has no local mutation
/// surface at all (see `api::queue`'s own doc comment for why: the
/// public Web API has no remove/reorder endpoint for it), so it's the
/// one list screen that's genuinely just a view.
fn render_queue(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(area);
    frame.render_widget(Paragraph::new(screen_header_line("Queue", Some("auto-refreshing"))), chunks[0]);
    match &app.queue.fetch {
        // Full-height body, not squeezed into a 1-row caption slot --
        // `render_fetch_error`'s bordered box needs real rows to draw a
        // border in, which it never had before this fix.
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(summary) => {
            let body = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Min(1)])
                .split(chunks[1]);
            let now_playing = Line::from(vec![
                Span::styled("now playing  ", Style::default().fg(DIM)),
                Span::raw(match &summary.currently_playing {
                    Some(t) => format!("{} \u{2014} {}", t.artist, t.title),
                    None => "(nothing)".to_string(),
                }),
            ]);
            frame.render_widget(Paragraph::new(now_playing), body[0]);
            if summary.queue.is_empty() {
                render_empty_state(frame, body[1], "queue is empty", None);
                return;
            }
            let label = |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title);
            let display: Vec<(usize, &TrackResult)> = summary.queue.iter().enumerate().collect();
            render_display_list(frame, body[1], &display, app.queue.selected, &label, true, list_state);
        }
    }
}

/// Connect devices (Phase 8): a plain list, the active one marked. `r`
/// refetches manually (see `DevicesState`'s own doc comment for why this
/// doesn't poll like Queue does).
fn render_devices(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(screen_header_line("Devices", Some("Enter transfer playback, r refresh"))),
        chunks[0],
    );
    match &app.devices.fetch {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(items) if items.is_empty() => {
            render_empty_state(
                frame,
                chunks[1],
                "no devices found",
                Some("open Spotify on another device, or press r to check again"),
            );
        }
        Fetch::Ready(items) => {
            // The active marker keeps ACCENT whether or not this row is
            // also selected -- an explicitly-styled span's own color
            // patches over the row's selection style per-cell, so
            // `render_display_list_lines` (not the plain-string
            // `render_display_list`) is what makes this possible.
            // Previously the marker was plain text baked into the label,
            // so an active-but-unselected device read identically to an
            // inactive one except for the bare glyph.
            let line = |d: &crate::api::devices::DeviceSummary| {
                let marker = if d.is_active { "\u{25cf} " } else { "  " };
                let volume = d.volume_percent.map(|v| format!(", {v}%")).unwrap_or_default();
                Line::from(vec![
                    Span::styled(marker, Style::default().fg(ACCENT)),
                    Span::raw(format!("{} ({}{volume})", d.name, d.kind)),
                ])
            };
            let display: Vec<(usize, &crate::api::devices::DeviceSummary)> = items.iter().enumerate().collect();
            render_display_list_lines(frame, chunks[1], &display, app.devices.selected, &line, true, list_state);
        }
    }
}

/// The two drill-down detail screens (Artist, Album) share this shape: a
/// one-row styled header over a plain list of the thing's children.
/// Previously duplicated in full, including re-rendering the header
/// separately inside each of the three `Fetch` arms -- second concrete
/// case of the identical shell, past this codebase's own established
/// "extract on the second case" bar. `fallback_title` is what the header
/// shows before the real name is known (loading/error), so it's never
/// blank and never written three times.
#[allow(clippy::too_many_arguments)]
fn render_detail_screen<D, T>(
    frame: &mut Frame,
    area: Rect,
    detail: &Fetch<D>,
    selected: usize,
    list_state: &mut ListState,
    fallback_title: &str,
    header: impl Fn(&D) -> Line<'static>,
    items: impl Fn(&D) -> &[T],
    label: impl Fn(&T) -> String,
    empty_headline: &str,
    empty_hint: Option<&str>,
) {
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(area);
    match detail {
        Fetch::NotStarted | Fetch::Loading => {
            frame.render_widget(Paragraph::new(screen_header_line(fallback_title, None)), chunks[0]);
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            frame.render_widget(Paragraph::new(screen_header_line(fallback_title, None)), chunks[0]);
            render_fetch_error(frame, chunks[1], e);
        }
        Fetch::Ready(d) => {
            frame.render_widget(Paragraph::new(header(d)), chunks[0]);
            let child_items = items(d);
            if child_items.is_empty() {
                render_empty_state(frame, chunks[1], empty_headline, empty_hint);
                return;
            }
            let display: Vec<(usize, &T)> = child_items.iter().enumerate().collect();
            render_display_list(frame, chunks[1], &display, selected, &label, true, list_state);
        }
    }
}

/// Artist Detail (Phase 9): name + genres in the header, a plain list of
/// albums below. No top-tracks section -- see `api::artist`'s own doc
/// comment for why (Spotify removed that endpoint).
fn render_artist_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(state) = &app.artist_detail else {
        frame.render_widget(Paragraph::new("no artist selected"), area);
        return;
    };
    render_detail_screen(
        frame,
        area,
        &state.detail,
        state.selected,
        list_state,
        "Artist",
        |artist: &crate::api::artist::ArtistDetail| {
            let genres = (!artist.genres.is_empty()).then(|| artist.genres.join(", "));
            screen_header_line(&artist.name, genres.as_deref())
        },
        |artist: &crate::api::artist::ArtistDetail| artist.albums.as_slice(),
        |a: &crate::api::library::SavedAlbumSummary| a.name.clone(),
        "no albums for this artist",
        None,
    );
}

/// Album Detail (Phase 9): name + artist in the header, the track list
/// below -- `Enter` plays the album as context starting from the
/// selected track, same convention `render_playlist_detail` already
/// established.
fn render_album_detail(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let Some(state) = &app.album_detail else {
        frame.render_widget(Paragraph::new("no album selected"), area);
        return;
    };
    render_detail_screen(
        frame,
        area,
        &state.detail,
        state.selected,
        list_state,
        "Album",
        |album: &crate::api::album::AlbumDetail| {
            // Single-space em dash, matching every track label in the
            // app (including this same screen's own list) -- the
            // previous double-padded "  --  " was the only one of its
            // kind in the file.
            let meta = format!("\u{2014} {} \u{00b7} {} tracks \u{00b7} v view artist", album.artist, album.tracks.len());
            screen_header_line(&album.name, Some(&meta))
        },
        |album: &crate::api::album::AlbumDetail| album.tracks.as_slice(),
        |t: &TrackResult| format!("{} \u{2014} {}", t.artist, t.title),
        "no tracks on this album",
        None,
    );
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
            render_loading(frame, chunks[1]);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, chunks[1], e);
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

/// Renders `query` with a block cursor sitting at char index `cursor` --
/// the app's one real text-input convention. Previously open-coded
/// separately at every call site (`filter_header`, Search's own query
/// line, the text-prompt overlay); the playlist-picker and quick-jump
/// overlays skipped this entirely and drew a trailing-only cursor, which
/// actively lied -- both already support real `Left`/`Right` cursor
/// movement in their key handlers.
fn cursor_text(query: &str, cursor: usize) -> String {
    let byte_pos = query.char_indices().nth(cursor).map(|(b, _)| b).unwrap_or(query.len());
    let (before, after) = query.split_at(byte_pos);
    format!("{before}\u{2588}{after}")
}

#[cfg(test)]
mod cursor_text_tests {
    use super::*;

    #[test]
    fn cursor_at_zero_leads() {
        assert_eq!(cursor_text("hello", 0), "\u{2588}hello");
    }

    #[test]
    fn cursor_mid_string_splits_there() {
        assert_eq!(cursor_text("hello", 2), "he\u{2588}llo");
    }

    #[test]
    fn cursor_past_the_end_trails() {
        assert_eq!(cursor_text("hello", 99), "hello\u{2588}");
    }

    #[test]
    fn char_boundary_safe_on_multibyte_text() {
        // Same real fixture the SearchState cursor tests already use --
        // chars are 0:友 1:人 2:A 3:君, so cursor=2 sits immediately
        // before 'A', not mid-codepoint.
        assert_eq!(cursor_text("\u{53cb}\u{4eba}A\u{541b}", 2), "\u{53cb}\u{4eba}\u{2588}A\u{541b}");
    }
}

/// The app's screen-header row: title bold, an optional secondary fact
/// demoted beside it in `DIM` -- replaces screens that were cramming key
/// hints or extra facts into the title string at equal visual weight
/// (Devices' "(Enter: transfer, r: refresh)", Album Detail's "(v: view
/// artist)").
fn screen_header_line(title: &str, meta: Option<&str>) -> Line<'static> {
    let mut spans = vec![Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD))];
    if let Some(meta) = meta {
        spans.push(Span::styled(format!("  {meta}"), Style::default().fg(DIM)));
    }
    Line::from(spans)
}

/// `/` (start typing a filter) shows the live query with a cursor, same
/// convention as the global Search screen's own query line. Otherwise
/// shows whatever filter/sort is currently applied, if any.
fn filter_header(title: &str, filter: &ListFilter) -> String {
    if filter.editing {
        // Cursor renders at its real position, same as Search's own
        // query line -- Left/Right move it mid-string here too.
        format!("{title}  /{}", cursor_text(&filter.query, filter.cursor))
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

/// Same contract as `render_display_list`, but rows arrive as `Line`s
/// instead of a plain label string -- lets a screen color one span (a
/// pin/playing/moving marker) independently of whether that row happens
/// to be selected. Ratatui patches an explicitly-styled span's own color
/// over the row's base style per-cell, so a marker span with its own
/// `.fg(...)` keeps that color even on a selected (ACCENT+BOLD) row,
/// while any unstyled span in the same line still follows selection
/// normally. This is what makes Devices' active-device marker and
/// move-mode's moving-row arrow possible without changing
/// `render_display_list`'s own signature or its 9 existing callers.
fn render_display_list_lines<T>(
    frame: &mut Frame,
    area: Rect,
    display: &[(usize, &T)],
    selected: usize,
    line: &impl Fn(&T) -> Line<'static>,
    filter_empty: bool,
    list_state: &mut ListState,
) {
    if display.is_empty() {
        let msg = if filter_empty { "nothing here yet" } else { "no matches" };
        render_empty_state(frame, area, msg, None);
        return;
    }
    let list_items: Vec<ListItem> = display
        .iter()
        .enumerate()
        .map(|(i, (_, it))| {
            let text = line(it);
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

fn render_display_list<T>(
    frame: &mut Frame,
    area: Rect,
    display: &[(usize, &T)],
    selected: usize,
    label: &impl Fn(&T) -> String,
    filter_empty: bool,
    list_state: &mut ListState,
) {
    render_display_list_lines(frame, area, display, selected, &|it: &T| Line::from(label(it)), filter_empty, list_state);
}

fn render_sidebar(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let rows = sidebar_rows(app);
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len() + 1);
    let mut saw_playlists_header = false;
    for (i, row) in rows.iter().enumerate() {
        if matches!(row, SidebarRow::Playlist(_)) && !saw_playlists_header {
            items.push(ListItem::new("PLAYLISTS").style(Style::default().fg(DIM)));
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
        Style::default().fg(DIM)
    }
}

#[cfg(test)]
mod repeat_mode_tests {
    use super::*;

    #[test]
    fn flags_map_to_the_mode_the_player_is_really_in() {
        assert_eq!(RepeatMode::from_flags(false, false), RepeatMode::Off);
        assert_eq!(RepeatMode::from_flags(true, false), RepeatMode::Context);
        assert_eq!(RepeatMode::from_flags(true, true), RepeatMode::Track);
    }

    #[test]
    fn a_track_flag_alone_still_means_repeat_song() {
        // Another device (or a mid-toggle event) can report the track flag
        // without the context flag; the track flag wins either way.
        assert_eq!(RepeatMode::from_flags(false, true), RepeatMode::Track);
    }

    #[test]
    fn next_cycles_off_album_song_off() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::Context);
        assert_eq!(RepeatMode::Context.next(), RepeatMode::Track);
        assert_eq!(RepeatMode::Track.next(), RepeatMode::Off);
    }

    #[test]
    fn each_mode_survives_a_round_trip_through_its_own_flags() {
        for mode in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
            let (context, track) = mode.flags();
            assert_eq!(RepeatMode::from_flags(context, track), mode);
        }
    }
}

#[cfg(test)]
mod shuffle_mode_tests {
    use super::*;

    #[test]
    fn the_flags_map_to_the_three_states() {
        assert_eq!(ShuffleMode::from_flags(false, false), ShuffleMode::Off);
        assert_eq!(ShuffleMode::from_flags(true, false), ShuffleMode::On);
        assert_eq!(ShuffleMode::from_flags(true, true), ShuffleMode::Smart);
    }

    #[test]
    fn smart_without_shuffle_is_just_off() {
        // A stale smart flag can't outlive shuffle itself.
        assert_eq!(ShuffleMode::from_flags(false, true), ShuffleMode::Off);
    }

    #[test]
    fn the_key_cycles_off_then_shuffle_then_smart_then_off() {
        assert_eq!(ShuffleMode::Off.next(), ShuffleMode::On);
        assert_eq!(ShuffleMode::On.next(), ShuffleMode::Smart);
        assert_eq!(ShuffleMode::Smart.next(), ShuffleMode::Off);
    }

    #[test]
    fn a_full_cycle_returns_to_the_start() {
        let start = ShuffleMode::On;
        assert_eq!(start.next().next().next(), start);
    }

    #[test]
    fn smart_shuffle_implies_shuffle() {
        assert!(!ShuffleMode::Off.shuffle());
        assert!(ShuffleMode::On.shuffle());
        assert!(ShuffleMode::Smart.shuffle());
    }

    #[test]
    fn status_labels_name_the_mode() {
        assert_eq!(ShuffleMode::Off.status_label(), "shuffle off");
        assert_eq!(ShuffleMode::On.status_label(), "shuffle on");
        assert!(ShuffleMode::Smart.status_label().starts_with("smart shuffle on"));
    }
}

#[cfg(test)]
mod playback_modes_tests {
    use super::*;

    #[test]
    fn both_glyphs_are_always_present_even_with_everything_off() {
        let [shuffle, smart, repeat] = playback_modes(false, false, RepeatMode::Off);
        assert_eq!(shuffle, ("\u{21c4}", false));
        assert_eq!(smart, (" ", false));
        assert_eq!(repeat, ("\u{21bb} ", false));
    }

    #[test]
    fn shuffle_lights_up_on_its_own() {
        let [shuffle, smart, repeat] = playback_modes(true, false, RepeatMode::Off);
        assert!(shuffle.1);
        assert!(!smart.1);
        assert!(!repeat.1);
    }

    #[test]
    fn smart_shuffle_fills_the_gap_between_the_toggles_with_a_lit_sparkle() {
        let [shuffle, smart, _] = playback_modes(true, true, RepeatMode::Off);
        assert!(shuffle.1, "smart shuffle is still shuffle");
        assert_eq!(smart, ("\u{2726}", true));
    }

    #[test]
    fn the_sparkle_needs_shuffle_itself_to_be_on() {
        // A stale smart flag with shuffle off must not draw a sparkle.
        let [_, smart, _] = playback_modes(false, true, RepeatMode::Off);
        assert_eq!(smart, (" ", false));
    }

    #[test]
    fn repeat_album_and_repeat_song_are_both_active_but_only_song_gets_the_one() {
        let [_, _, context] = playback_modes(false, false, RepeatMode::Context);
        let [_, _, track] = playback_modes(false, false, RepeatMode::Track);
        assert_eq!(context, ("\u{21bb} ", true));
        assert_eq!(track, ("\u{21bb}1", true));
    }

    #[test]
    fn the_readout_is_the_same_width_in_every_state() {
        // A fixed-width readout is what lets the playbar reserve its room
        // once -- otherwise the track title's truncation point would jump
        // every time shuffle or repeat changed.
        let width = |shuffle, smart, repeat| {
            playback_modes(shuffle, smart, repeat).iter().map(|(text, _)| text.chars().count()).sum::<usize>()
        };
        let expected = width(false, false, RepeatMode::Off);
        for shuffle in [false, true] {
            for smart in [false, true] {
                for repeat in [RepeatMode::Off, RepeatMode::Context, RepeatMode::Track] {
                    assert_eq!(
                        width(shuffle, smart, repeat),
                        expected,
                        "shuffle={shuffle} smart={smart} repeat={repeat:?}"
                    );
                }
            }
        }
    }
}

fn render_playbar(frame: &mut Frame, app: &AppState, area: Rect) {
    // 28 is the room the icon, time, and volume readouts already need; the
    // always-present shuffle/repeat toggles (plus their 3-space gap) come
    // out of the title's share.
    let title_room = (area.width as usize).saturating_sub(28 + 3 + PLAYBACK_MODES_WIDTH);
    let [shuffle, smart, repeat] = playback_modes(app.shuffle, app.smart_shuffle, app.repeat);
    let toggle_style = |on: bool| Style::default().fg(if on { ACCENT } else { DIM });
    let spans = vec![
        Span::raw(format!(
            "{} {}   {}   {}   ",
            playing_icon(app),
            header(app, title_room),
            time_readout(app),
            volume_readout(app),
        )),
        Span::styled(shuffle.0, toggle_style(shuffle.1)),
        Span::styled(smart.0, toggle_style(smart.1)),
        Span::styled(repeat.0, toggle_style(repeat.1)),
    ];
    frame.render_widget(
        Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::TOP)),
        area,
    );
}

fn render_status(frame: &mut Frame, app: &AppState, area: Rect) {
    // Move mode's key hints live here, not in the header row (which is
    // `Constraint::Length(1)` with no wrap, so the old placement was
    // already silently truncated on a narrow terminal) -- checked before
    // `app.status` since both `Enter`/`Esc` in move mode already call
    // `pd.move_mode.take()` before dispatching a reorder, so no mutation
    // result can ever land while this branch would also be showing.
    if *app.nav.top() == Screen::PlaylistDetail
        && app.playlist_detail.as_ref().is_some_and(|pd| pd.move_mode.is_some())
    {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("MOVE MODE", Style::default().fg(WARN).add_modifier(Modifier::BOLD)),
                Span::styled(
                    "  \u{2191}/\u{2193} relocate, g jump to position, Enter confirm, Esc cancel",
                    Style::default().fg(DIM),
                ),
            ])),
            area,
        );
        return;
    }
    // A Phase 5 mutation's result (success or failure) takes over this
    // line until the next keypress, same lifetime a status line
    // conventionally gets -- the depth readout resumes once it's gone.
    if let Some((message, is_error)) = &app.status {
        let color = if *is_error { DANGER } else { ACCENT };
        frame.render_widget(Paragraph::new(message.clone()).style(Style::default().fg(color)), area);
        return;
    }
    let text = format!("stack depth {} \u{2014} Tab switch pane, Esc back", app.nav.depth());
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)), area);
}

fn render_search(frame: &mut Frame, app: &AppState, list_state: &mut ListState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    // Cursor renders at its real position, not always trailing -- Left/Right
    // now move it mid-string (arrow-key editing, reported live as missing).
    let query_line = format!("/ {}", cursor_text(&app.search.query, app.search.cursor));
    frame.render_widget(
        Paragraph::new(query_line).block(Block::default().borders(Borders::ALL).title("search")),
        chunks[0],
    );

    if app.search.searching {
        render_loading(frame, chunks[1]);
        return;
    }
    if let Some(err) = &app.search.error {
        render_fetch_error(frame, chunks[1], err);
        return;
    }
    if app.search.results.is_empty() {
        let headline = if !app.search.client_ready {
            "search not ready yet (loading Spotify auth\u{2026})"
        } else if app.search.query.is_empty() {
            "type a query, then Enter to search, Esc to cancel"
        } else {
            // Distinct from the empty-query message on purpose: this is
            // the state reported live as "have to click enter first and
            // then scroll" -- clarifying *why* up/down do nothing yet
            // (there's a real Web API call to make, not a local list to
            // narrow) rather than leaving it looking broken or identical
            // to having typed nothing at all.
            "press Enter to search \u{2014} this hits Spotify directly, not a live filter like Library's /"
        };
        render_empty_state(frame, chunks[1], headline, None);
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
        DIM // frozen/paused reads as visually "asleep"
    } else {
        ACCENT
    };
    Gauge::default().gauge_style(Style::default().fg(color)).label("").ratio(progress_ratio(app))
}

/// Same gauge, with a border -- used only by the fullscreen layouts
/// (which have a whole extra row to spare for it), not the compact view.
/// At low progress (a song's first few seconds) an unbordered gauge is
/// almost entirely its own background color, which reads as "a tiny
/// colored square" with no visible indication of where the bar actually
/// ends. The border always outlines the full capsule regardless of how
/// little of it is filled.
fn progress_gauge_bordered(app: &AppState) -> Gauge<'static> {
    progress_gauge(app).block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(DIM)))
}

/// What a lyric line shows: its romanization (text and re-timed words) when
/// romanized lyrics are on and this line has one, else the native line. A
/// romanized line whose words couldn't be re-timed is drawn whole rather than
/// swept with the native words, which would colour the wrong text.
fn display_line<'a>(
    line: &'a crate::lyrics::LyricLine,
    roman: Option<&'a crate::romanize::RomanLine>,
    romanize: bool,
) -> (&'a str, &'a [crate::lyrics::WordSeg]) {
    match roman {
        Some(roman) if romanize => (roman.text.as_str(), roman.words.as_slice()),
        _ => (line.text.as_str(), line.words.as_slice()),
    }
}

/// The text lines of unsynced lyrics as they should be shown: romanized where
/// romanization is on and a line has one, native otherwise. Blank lines stay
/// so the layout doesn't shift, and a result that doesn't line up with the
/// text (a different number of lines) is ignored rather than misplaced.
fn plain_display_lines(
    text: &str,
    roman: Option<&[Option<crate::romanize::RomanLine>]>,
    romanize: bool,
) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let roman = roman.filter(|r| romanize && r.len() == lines.len());
    lines
        .iter()
        .enumerate()
        .map(|(i, native)| roman.and_then(|r| r[i].as_ref()).map_or_else(|| (*native).to_string(), |r| r.text.clone()))
        .collect()
}

fn body_lines(app: &AppState) -> Vec<Line<'static>> {
    match &app.lyrics {
        // `header()` (this screen's title line, and the persistent
        // playback bar's idle text) already carries the "press / to
        // search" instruction -- this used to repeat the identical
        // sentence here too, so an idle Now Playing screen showed it
        // twice in the same frame. This says something lyrics-area-
        // appropriate instead, matching the tone of the other
        // non-synced states below (e.g. `SessionEnded`'s own distinct
        // line) rather than duplicating the header's.
        LyricsState::Idle => vec![Line::from("nothing playing yet")],
        LyricsState::SessionEnded => vec![
            Line::from("session disconnected"),
            Line::from("restart spot-tui to reconnect"),
        ],
        LyricsState::Loading => vec![Line::from("fetching lyrics\u{2026}")],
        LyricsState::Instrumental => vec![Line::from("\u{266a} instrumental")],
        LyricsState::NotFound => vec![Line::from("no lyrics found")],
        LyricsState::Plain(text) => vec![Line::from("(unsynced)")]
            .into_iter()
            .chain(
                plain_display_lines(text, app.romanized_lines.as_deref(), app.romanize_lyrics)
                    .into_iter()
                    .map(Line::from),
            )
            .collect(),
        // Shows the whole sheet, not a windowed few lines around the
        // current one -- matches official Spotify's own default lyrics
        // view. `render_now_playing_hero`/`render_fullscreen_hero` are
        // responsible for scrolling the viewport to keep the current
        // line visible (see `center_current_line`); this function just
        // decides what every line looks like, not which ones show.
        // A blank line follows every real one -- ratatui packs lines
        // edge to edge by default, which read as cramped next to the
        // reference's generous line height. Each real line occupies 2
        // rendered rows now, so `current_body_line_row` doubles the
        // current-line index to match when it centers the viewport.
        LyricsState::Synced(lines) => {
            if lines.is_empty() {
                return vec![Line::from("no lyrics found")];
            }
            let current = app.current_line.unwrap_or(0);
            let mut out = Vec::with_capacity(lines.len() * 2);
            let romanized = app.romanized_lines.as_deref().filter(|r| r.len() == lines.len());
            for (i, line) in lines.iter().enumerate() {
                let (shown, words) =
                    display_line(line, romanized.and_then(|r| r[i].as_ref()), app.romanize_lyrics);
                let text = if shown.is_empty() { "\u{266a}".to_string() } else { shown.to_string() };
                let styled = if i == current && !words.is_empty() {
                    // Word-by-word: same text, coloured by how far the voice
                    // has got. Sung = accent, still to come = white, both bold
                    // so nothing shifts as the sweep passes.
                    let sung = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
                    let unsung = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
                    Line::from(
                        sweep_runs(words, app.position.as_secs_f64())
                            .into_iter()
                            .map(|(run, fill)| Span::styled(run, if fill == Fill::Sung { sung } else { unsung }))
                            .collect::<Vec<_>>(),
                    )
                } else if i == current {
                    Line::from(Span::styled(text, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
                } else {
                    Line::from(Span::styled(text, Style::default().fg(lyric_tier_color(i.abs_diff(current)))))
                };
                out.push(styled);
                out.push(Line::from(""));
            }
            out
        }
    }
}

/// The 4-tier fade by distance from the current line -- `Color::DarkGray`
/// alone read as ~1.4:1 contrast against this app's near-black
/// background, functionally unreadable for a screen built to show the
/// whole sheet, not just the current line. Used by every `body_lines`
/// caller, compact and fullscreen alike, so they can't drift apart.
fn lyric_tier_color(distance: usize) -> Color {
    match distance {
        0 => ACCENT,
        1 => Color::White,
        2..=3 => Color::Gray,
        _ => DIM,
    }
}

/// Whether a stretch of the current line has been sung yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    Sung,
    Unsung,
}

/// The current line as contiguous runs of sung / unsung text at `pos_secs`,
/// for word-by-word highlighting. Inside the word being sung, the sung part
/// is `floor(fraction * chars)` characters -- a real sweep, since a terminal
/// can colour per character -- and a word's trailing space is only sung once
/// the word is finished. Each segment is judged on its own clock, so a
/// background vocal that overlaps the lead sweeps independently. The runs
/// always concatenate to exactly the segments' text (the renderer must never
/// change what is drawn or how it wraps), and adjacent runs with the same
/// fill are merged.
pub fn sweep_runs(words: &[crate::lyrics::WordSeg], pos_secs: f64) -> Vec<(String, Fill)> {
    fn push(runs: &mut Vec<(String, Fill)>, text: &str, fill: Fill) {
        if text.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some((last, last_fill)) if *last_fill == fill => last.push_str(text),
            _ => runs.push((text.to_string(), fill)),
        }
    }

    let mut runs = Vec::new();
    for word in words {
        if pos_secs >= word.end && pos_secs >= word.start {
            push(&mut runs, &word.text, Fill::Sung);
        } else if pos_secs <= word.start {
            push(&mut runs, &word.text, Fill::Unsung);
        } else {
            let body = word.text.trim_end();
            let chars = body.chars().count();
            let fraction = (pos_secs - word.start) / (word.end - word.start);
            let sung = ((fraction * chars as f64).floor() as usize).min(chars);
            let split = body.char_indices().nth(sung).map_or(body.len(), |(i, _)| i);
            push(&mut runs, &word.text[..split], Fill::Sung);
            push(&mut runs, &word.text[split..], Fill::Unsung);
        }
    }
    runs
}

/// True while a word sweep is actually moving: playing, and the current line
/// has word timing. Only then does the event loop redraw faster than its
/// normal tick, so nothing else (line-level lyrics, paused playback) pays for
/// the extra frames.
pub fn word_sweep_active(lyrics: &LyricsState, current_line: Option<usize>, playing: Option<bool>) -> bool {
    if playing != Some(true) {
        return false;
    }
    match (lyrics, current_line) {
        (LyricsState::Synced(lines), Some(i)) => lines.get(i).is_some_and(|line| !line.words.is_empty()),
        _ => false,
    }
}

/// Only `Synced` has a real "current line" to center on -- every other
/// `LyricsState` (idle/instrumental/not-found/plain/loading) has no
/// notion of a current line at all, so they always render from the top.
/// `*2`: `body_lines` interleaves a blank spacer after every real line,
/// so the current line's actual row in the rendered `Vec` is twice its
/// index into the raw synced-lyrics data.
fn current_body_line_row(app: &AppState) -> Option<usize> {
    match &app.lyrics {
        LyricsState::Synced(lines) if !lines.is_empty() => Some(app.current_line.unwrap_or(0) * 2),
        _ => None,
    }
}

/// A `Line`'s real on-screen height once `Paragraph`'s own `Wrap` gets to
/// it -- 1 for a blank spacer (nothing to wrap), otherwise the same
/// greedy word-wrap `wrapped_line_count` already uses to size the
/// confirm overlay. Needed because `Paragraph::scroll`'s `y` counts
/// *wrapped* rows, not logical `Line`s (confirmed by reading
/// `ratatui-widgets`' `Paragraph::render_paragraph`: "the scroll offset
/// is applied after the text is wrapped") -- a centering formula that
/// assumes 1 row per `Line` silently drifts further off-center every
/// time an earlier line actually wraps to more than one row, which is
/// exactly what a long lyric line in a narrower pane does. Reported
/// live as the current line reading progressively lower down the screen
/// the further into the song it got -- each additional wrapped line
/// above it added rows this function wasn't accounting for.
fn line_row_height(line: &Line<'static>, width: u16) -> usize {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text.trim().is_empty() { 1 } else { wrapped_line_count(&text, width) as usize }
}

/// The current line's own vertical middle, in real wrapped-row units
/// (`line_row_height`), counting from the top of `lines` -- shared by
/// both `center_current_line` (fullscreen) and `top_anchored_offset`
/// (compact), so they can't drift apart on the wrap-awareness fix even
/// though they anchor to different screen positions.
fn anchor_row_of(lines: &[Line<'static>], current_row: usize, width: u16) -> usize {
    let heights: Vec<usize> = lines.iter().map(|l| line_row_height(l, width)).collect();
    let rows_before_current: usize = heights[..current_row.min(heights.len())].iter().sum();
    let current_height = heights.get(current_row).copied().unwrap_or(1);
    rows_before_current + current_height / 2
}

fn total_row_height(lines: &[Line<'static>], width: u16) -> usize {
    lines.iter().map(|l| line_row_height(l, width)).sum()
}

/// Pads `lines` with `viewport_height / 2` blank rows above and below,
/// and returns the scroll offset that puts the current line's own
/// vertical middle at the exact vertical middle of the viewport --
/// measured in real wrapped rows (`anchor_row_of`), not logical `Line`
/// count, so it stays correct however many of the preceding lines
/// happen to wrap. A plain clamped scroll offset (`ideal =
/// current.saturating_sub(half); ideal.min(total - viewport)`) can't
/// center at either edge of the sheet either -- there's no real content
/// to scroll into above line 0 or below the last line, so a song's
/// opening (or closing) line rendered pinned to the top (or bottom)
/// instead of centered, also reported live. Padding with real blank
/// rows gives the offset somewhere to scroll into even there, so the
/// current line centers unconditionally, including a song's first and
/// last line and a current line that itself wraps to more than one row.
/// Fullscreen only -- see `top_anchored_offset` for the compact view,
/// which was explicitly asked *not* to center this way.
fn center_current_line(
    lines: Vec<Line<'static>>,
    current_row: Option<usize>,
    viewport_height: u16,
    width: u16,
) -> (Vec<Line<'static>>, u16) {
    let Some(current_row) = current_row else {
        // No current line to anchor on -- this is a short status message
        // (Loading/"fetching lyrics...", Idle, Instrumental, NotFound,
        // SessionEnded), not a lyric sheet. It still renders through this
        // same fullscreen paragraph, so it needs the same vertical-center
        // treatment real lyrics get here, rather than sitting pinned to
        // the pane's top edge -- reported live ("loading lyrics text is
        // so high - center it like the actual lyrics").
        let total_rows = total_row_height(&lines, width);
        let pad_top = (viewport_height as usize).saturating_sub(total_rows) / 2;
        let mut padded = Vec::with_capacity(lines.len() + pad_top);
        padded.extend(std::iter::repeat_with(|| Line::from("")).take(pad_top));
        padded.extend(lines);
        return (padded, 0);
    };
    let anchor_row = anchor_row_of(&lines, current_row, width);
    let total_rows = total_row_height(&lines, width);

    let half = (viewport_height / 2) as usize;
    let mut padded = Vec::with_capacity(lines.len() + half * 2);
    padded.extend(std::iter::repeat_with(|| Line::from("")).take(half));
    padded.extend(lines);
    padded.extend(std::iter::repeat_with(|| Line::from("")).take(half));

    let padded_total_rows = total_rows + half * 2;
    let max_offset = padded_total_rows.saturating_sub(viewport_height as usize);
    let offset = anchor_row.min(max_offset) as u16;
    (padded, offset)
}

/// The compact (non-fullscreen) Now Playing view's lyrics scroll: keeps
/// a couple of already-seen lines visible above the current one instead
/// of forcing it to the vertical middle the way `center_current_line`
/// does -- explicitly asked for over centering ("in now playing have it
/// at the top, not middle"), since centering there ate a large, fixed
/// share of an already-small pane with blank padding on every render,
/// which is what made the actually-rendered lyric text read as smaller
/// even though nothing about its size had changed. No padding here:
/// unlike the fullscreen view, "settle at the top" (song start) and
/// "settle at the bottom" (song end, once there's more sheet than fits)
/// are both already correct, ordinary scrolling behavior, the same as
/// every other list in this app -- there's nothing to fabricate.
fn top_anchored_offset(lines: &[Line<'static>], current_row: Option<usize>, viewport_height: u16, width: u16) -> u16 {
    const TOP_MARGIN: usize = 2;
    let Some(current_row) = current_row else { return 0 };
    let anchor_row = anchor_row_of(lines, current_row, width);
    let total_rows = total_row_height(lines, width);
    let max_offset = total_rows.saturating_sub(viewport_height as usize);
    anchor_row.saturating_sub(TOP_MARGIN).min(max_offset) as u16
}

#[cfg(test)]
mod center_current_line_tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(i.to_string())).collect()
    }

    fn line_text(l: &Line<'static>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn no_current_row_vertically_centers_a_short_status_message() {
        // 10-row viewport, 5-row message -- pad_top = (10 - 5) / 2 = 2.
        let (out, offset) = center_current_line(lines(5), None, 10, 80);
        assert_eq!(out.len(), 2 + 5);
        assert_eq!(offset, 0);
        assert_eq!(line_text(&out[0]), "");
        assert_eq!(line_text(&out[2]), "0");
    }

    #[test]
    fn no_current_row_with_a_message_taller_than_the_viewport_pads_nothing() {
        let (out, offset) = center_current_line(lines(20), None, 10, 80);
        assert_eq!(out.len(), 20);
        assert_eq!(offset, 0);
    }

    #[test]
    fn the_very_first_line_still_centers_via_top_padding() {
        // viewport 10, half = 5 -- 5 blank rows padded above line 0 means
        // scrolling 0 rows still puts line 0 at screen row 5, dead center.
        let (out, offset) = center_current_line(lines(40), Some(0), 10, 80);
        assert_eq!(offset, 0);
        assert_eq!(out.len(), 40 + 5 + 5);
        assert_eq!(out[5].spans[0].content.as_ref(), "0");
    }

    #[test]
    fn the_very_last_line_still_centers_via_bottom_padding() {
        let (out, offset) = center_current_line(lines(40), Some(39), 10, 80);
        // padded index of line 39 is 5 (top pad) + 39 = 44; centering
        // needs it at screen row 5, so offset = 44 - 5 = 39.
        assert_eq!(offset, 39);
        let screen_row = 44 - offset as usize;
        assert_eq!(screen_row, 5);
        assert_eq!(out[44].spans[0].content.as_ref(), "39");
    }

    #[test]
    fn a_middle_line_centers_at_the_viewport_midpoint() {
        // padded index of line 20 is 5 (top pad) + 20 = 25; offset 20
        // puts it at screen row 5, dead center of a 10-row viewport.
        let (_, offset) = center_current_line(lines(40), Some(20), 10, 80);
        assert_eq!(offset, 20);
        assert_eq!(25 - offset as usize, 5);
    }

    #[test]
    fn a_wrapped_earlier_line_does_not_push_the_current_line_off_center() {
        // "one two three four five" greedy-wraps to 3 rows at width 10.
        // A Line-count-based offset (the old bug) would put line 1's
        // anchor at row 1; the real wrapped anchor is row 3 (after the
        // 3 wrapped rows line 0 actually consumes).
        let lines = vec![Line::from("one two three four five"), Line::from("current")];
        let (_, offset) = center_current_line(lines, Some(1), 10, 10);
        assert_eq!(offset, 3);
    }

    #[test]
    fn a_current_line_that_itself_wraps_anchors_at_its_own_middle() {
        // "current line" (12 chars) wraps to 2 rows against width 6;
        // anchoring at its top row (old behavior) would sit it half a
        // row high of true center -- anchor should land mid-way through
        // its own wrapped block instead.
        let lines = vec![Line::from("current line")];
        let (_, offset) = center_current_line(lines, Some(0), 10, 6);
        // current_height = wrapped_line_count("current line", 6) = 2;
        // anchor_row = 0 + 2/2 = 1.
        assert_eq!(offset, 1);
    }
}

#[cfg(test)]
mod top_anchored_offset_tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(i.to_string())).collect()
    }

    #[test]
    fn no_current_row_is_zero() {
        assert_eq!(top_anchored_offset(&lines(5), None, 10, 80), 0);
    }

    #[test]
    fn a_song_s_opening_line_stays_pinned_to_the_actual_top() {
        // No padding, no forced centering -- current row 0 needs no
        // scroll at all, unlike `center_current_line`'s offset 0 which
        // only reads as centered because of the padding it adds.
        assert_eq!(top_anchored_offset(&lines(40), Some(0), 10, 80), 0);
    }

    #[test]
    fn a_middle_line_keeps_a_small_margin_of_context_above_it() {
        // anchor_row = 20; margin 2 -- offset settles 2 rows short of
        // the current line, not at the viewport's vertical middle.
        assert_eq!(top_anchored_offset(&lines(40), Some(20), 10, 80), 18);
    }

    #[test]
    fn near_the_end_clamps_to_the_real_bottom_not_past_it() {
        // total 40, viewport 10 -- max_offset 30. anchor_row 39 minus
        // margin 2 = 37, clamped down to 30 (ordinary scroll-to-end).
        assert_eq!(top_anchored_offset(&lines(40), Some(39), 10, 80), 30);
    }
}

const ART_MIN_WIDTH: u16 = 14;
const ART_MAX_WIDTH: u16 = 26;
/// The compact hero's gauge sits beside the art in the (usually much
/// wider) text column, unlike the fullscreen layouts' gauge, which
/// shares the same narrow column as the art and is already
/// `capsule_row`-matched to it. Left unconstrained, a bordered gauge
/// there stretches to the full text-column width on a wide terminal --
/// mostly empty bordered space -- reported live as "stretches for so
/// long in empty space." Capped at a fixed, modest width instead of
/// matching the art (the two aren't stacked in the same column here, so
/// there's no natural width to match).
const COMPACT_GAUGE_MAX_WIDTH: u16 = 44;

/// The real per-cell pixel size, for sizing an art card to an actual
/// pixel square instead of guessing a fixed ratio. Reads
/// `Picker::font_size()` directly -- `main.rs` corrects that stored
/// value once at startup (via the same OS `window_size` ioctl this
/// function used to call itself) specifically so this and
/// `ratatui-image`'s own internal image encoder agree on the same real
/// cell size; calling the ioctl again independently here, after that
/// fix, is exactly what caused the two to *disagree* the first time
/// this bug was chased (this app's layout math using one freshly-
/// queried value while the encoder kept using `Picker`'s own separate,
/// uncorrected one) -- confirmed live as a visibly pixelated card, the
/// transmitted image encoded at a different, lower resolution than the
/// cells this app's math stretched it across. One corrected value, read
/// from one place, fixes both. Falls back to a flat 2:1 guess only when
/// there's no real `Picker` at all (no graphics protocol in use).
fn real_cell_size(picker: Option<&ratatui_image::picker::Picker>) -> (u16, u16) {
    match picker.map(|p| p.font_size()) {
        Some(font) if font.width > 0 && font.height > 0 => (font.width, font.height),
        _ => (1, 2),
    }
}

/// `render_art` always wraps the image in `Borders::ALL`, which removes
/// exactly 1 cell per side (2 total) from both width and height before
/// the image itself ever gets drawn. That flat cell subtraction removes
/// a *different number of real pixels* on each axis whenever a cell
/// isn't exactly square (which real fonts never are) -- taller cells
/// mean the 2 rows taken for the border cost more real vertical pixels
/// than the 2 columns cost horizontally. The smaller the card, the
/// larger that skew is as a fraction of the whole: negligible on
/// fullscreen's 50-70-cell-wide cards, but large enough on the compact
/// hero's much smaller `ART_MAX_WIDTH = 26` card to read as a real,
/// reported gap on one side once the image (still correctly square in
/// itself, since `Resize::Scale` never distorts it) didn't fill the
/// remaining space. Squaring the *inner*, post-border region -- not the
/// outer card size -- and only then adding the border back is what
/// actually keeps the finished, bordered card itself square.
const ART_BORDER_CELLS: u16 = 2;

/// Pure square-sizing math, given an already-known real cell pixel size
/// (`real_cell_size`) -- kept separate from that detection so this part
/// stays a plain, environment-free function to unit test. Returns the
/// *outer* (pre-border) height needed so that the card's inner,
/// post-border region is a true pixel square -- see `ART_BORDER_CELLS`.
fn square_height_cells(width_cells: u16, cell_size: (u16, u16)) -> u16 {
    let (cell_w, cell_h) = cell_size;
    if cell_h == 0 {
        return (width_cells / 2).max(1);
    }
    let inner_width = width_cells.saturating_sub(ART_BORDER_CELLS).max(1);
    let inner_height = ((inner_width as u32 * cell_w as u32) / cell_h as u32).max(1) as u16;
    inner_height + ART_BORDER_CELLS
}

/// The inverse of `square_height_cells`, for the one call site that
/// picks a height first and needs the matching square width (the
/// narrow-terminal fullscreen fallback). Same border-aware math, mirrored.
fn square_width_cells(height_cells: u16, cell_size: (u16, u16)) -> u16 {
    let (cell_w, cell_h) = cell_size;
    if cell_w == 0 {
        return (height_cells * 2).max(1);
    }
    let inner_height = height_cells.saturating_sub(ART_BORDER_CELLS).max(1);
    let inner_width = ((inner_height as u32 * cell_h as u32) / cell_w as u32).max(1) as u16;
    inner_width + ART_BORDER_CELLS
}

#[cfg(test)]
mod square_cells_tests {
    use super::*;

    #[test]
    fn zero_cell_dimension_falls_back_to_the_2_to_1_approximation() {
        assert_eq!(square_height_cells(26, (0, 0)), 13);
        assert_eq!(square_width_cells(13, (0, 0)), 26);
    }

    #[test]
    fn exact_2_to_1_cell_size_still_needs_one_extra_row_for_the_border() {
        // Naive flat math (26/2) says 13, but that's the *inner* square's
        // height -- the outer, bordered card needs 2 more cells of
        // height than a naive width/2 would suggest, then squared
        // against the inner width (24, not 26): 24*8/16 = 12, +2 border
        // rows = 14.
        assert_eq!(square_height_cells(26, (8, 16)), 14);
    }

    #[test]
    fn a_taller_real_cell_needs_fewer_rows_for_the_same_square() {
        // 8x18 (2.25:1) is taller-per-cell than the flat 2:1 fallback
        // assumes -- fewer rows are needed to reach the same real-pixel
        // width.
        let height = square_height_cells(26, (8, 18));
        assert!(height < 13, "expected fewer than the 2:1 fallback's 13 rows, got {height}");
    }

    #[test]
    fn a_wider_real_cell_needs_more_rows_for_the_same_square() {
        // 8x12 (1.5:1) is wider-per-cell (relatively) than the flat 2:1
        // fallback assumes -- more rows are needed, not fewer. This is
        // the actual shape of the live-reported bug: a wrongly-trusted
        // font size closer to this end of the ratio than assumed is
        // what left the card too wide (too few rows) relative to a true
        // square.
        let height = square_height_cells(26, (8, 12));
        assert!(height > 13, "expected more than the 2:1 fallback's 13 rows, got {height}");
    }

    #[test]
    fn the_inner_post_border_region_is_the_true_square_not_the_outer_card() {
        // The actual property this fix exists for: it's the *inner*
        // (post-border) region that must be a real pixel square, not
        // the outer card -- verified directly here rather than only
        // indirectly through the exact-2:1 case above.
        let cell = (8u32, 20u32); // 2.5:1, deliberately not exactly 2:1
        let outer_width = 26u16;
        let outer_height = square_height_cells(outer_width, (cell.0 as u16, cell.1 as u16));
        let inner_width = (outer_width - ART_BORDER_CELLS) as u32;
        let inner_height = (outer_height - ART_BORDER_CELLS) as u32;
        let (inner_w_px, inner_h_px) = (inner_width * cell.0, inner_height * cell.1);
        assert!(
            inner_w_px.abs_diff(inner_h_px) <= cell.1,
            "inner region not square: {inner_w_px}px wide vs {inner_h_px}px tall"
        );
    }

    #[test]
    fn square_width_cells_round_trips_within_integer_rounding_slack() {
        // Two floor-divisions in a row (width->height, then height back
        // to width) can lose at most a couple of cells to truncation --
        // not an exact inverse, but close enough that the resulting card
        // still reads as square, which is all this is for.
        let width = 26;
        let height = square_height_cells(width, (8, 18));
        let round_tripped = square_width_cells(height, (8, 18));
        assert!(
            width.abs_diff(round_tripped) <= 2,
            "expected {round_tripped} to be within 2 cells of the original {width}"
        );
    }
}

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

/// A visible frame around the fill, not a borderless rectangle -- reads
/// as a distinct thumbnail card sitting on the pane background, closer
/// to the reference's crisp album-art card, instead of the color block
/// blending into whatever's behind it with no edge at all.
/// A fixed-width strip horizontally centered within a wider one. Applied
/// to two rows of the *same* parent column (the art card's row and the
/// progress gauge's row), it returns byte-identical x/width for both --
/// the actual fix for the gauge stretching to the full column while the
/// art card above it stayed narrow: they were computing their own
/// centering independently before, which is how the two drifted apart.
fn capsule_row(area: Rect, width: u16) -> Rect {
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(width), Constraint::Min(0)])
        .split(area)[1]
}

#[cfg(test)]
mod capsule_row_tests {
    use super::*;

    #[test]
    fn the_gauge_capsule_lands_on_the_same_columns_as_the_art_card() {
        let column = Rect::new(3, 0, 100, 40);
        let art = capsule_row(Rect { y: 2, height: 20, ..column }, 44);
        let gauge = capsule_row(Rect { y: 26, height: 1, ..column }, 44);
        assert_eq!((art.x, art.width), (gauge.x, gauge.width));
    }
}

/// Scales `image` up just enough that it fully covers a `target_w` x
/// `target_h` box (never leaves either axis short), then center-crops
/// whatever overflows on the other axis -- CSS `background-size: cover`,
/// not `contain`. See `render_art`'s own doc comment on this call site for
/// why: `ratatui-image`'s `Resize::Scale`/`Fit` both *fit within* their
/// target (confirmed by reading its `resize_pixels`), so any mismatch
/// between an integer terminal-cell count and the real pixel square it's
/// meant to approximate always showed up as a blank letterboxed gap, never
/// distortion. Doing the crop ourselves, before `ratatui-image` ever sees
/// the image, means there's no aspect mismatch left for it to mishandle.
fn cover_crop(image: &image::DynamicImage, target_w: u32, target_h: u32) -> image::DynamicImage {
    let (target_w, target_h) = (target_w.max(1), target_h.max(1));
    let (src_w, src_h) = (image.width().max(1), image.height().max(1));
    let scale = (target_w as f64 / src_w as f64).max(target_h as f64 / src_h as f64);
    let scaled_w = ((src_w as f64 * scale).ceil() as u32).max(target_w);
    let scaled_h = ((src_h as f64 * scale).ceil() as u32).max(target_h);
    let scaled = image.resize_exact(scaled_w, scaled_h, image::imageops::FilterType::Lanczos3);
    let x = (scaled_w - target_w) / 2;
    let y = (scaled_h - target_h) / 2;
    scaled.crop_imm(x, y, target_w, target_h)
}

#[cfg(test)]
mod cover_crop_tests {
    use super::*;

    fn image(w: u32, h: u32) -> image::DynamicImage {
        image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h))
    }

    #[test]
    fn a_square_source_into_a_wider_target_fills_it_exactly() {
        let out = cover_crop(&image(640, 640), 24, 10);
        assert_eq!((out.width(), out.height()), (24, 10));
    }

    #[test]
    fn a_square_source_into_a_taller_target_fills_it_exactly() {
        let out = cover_crop(&image(640, 640), 10, 24);
        assert_eq!((out.width(), out.height()), (10, 24));
    }

    #[test]
    fn an_already_matching_aspect_ratio_still_produces_the_exact_target_size() {
        let out = cover_crop(&image(400, 400), 20, 20);
        assert_eq!((out.width(), out.height()), (20, 20));
    }

    #[test]
    fn a_target_larger_than_the_source_upscales_rather_than_leaving_a_gap() {
        let out = cover_crop(&image(10, 10), 100, 40);
        assert_eq!((out.width(), out.height()), (100, 40));
    }

    #[test]
    fn a_wide_source_into_a_square_target_still_fills_it_exactly() {
        let out = cover_crop(&image(1000, 200), 30, 30);
        assert_eq!((out.width(), out.height()), (30, 30));
    }
}

/// Draws the card border once, then dispatches to a real rendered cover
/// image (`images.cover`, when it's actually built for the track
/// currently playing) or the hashed-color monogram placeholder --
/// exactly one of the two ever renders into `inner`, never both. Falling
/// back to the placeholder is a complete, good-looking state on its own
/// (not a degraded one), covering every non-error-worthy reason a real
/// image might not be showing yet: no real graphics protocol on this
/// terminal, no cover fetched yet for this track, or the fetch/decode
/// itself failing.
fn render_art(frame: &mut Frame, app: &AppState, images: &mut ImageState, artist: &str, album: &str, area: Rect) {
    if let Some(deadline) = images.startup_retransmit_at
        && std::time::Instant::now() >= deadline
    {
        images.startup_retransmit_at = None;
        images.startup_retransmit_done = true;
        images.sized_covers.clear();
    }
    if area.height == 0 || area.width == 0 {
        return;
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(DIM));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let current_uri = app.current_track_uri.as_deref();
    let cover = images
        .cover_image
        .as_ref()
        .filter(|(uri, _)| Some(uri.as_str()) == current_uri)
        .map(|(uri, image)| (uri.clone(), image.clone()));

    if let (Some(picker), Some((uri, image))) = (images.picker.clone(), cover) {
        let cached = images
            .sized_covers
            .iter_mut()
            .find(|(u, w, h, _)| *u == uri && *w == inner.width && *h == inner.height);

        let proto = if let Some((_, _, _, proto)) = cached {
            proto
        } else {
            // Round 9 of the art-card sizing saga: three fixes in a row
            // (font-size source, border-thickness math, centering) each
            // addressed a real, verified defect, and the compact hero's
            // border still doesn't tightly match the image. Rather than
            // guess a 4th number, log every real value that feeds the
            // square math -- exactly once per distinct size this cache
            // actually builds for, not every frame -- so the next real
            // run gives actual numbers to compare against what a true
            // square needs, instead of another screenshot to eyeball.
            let cell = real_cell_size(Some(&picker));
            // `log::debug!` would be silently dropped -- this app's own
            // env_logger filter (`main.rs`) is `"info,librespot=debug"`,
            // base level `info`, not `debug`, for anything outside the
            // librespot crates. Using `info!` here is what actually
            // makes this diagnostic show up in the log file at all.
            log::info!(
                "art size: area={area:?} inner={inner:?} cell_size={cell:?} image_native=({}, {})",
                image.width(),
                image.height()
            );
            // Nine rounds of tuning `art_width`/`art_height`'s cell-count
            // math (font-size source, border-thickness, centering) each
            // fixed a real defect, but the border still doesn't tightly
            // hug the image -- confirmed from this exact log line: a
            // 24x10-cell inner region at an 18x40px cell is 432x400 real
            // pixels, an unavoidable ~8% mismatch no integer cell count
            // can close (24 cells can't split evenly into a 40px-tall
            // grid the way 400/18 would need). `Resize::Scale` (below,
            // previously) *fits within* whatever target it's given
            // (confirmed by reading `resize_pixels`: both `Fit` and
            // `Scale` call `image.resize`, which never overflows its
            // target) -- so any such mismatch was never going to do
            // anything but letterbox. Fixed at the root instead of
            // tuning the cell math further: `cover_crop` pre-scales and
            // center-crops the image to the *exact* real pixel size this
            // cell region will occupy, before `ratatui-image` ever sees
            // it -- no cell-to-pixel rounding left for it to get wrong.
            let target_w = inner.width as u32 * cell.0.max(1) as u32;
            let target_h = inner.height as u32 * cell.1.max(1) as u32;
            let image = cover_crop(&image, target_w, target_h);
            if images.sized_covers.len() >= SIZED_COVER_CACHE_CAP {
                images.sized_covers.remove(0);
            }
            let built = picker.new_resize_protocol(image);
            images.sized_covers.push((uri, inner.width, inner.height, built));
            // Arms only off this process's very first-ever cache build,
            // never again -- see `ImageState::startup_retransmit_at`'s
            // own doc comment for the full reasoning.
            if images.startup_retransmit_at.is_none() && !images.startup_retransmit_done {
                images.startup_retransmit_at = Some(std::time::Instant::now() + STARTUP_RETRANSMIT_DELAY);
            }
            &mut images.sized_covers.last_mut().unwrap().3
        };

        // `Resize::Crop` was tried here on the theory that, since
        // `cover_crop` above already pre-sizes the image exactly, `Crop`
        // would be a pure no-op -- reported live as visibly pixelated
        // instead. `Crop`'s own doc comment names the actual reason: it
        // exists for terminals where "overdrawing characters over
        // graphics" needs avoiding (its example is Alacritty's sixel
        // branch), which implies a different, less precise transmission
        // path than `Scale` -- not the "no-op on an already-correct
        // image" behavior assumed here. Reverted to `Resize::Scale`,
        // proven pixelation-free across every prior round of this saga;
        // `cover_crop`'s pre-sizing (the part that actually fixed the
        // gap) is unaffected by this revert.
        let widget = ratatui_image::StatefulImage::default().resize(ratatui_image::Resize::Scale(None));
        frame.render_stateful_widget(widget, inner, proto);
        return;
    }

    render_art_placeholder(frame, artist, album, inner);
}

fn render_art_placeholder(frame: &mut Frame, artist: &str, album: &str, inner: Rect) {
    let bg = art_color(artist, album);
    frame.render_widget(Block::default().style(Style::default().bg(bg)), inner);
    let text_style = Style::default().bg(bg).fg(Color::White).add_modifier(Modifier::BOLD);
    let top_pad = inner.height / 2;
    let mut lines: Vec<Line> = (0..top_pad).map(|_| Line::from("")).collect();
    lines.push(Line::from(monogram(artist, album)));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center).style(text_style), inner);
}

fn render_compact(frame: &mut Frame, app: &AppState, images: &mut ImageState, area: Rect) {
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_now_playing_hero(frame, app, images, artist, title, area),
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
fn render_now_playing_hero(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    // Below ART_MIN_WIDTH the block would crush the monogram illegibly --
    // in a narrow pane, skip the art entirely rather than render
    // something unreadable just to say there's art.
    let art_width = (area.width / 4).clamp(ART_MIN_WIDTH, ART_MAX_WIDTH);
    let show_art = area.width >= art_width + 24;
    // A real pixel square, not a flat "half as tall as wide" guess --
    // see `square_height_cells`'s own doc comment for why that flat
    // assumption produced a card reported live as visibly too tall.
    let art_height = square_height_cells(art_width, real_cell_size(images.picker.as_ref()));

    // +2 over the original 6-11 clamp: the gauge row grew from
    // `Length(1)` to `Length(3)` below (a border, requested live to
    // match the fullscreen views' already-bordered gauge) and needs the
    // 2 extra rows of slack. `.max(art_height + 2)`, not just
    // `.max(art_height)`: the art column below splits into
    // `[Min(1), Length(art_height), Min(1)]` to center the card --
    // a real, log-confirmed bug (not assumed) was `hero_height` only
    // ever guaranteeing *exactly* `art_height`, leaving zero room for
    // those two spacers; ratatui's layout solver then shrank the
    // `Length(art_height)` allocation by 1 to make room for them,
    // silently handing `render_art` a card one row short of the square
    // it asked for (confirmed directly from the `"art size"` diagnostic
    // log: computed `art_height` was 12, actually-rendered `area.height`
    // was 11). `+2` reserves the spacers' own minimum up front instead.
    // Clamp floor raised 8 -> 9: `meta_chunks` below now needs 9 rows
    // minimum (its leading spacer grew to `Length(2)`, see that comment),
    // and a `hero_height` that's too short for its own content would
    // reproduce the exact same silent-shrink failure mode named above,
    // just against `meta_chunks` instead of the art column this time.
    let hero_height = (area.height / 2).clamp(9, 13).max(art_height + 2).min(area.height);
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(hero_height), Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let hero_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_art {
            vec![Constraint::Length(art_width), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1)]
        })
        .split(outer[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    let mut art_top_row: Option<u16> = None;
    let meta_area = if show_art {
        // The art column reserves `hero_height` rows (matching the text
        // column beside it), but the card itself only ever needs
        // `art_height` of them to stay square -- rendering into the
        // whole column would hand `render_art`/`Resize::Scale` a taller-
        // than-square target, and while `Scale` still preserves the
        // image's own aspect (it won't distort), it does leave a blank
        // gap on one side to do it, right back to the shape of bug
        // `Resize::Scale` was originally introduced to fix.
        //
        // Top margin is a fixed `Length(1)`, not `Min(1)` on both ends --
        // a symmetric top+bottom `Min(1)` split centers the card within
        // `hero_height`, but `meta_chunks` below starts its own content
        // (the title) after a *fixed* one-row spacer regardless of
        // `hero_height`'s leftover slack. Whenever the two didn't agree
        // (any time `hero_height` exceeded `art_height + 2`, which is
        // the common case once the 8-13 row clamp binds), the card's
        // computed centering offset and the title's fixed offset drifted
        // apart -- reported live as "the song name still not aligned
        // with top of the frame". Matching this column's own top margin
        // to the text column's fixed spacer keeps both starting at the
        // exact same row, by construction, regardless of `hero_height`.
        let art_area = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Length(art_height), Constraint::Min(1)])
            .split(hero_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        hero_cols[1]
    } else {
        hero_cols[0]
    };

    // Round 9 put the title on the *same* row as the art border's own top
    // edge (`Length(1)` spacer, matching the art column's own), confirmed
    // live via the diagnostic below to actually land on the identical row
    // -- and still reported as visibly misaligned. Root cause, reasoned
    // out rather than guessed further: Unicode box-drawing corner
    // characters (the border's `┌`) render their ink starting from the
    // *vertical center* of their cell, not the top -- that's what makes
    // stacked box-drawing rows connect seamlessly. Regular text glyphs
    // sit near the top of their cell. So even on the *identical* buffer
    // row, the border's visible line sits at that row's middle while the
    // title's visible glyph-top sits near that row's top -- the title
    // reads as floating above the border line no matter what row it's
    // on, because the two kinds of glyph don't align to the same point
    // within a shared cell. Asked directly which side of that gap is
    // preferred, since eliminating it entirely isn't reachable at
    // integer-row granularity (the border's true visual position sits
    // *between* two text rows, not on either one): the leading spacer
    // grew from `Length(1)` to `Length(2)`, moving the title one row
    // *below* the border line -- lining up with where the art's actual
    // pixel content starts (`inner.y`, one row below the border) instead
    // of the border line's own row.
    let meta_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // spacer (one extra row -- see above)
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered -- matches the fullscreen views)
        ])
        .split(meta_area);

    if let Some(art_row) = art_top_row {
        let title_row = meta_chunks[1].y;
        thread_local! {
            static LAST_LOGGED: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
        }
        let pair = (art_row, title_row);
        let changed = LAST_LOGGED.with(|c| c.replace(Some(pair)) != Some(pair));
        if changed {
            log::info!(
                "hero alignment: art_top_row={art_row} title_row={title_row} (one_below_border={})",
                title_row == art_row + 1
            );
        }
    }

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
            .style(Style::default().fg(DIM)),
        meta_chunks[2],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), meta_area.width as usize))
                .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app))),
        meta_chunks[4],
    );
    let gauge_area = Rect { width: meta_chunks[5].width.min(COMPACT_GAUGE_MAX_WIDTH), ..meta_chunks[5] };
    frame.render_widget(progress_gauge_bordered(app), gauge_area);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    let lyrics_area = render_lyrics_credit(frame, app, outer[2], Alignment::Left);
    let lines = body_lines(app);
    let offset = top_anchored_offset(&lines, current_body_line_row(app), lyrics_area.height, lyrics_area.width);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).scroll((offset, 0)), lyrics_area);
}

fn render_fullscreen(frame: &mut Frame, app: &AppState, images: &mut ImageState) {
    let area = frame.area();
    match (&app.track_artist, &app.track_title) {
        (Some(artist), Some(title)) => render_fullscreen_hero(frame, app, images, artist, title, area),
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

fn bold_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|l| {
            Line::from(
                l.spans
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.add_modifier(Modifier::BOLD)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

/// The most immersive treatment: art + song info on one half, the full
/// lyric sheet on the other, no divider between them -- replacing the
/// previous single stacked column (which `render_fullscreen_hero_stacked`
/// below still covers, as the fallback for a terminal too narrow to split).
fn render_fullscreen_hero(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    // A true even split, no divider column between them -- art+info and
    // lyrics each get half, not art squeezed into a narrow fixed sidebar
    // with the rest handed to lyrics by default.
    let can_split = area.width >= 60 && area.height >= 10;
    if !can_split {
        render_fullscreen_hero_stacked(frame, app, images, artist, title, area);
        return;
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Sized off the column's own width first, height derived from that --
    // the previous version did the opposite (`art_width = art_height*2`,
    // with `art_height` itself just half the pane's height), which on a
    // wide fullscreen column left a large, unfilled gutter on both sides
    // of the art card no matter how tall the terminal was -- reported
    // live as "the left side... looks empty." Deriving width from the
    // column directly fills far more of it; height still follows width
    // at the same 2:1 ratio the stacked fallback uses (a terminal cell
    // is roughly twice as tall as it is wide, so this is what reads as
    // a visually square card).
    //
    // Two flat-cap attempts both failed for the same underlying reason:
    // a fixed cell ceiling only looks right at one specific terminal
    // height. 70 cols / 35 rows was tuned for a solid-color placeholder
    // and, once Phase 11 started rendering a real photo there, consumed
    // nearly the entire column on a normal terminal, squeezing the
    // fixed content below to nothing ("still not completely right").
    // 50 cols / 20 rows fixed that squeeze but then read as too small
    // ("shrunk") on a taller terminal, where 20 rows is a shrinking
    // fraction of the available height the taller the terminal gets --
    // a flat cap can't scale with the pane, by definition.
    //
    // Fixed properly this time: height is the *lesser* of two numbers
    // that each answer a different question, instead of one flat
    // ceiling trying to answer both. `ideal_height` keeps the card
    // visually square against whatever width the column produced ("how
    // tall should a square card of this width be"). `height_budget` is
    // 75% of the vertical room actually left after the 8 fixed rows
    // below the card ("how tall can the card get before the fixed
    // content below it, and both breathing-room spacers, get squeezed
    // out") -- the other 25% covers those two `Min(1)` spacers plus
    // slack. Taking the smaller of the two means: on a short terminal,
    // `height_budget` is the binding constraint and the card shrinks to
    // fit safely (this round's original goal); on a tall terminal,
    // `ideal_height` is the binding constraint and the card simply stays
    // a natural, width-matched square instead of an arbitrarily tiny
    // fixed size ("shrunk" complaint, now fixed by not having a flat
    // ceiling at all).
    let art_width_candidate = cols[0].width.saturating_sub(8).clamp(20, 70);
    let cell_size = real_cell_size(images.picker.as_ref());
    let ideal_height = square_height_cells(art_width_candidate, cell_size);
    let max_safe_height = area.height.saturating_sub(8);
    let height_budget = ((max_safe_height as f32) * 0.75) as u16;
    // A real bug from an unconditional `.max(10)` here, caught live: on
    // a short enough terminal, `height_budget` (already `<= max_safe_height`
    // by construction, since it's 75% of it) could fall under 10, but
    // the old floor forced `art_height` back up to 10 regardless --
    // pushing `art_height + 8` past `area.height` and starving the
    // fixed title/artist/transport rows below it of any space at all
    // (ratatui's layout solver dropped them to zero height under the
    // resulting pressure, so they silently disappeared rather than just
    // looking cramped). The floor now aims for 10 only when the terminal
    // actually has that much room to give.
    let floor = 10.min(max_safe_height);
    let effective_ceiling = height_budget.max(floor);
    // The real bug the diagnostic log confirmed: previously `art_height`
    // alone was clamped down to `height_budget` whenever the terminal
    // didn't have room for a true square at `art_width_candidate`, but
    // `art_width` never shrank to match -- producing a card that was
    // *shorter* than square without ever becoming *narrower* to match,
    // i.e. not a square at all (logged live: 70x30 cells at an 18x40px
    // cell came out 1224x1120 real pixels, 9% wider than tall). Fixed by
    // re-deriving width from the constrained height with
    // `square_width_cells` (already built for Phase 11's own narrow-
    // stacked fallback) whenever height ends up being the limiting
    // dimension, so the card is a true square either way -- "adjust one
    // or the other but get them to fit flush," per the live report --
    // instead of only ever adjusting height and leaving the mismatch.
    let (art_width, art_height) = if ideal_height <= effective_ceiling {
        (art_width_candidate, ideal_height.max(1))
    } else {
        let constrained_height = effective_ceiling.max(1);
        let constrained_width = square_width_cells(constrained_height, cell_size).min(art_width_candidate).max(1);
        (constrained_width, constrained_height)
    };
    // Symmetric `Min(1)` on both ends -- true centering. This looked
    // wrong once before only because the fixed content above it (art +
    // 6 text rows) was too small a block to center inside a tall pane
    // without the leftover space reading as excessive; with the art
    // itself now scaling to the pane, the same centering reads as
    // balanced instead of top- or bottom-heavy.
    let side_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1), // top spacer
            Constraint::Length(art_height),
            Constraint::Length(1), // spacer
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered -- needs its own top/bottom rows)
            Constraint::Min(1), // bottom spacer
        ])
        .split(cols[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    render_art(frame, app, images, artist, album, capsule_row(side_rows[1], art_width));

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, cols[0].width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        side_rows[3],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, cols[0].width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        side_rows[4],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), cols[0].width as usize))
                .alignment(Alignment::Center)
                .style(Style::default().fg(DIM)),
            side_rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
            .alignment(Alignment::Center),
        side_rows[6],
    );
    // A contained capsule the same width as the art card above it, not a
    // bar stretching to the full column -- same `capsule_row` call
    // applied to a different row of the same parent guarantees
    // byte-identical left/right edges with the art card by construction,
    // which is the actual fix (previously computed independently, which
    // is how the two drifted apart).
    frame.render_widget(progress_gauge_bordered(app), capsule_row(side_rows[7], art_width));

    // No divider rule between the two halves -- asked for directly,
    // relying on the whitespace gap alone (lyrics get a left inset
    // below) to separate them rather than a drawn line.
    let lyrics_area = Rect { x: cols[1].x + 2, width: cols[1].width.saturating_sub(2), ..cols[1] };
    render_fullscreen_lyrics(frame, app, lyrics_area, Alignment::Left);
}

/// Splits off the last row of a lyrics pane for the credit line. Fewer than
/// three rows and the credit gives way instead: two rows of lyrics is the
/// least worth keeping.
fn credit_split(area: Rect, has_credit: bool) -> (Rect, Option<Rect>) {
    if !has_credit || area.height < 3 {
        return (area, None);
    }
    let lyrics = Rect { height: area.height - 1, ..area };
    let credit = Rect { y: area.y + area.height - 1, height: 1, ..area };
    (lyrics, Some(credit))
}

/// Draws the dim "where these lyrics came from" line (when there is one)
/// and returns the area left for the lyrics themselves.
fn render_lyrics_credit(frame: &mut Frame, app: &AppState, area: Rect, alignment: Alignment) -> Rect {
    let (lyrics, credit_area) = credit_split(area, app.lyrics_credit.is_some());
    if let (Some(credit_area), Some(credit)) = (credit_area, app.lyrics_credit.as_deref()) {
        let text = truncate_ellipsis(credit, credit_area.width as usize);
        frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)).alignment(alignment), credit_area);
    }
    lyrics
}

// Six attempts at "bigger" here, in order -- see git history for each
// one's full detail. First: `tui-big-text` enlarged only the current
// line, jarringly inconsistent next to its normal-size neighbors.
// Second and third: `tui-big-text` uniformly at `PixelSize::Quadrant`,
// reported "way too large" twice, with a song's opening line pinned to
// the top instead of centered. Fourth: letter-spacing instead of block
// glyphs -- "irregularly big spacing," then still "way too large" and
// "unnatural" even after fixing the spacing ratio. Fifth and sixth:
// block glyphs reopened at the user's explicit request, first at
// `PixelSize::Sextant` (confirmed to render with correct, non-garbled
// glyphs -- the font-coverage risk was real but didn't materialize),
// then `PixelSize::Octant` for even smaller -- both still reported "way
// too big," and, decisively this round, rejected on a different axis
// entirely: "not pixelized... more curved," an explicit preference for
// how the text looks, not just how big it is. Block-glyph rendering
// (font8x8-backed, inherently blocky at any `PixelSize`) cannot satisfy
// that -- it's not a parameter to tune, it's the technique itself. Six
// attempts across two families (glyph-scaling, letter-spacing) both
// eventually rejected is well past the systematic-debugging "question
// the architecture" threshold a second and third time over: `tui-big-
// text`/`font8x8` removed from the project outright (`cargo remove`,
// confirmed `Cargo.toml`/`Cargo.lock` clean via `git diff`), the same
// full removal already proven twice this session for letter-spacing and
// the original block-glyph code. What's left -- `bold_lines` +
// `lyric_tier_color`'s 4-tier fade + `center_current_line`/
// `top_anchored_offset` -- renders with the terminal's own font, which
// is exactly what "curved" means in a terminal context: normal
// anti-aliased glyphs, not a bitmap approximation. Literally bigger
// *and* curved at the same time isn't achievable through text alone in
// a fixed-size cell grid -- that would need rendering text to a real
// raster image via an actual font and displaying it through a terminal
// graphics protocol (kitty/iTerm2/sixel), the same category of
// investment Phase 11 already tracks for album art specifically, not
// attempted here without discussing that scope and cost first.
fn render_fullscreen_lyrics(frame: &mut Frame, app: &AppState, area: Rect, alignment: Alignment) {
    let area = render_lyrics_credit(frame, app, area, alignment);
    let (lines, offset) =
        center_current_line(bold_lines(body_lines(app)), current_body_line_row(app), area.height, area.width);
    frame.render_widget(
        Paragraph::new(lines).alignment(alignment).wrap(Wrap { trim: true }).scroll((offset, 0)),
        area,
    );
}

/// Narrow-terminal fallback: the original single stacked column (art on
/// top, then title/transport/lyrics, all centered) -- kept rather than
/// deleted since a split too narrow to read either half legibly is
/// worse than not splitting at all.
/// Inline (art beside title/artist/transport/gauge), mirroring the
/// compact hero's own established shape -- reported live as wanted here
/// too ("the now playing format is not inline... song name is above
/// the album cover"). Previously stacked (art on top, text below,
/// lyrics under that); keeps the same two regions (an art+meta header,
/// then lyrics spanning the full width below it) but makes the header
/// row inline instead of vertically stacked, matching
/// `render_now_playing_hero`'s structure -- centered instead of
/// left-aligned, and using `render_fullscreen_lyrics` (this app's
/// bold+color-fade fullscreen lyrics treatment) rather than the compact
/// hero's small top-anchored one, since this is still a fullscreen view.
fn render_fullscreen_hero_stacked(
    frame: &mut Frame,
    app: &AppState,
    images: &mut ImageState,
    artist: &str,
    title: &str,
    area: Rect,
) {
    let art_width = (area.width / 4).clamp(ART_MIN_WIDTH, ART_MAX_WIDTH);
    let art_height = square_height_cells(art_width, real_cell_size(images.picker.as_ref()));
    let show_art = area.width >= art_width + 24 && area.height >= art_height + 4;

    // `.max(9)`, not `8`: `meta_chunks` below needs 9 rows minimum now
    // (its leading spacer grew to `Length(2)`, matching the compact
    // hero's own identical fix) -- a `header_height` too short for that
    // would silently shrink `meta_chunks`' own content instead.
    let header_height = (art_height + 2).max(9).min(area.height.saturating_sub(2).max(1));
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_height), Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let header_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if show_art {
            vec![Constraint::Length(art_width), Constraint::Min(1)]
        } else {
            vec![Constraint::Min(1)]
        })
        .split(outer[0]);

    let album = app.track_album.as_deref().unwrap_or(title);
    let mut art_top_row: Option<u16> = None;
    let meta_area = if show_art {
        // Same bug, same fix, as `render_now_playing_hero`'s own art
        // column (see its doc comment for the full account): a top
        // `Min(1)` competes with the bottom `Min(1)` for whatever slack
        // `header_height` has beyond `art_height`, and ratatui's surplus
        // distribution between two `Min` constraints doesn't reliably
        // split it 1-and-1 the way a hand-check might assume -- while
        // `meta_chunks` below starts the title after a *fixed* `Length(1)`
        // spacer regardless. This function was missed when that fix
        // shipped for the compact hero (this is a *different* function,
        // not a leftover branch of the same one), so the exact same
        // "song name floats above the art card" report kept reproducing
        // here even after the compact view was confirmed fixed. Fixed
        // identically: a fixed `Length(1)` top margin, matching this
        // column's own `meta_chunks[0]` spacer exactly, by construction.
        let art_area = Layout::default()
            .constraints([Constraint::Length(1), Constraint::Length(art_height), Constraint::Min(1)])
            .split(header_cols[0])[1];
        art_top_row = Some(art_area.y);
        render_art(frame, app, images, artist, album, art_area);
        header_cols[1]
    } else {
        header_cols[0]
    };

    // Same shift as `render_now_playing_hero`'s own `meta_chunks` -- see
    // its doc comment for the full reasoning (box-drawing corner glyphs
    // render centered in their cell, regular text glyphs render near the
    // top, so the two can never visually align on one shared row; asked
    // directly, the title moves one row below the border line instead).
    let meta_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // spacer (one extra row -- see above)
            Constraint::Length(1), // title
            Constraint::Length(1), // artist -- album
            Constraint::Length(1), // spacer / context label
            Constraint::Length(1), // icon + time + vol
            Constraint::Length(3), // gauge (bordered)
        ])
        .split(meta_area);

    // Same diagnostic as `render_now_playing_hero`, extended here after
    // finding this function had the same top-spacer bug that function's
    // own fix never reached (see the art_area comment above) -- logs once
    // per distinct value change so a live run can confirm this call site
    // too, not just the compact one.
    if let Some(art_row) = art_top_row {
        let title_row = meta_chunks[1].y;
        thread_local! {
            static LAST_LOGGED: std::cell::Cell<Option<(u16, u16)>> = const { std::cell::Cell::new(None) };
        }
        let pair = (art_row, title_row);
        let changed = LAST_LOGGED.with(|c| c.replace(Some(pair)) != Some(pair));
        if changed {
            log::info!(
                "fullscreen-stacked hero alignment: art_top_row={art_row} title_row={title_row} (one_below_border={})",
                title_row == art_row + 1
            );
        }
    }

    frame.render_widget(
        Paragraph::new(truncate_ellipsis(title, meta_area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD)),
        meta_chunks[1],
    );
    let artist_album = match &app.track_album {
        Some(al) => format!("{artist} \u{2014} {al}"),
        None => artist.to_string(),
    };
    frame.render_widget(
        Paragraph::new(truncate_ellipsis(&artist_album, meta_area.width as usize))
            .alignment(Alignment::Center)
            .style(Style::default().fg(DIM)),
        meta_chunks[2],
    );
    if let Some(label) = &app.context_label {
        frame.render_widget(
            Paragraph::new(truncate_ellipsis(&format!("Playing from {label}"), meta_area.width as usize))
                .alignment(Alignment::Center)
                .style(Style::default().fg(DIM)),
            meta_chunks[3],
        );
    }
    frame.render_widget(
        Paragraph::new(format!("{} {}   {}", playing_icon(app), time_readout(app), volume_readout(app)))
            .alignment(Alignment::Center),
        meta_chunks[4],
    );
    frame.render_widget(progress_gauge_bordered(app), meta_chunks[5]);

    frame.render_widget(Block::default().borders(Borders::TOP), outer[1]);
    render_fullscreen_lyrics(frame, app, outer[2], Alignment::Center);
}

#[cfg(test)]
mod credit_split_tests {
    use super::*;

    fn area(height: u16) -> Rect {
        Rect { x: 4, y: 10, width: 60, height }
    }

    #[test]
    fn no_credit_leaves_the_lyrics_area_untouched() {
        assert_eq!(credit_split(area(20), false), (area(20), None));
    }

    #[test]
    fn a_credit_takes_exactly_the_last_row() {
        let (lyrics, credit) = credit_split(area(20), true);
        assert_eq!(lyrics, Rect { height: 19, ..area(20) });
        assert_eq!(credit, Some(Rect { x: 4, y: 29, width: 60, height: 1 }));
    }

    #[test]
    fn the_two_never_overlap_and_together_fill_the_area() {
        let (lyrics, credit) = credit_split(area(12), true);
        let credit = credit.unwrap();
        assert_eq!(lyrics.y + lyrics.height, credit.y);
        assert_eq!(credit.y + credit.height, area(12).y + area(12).height);
    }

    #[test]
    fn a_pane_too_short_to_spare_a_row_shows_lyrics_only() {
        // Two rows of lyrics is the least worth keeping; below that the
        // credit gives way rather than squeezing the lyrics out.
        assert_eq!(credit_split(area(2), true), (area(2), None));
        assert_eq!(credit_split(area(0), true), (area(0), None));
        assert!(credit_split(area(3), true).1.is_some());
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::lyrics::WordSeg;

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    fn hi_there() -> Vec<WordSeg> {
        vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)]
    }

    fn run(text: &str, fill: Fill) -> (String, Fill) {
        (text.to_string(), fill)
    }

    fn joined(runs: &[(String, Fill)]) -> String {
        runs.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn before_the_first_word_the_whole_line_is_unsung() {
        assert_eq!(sweep_runs(&hi_there(), 0.5), vec![run("hi there", Fill::Unsung)]);
        assert_eq!(sweep_runs(&hi_there(), 1.0), vec![run("hi there", Fill::Unsung)]);
    }

    #[test]
    fn after_the_last_word_the_whole_line_is_sung() {
        assert_eq!(sweep_runs(&hi_there(), 2.0), vec![run("hi there", Fill::Sung)]);
        assert_eq!(sweep_runs(&hi_there(), 60.0), vec![run("hi there", Fill::Sung)]);
    }

    #[test]
    fn inside_a_word_that_fraction_of_its_characters_is_sung() {
        // "there" is 1.4..2.0; at 1.7 it is half done: floor(0.5 * 5) = 2 chars.
        assert_eq!(
            sweep_runs(&hi_there(), 1.7),
            vec![run("hi th", Fill::Sung), run("ere", Fill::Unsung)]
        );
    }

    #[test]
    fn a_word_s_trailing_space_is_not_sung_until_the_word_is_finished() {
        let words = vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)];
        // Half through "hi": floor(0.5 * 2) = 1 char of "hi", the space still unsung.
        assert_eq!(sweep_runs(&words, 1.2), vec![run("h", Fill::Sung), run("i there", Fill::Unsung)]);
        // Exactly at the end of "hi ": the space is sung with it.
        assert_eq!(sweep_runs(&words, 1.4), vec![run("hi ", Fill::Sung), run("there", Fill::Unsung)]);
    }

    #[test]
    fn multibyte_text_is_split_on_characters_not_bytes() {
        let words = vec![seg("\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}", 0.0, 1.0)];
        assert_eq!(
            sweep_runs(&words, 0.5),
            vec![run("\u{3053}\u{3093}", Fill::Sung), run("\u{306b}\u{3061}\u{306f}", Fill::Unsung)]
        );
    }

    #[test]
    fn a_zero_length_word_flips_at_its_start() {
        let words = vec![seg("a ", 1.0, 1.0), seg("b", 2.0, 2.0)];
        assert_eq!(sweep_runs(&words, 0.9), vec![run("a b", Fill::Unsung)]);
        assert_eq!(sweep_runs(&words, 1.0), vec![run("a ", Fill::Sung), run("b", Fill::Unsung)]);
        assert_eq!(sweep_runs(&words, 2.0), vec![run("a b", Fill::Sung)]);
    }

    #[test]
    fn an_overlapping_background_vocal_sweeps_on_its_own_clock() {
        // The background starts while the lead is still going.
        let words = vec![seg("Hey ", 1.0, 1.4), seg("(Oh)", 1.1, 1.9)];
        assert_eq!(
            sweep_runs(&words, 1.5),
            vec![run("Hey (O", Fill::Sung), run("h)", Fill::Unsung)]
        );
    }

    #[test]
    fn the_runs_always_re_form_the_line_whatever_the_position() {
        let words = vec![seg("I ", 27.395, 27.549), seg("been ", 27.549, 27.74), seg("try", 27.74, 27.908), seg("na ", 27.908, 28.077), seg("call", 28.077, 28.96)];
        let text: String = words.iter().map(|w| w.text.as_str()).collect();
        let mut pos = 26.0;
        while pos < 30.0 {
            assert_eq!(joined(&sweep_runs(&words, pos)), text, "at {pos}");
            pos += 0.037;
        }
    }

    #[test]
    fn neighbouring_runs_with_the_same_fill_are_merged() {
        let runs = sweep_runs(&hi_there(), 60.0);
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn no_words_no_runs() {
        assert!(sweep_runs(&[], 5.0).is_empty());
    }
}

#[cfg(test)]
mod sweep_active_tests {
    use super::*;
    use crate::lyrics::{LyricLine, WordSeg};
    use std::time::Duration;

    fn line(words: bool) -> LyricLine {
        LyricLine {
            timestamp: Duration::from_secs(1),
            text: "hi".to_string(),
            words: if words { vec![WordSeg { text: "hi".to_string(), start: 1.0, end: 2.0 }] } else { Vec::new() },
        }
    }

    #[test]
    fn active_only_while_playing_a_line_that_has_words() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(word_sweep_active(&lyrics, Some(0), Some(true)));
    }

    #[test]
    fn paused_or_stopped_needs_no_faster_redraw() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(false)));
        assert!(!word_sweep_active(&lyrics, Some(0), None));
    }

    #[test]
    fn a_line_level_sync_never_speeds_up_the_redraw() {
        let lyrics = LyricsState::Synced(vec![line(false)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(true)));
    }

    #[test]
    fn only_the_current_line_counts() {
        let lyrics = LyricsState::Synced(vec![line(false), line(true)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(true)));
        assert!(word_sweep_active(&lyrics, Some(1), Some(true)));
    }

    #[test]
    fn no_current_line_or_other_lyric_states_are_inactive() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(!word_sweep_active(&lyrics, None, Some(true)));
        assert!(!word_sweep_active(&LyricsState::Loading, Some(0), Some(true)));
        assert!(!word_sweep_active(&LyricsState::Synced(vec![line(true)]), Some(9), Some(true)));
    }
}

#[cfg(test)]
mod display_line_tests {
    use super::*;
    use crate::lyrics::{LyricLine, WordSeg};
    use crate::romanize::RomanLine;
    use std::time::Duration;

    fn seg(text: &str) -> WordSeg {
        WordSeg { text: text.to_string(), start: 1.0, end: 2.0 }
    }

    fn native() -> LyricLine {
        LyricLine { timestamp: Duration::from_secs(1), text: "\u{541b}".to_string(), words: vec![seg("\u{541b}")] }
    }

    fn roman() -> RomanLine {
        RomanLine { text: "kimi".to_string(), words: vec![seg("kimi")] }
    }

    #[test]
    fn the_native_line_shows_when_romanization_is_off() {
        let (line, roman) = (native(), roman());
        assert_eq!(display_line(&line, Some(&roman), false), ("\u{541b}", &line.words[..]));
    }

    #[test]
    fn the_romanized_line_and_its_words_replace_it_when_on() {
        let (line, roman) = (native(), roman());
        assert_eq!(display_line(&line, Some(&roman), true), ("kimi", &roman.words[..]));
    }

    #[test]
    fn a_line_with_no_romanization_stays_native_even_when_on() {
        let line = native();
        assert_eq!(display_line(&line, None, true), ("\u{541b}", &line.words[..]));
    }

    #[test]
    fn a_romanized_line_without_re_timed_words_is_drawn_whole_not_swept() {
        // Falling back to the native words would sweep the wrong text.
        let line = native();
        let plain = RomanLine { text: "kimi".to_string(), words: Vec::new() };
        let (text, words) = display_line(&line, Some(&plain), true);
        assert_eq!(text, "kimi");
        assert!(words.is_empty());
    }
}

#[cfg(test)]
mod plain_display_tests {
    use super::*;
    use crate::romanize::RomanLine;

    fn roman(text: &str) -> Option<RomanLine> {
        Some(RomanLine { text: text.to_string(), words: Vec::new() })
    }

    #[test]
    fn native_text_shows_when_romanization_is_off() {
        let r = vec![roman("kimi")];
        assert_eq!(plain_display_lines("\u{541b}", Some(&r), false), vec!["\u{541b}"]);
    }

    #[test]
    fn romanized_lines_replace_native_ones_when_on() {
        let r = vec![roman("kimi"), None, roman("sayonara")];
        assert_eq!(
            plain_display_lines("\u{541b}\nStay\n\u{3055}\u{3088}\u{306a}\u{3089}", Some(&r), true),
            vec!["kimi", "Stay", "sayonara"]
        );
    }

    #[test]
    fn blank_lines_are_kept_so_the_layout_does_not_shift() {
        let r = vec![roman("kimi"), None, roman("nani")];
        assert_eq!(plain_display_lines("\u{541b}\n\n\u{4f55}", Some(&r), true), vec!["kimi", "", "nani"]);
    }

    #[test]
    fn nothing_computed_yet_shows_native_text() {
        assert_eq!(plain_display_lines("a\nb", None, true), vec!["a", "b"]);
    }

    #[test]
    fn a_result_that_does_not_line_up_is_ignored_rather_than_misplaced() {
        // Defensive: entries for a different number of lines than the text has.
        let r = vec![roman("kimi")];
        assert_eq!(plain_display_lines("a\nb\nc", Some(&r), true), vec!["a", "b", "c"]);
    }
}
