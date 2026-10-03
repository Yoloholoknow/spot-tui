use super::*;

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

