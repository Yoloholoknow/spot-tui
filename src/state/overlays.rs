use super::*;
use crate::api::search::TrackResult;

/// A single-line text prompt (a playlist name, or a move-to position).
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
    pub fn new(
        title: impl Into<String>,
        initial: impl Into<String>,
        action: TextPromptAction,
    ) -> Self {
        let query: String = initial.into();
        let cursor = query.chars().count();
        Self {
            title: title.into(),
            query,
            cursor,
            action,
        }
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
    RemoveTrack {
        playlist_uri: String,
        track_uri: String,
        occurrences: usize,
    },
    /// Confirmed past the "this playlist already has this track" warning
    /// -- adds it anyway, the exact same call `TrackAdded`'s normal path
    /// uses, just reached from the confirm overlay instead of directly.
    AddTrackAnyway {
        playlist_uri: String,
        track_uri: String,
    },
    /// `q`, when `Config::confirm_quit` is on -- the one confirm action
    /// that doesn't mutate anything, just tells the main loop to actually
    /// exit once confirmed.
    Quit,
    /// Quick jump's "Sign out": deletes the stored logins, then the app returns
    /// to its signed-out screen.
    SignOut,
    // Liking, following and saving fire immediately, like add-to-playlist.
    // The reverse (unlike/unfollow/unsave), always reached from the list
    // that owns the item, confirms first, like every removal.
    UnlikeTrack {
        track_uri: String,
    },
    UnfollowArtist {
        artist_uri: String,
    },
    UnsaveAlbum {
        album_uri: String,
    },
}

/// How carefully to read a confirmation. Derived from the action rather
/// than stored, since it is a fact about which action it is.
pub enum ConfirmSeverity {
    Danger,
    Warn,
    Neutral,
}

impl ConfirmAction {
    pub fn severity(&self) -> ConfirmSeverity {
        match self {
            ConfirmAction::DeletePlaylist(_) | ConfirmAction::RemoveTrack { .. } => {
                ConfirmSeverity::Danger
            }
            // Warn, not Danger -- unlike RemoveTrack/DeletePlaylist,
            // undoing any of these is one more keypress away (like/
            // follow/save again), not a real, harder-to-recover loss.
            ConfirmAction::AddTrackAnyway { .. }
            | ConfirmAction::UnlikeTrack { .. }
            | ConfirmAction::UnfollowArtist { .. }
            | ConfirmAction::UnsaveAlbum { .. } => ConfirmSeverity::Warn,
            ConfirmAction::Quit => ConfirmSeverity::Neutral,
            // Reversible, but it ends the session and needs a browser to undo.
            ConfirmAction::SignOut => ConfirmSeverity::Warn,
        }
    }
}

/// The add-to-playlist picker (`a`): the user's playlists, pinned first (the
/// same order as Your Playlists and the sidebar).
pub struct PlaylistPicker {
    pub track_uri: String,
    pub selected: usize,
    /// Always live: the picker has no letter-key actions competing for
    /// input, so every printable key narrows it with no `/` step. Reuses
    /// `ListFilter` for `filtered_sorted`; `editing` and `sort_alpha` go
    /// unused.
    pub filter: ListFilter,
}

/// The quick-jump palette (`Ctrl+P`): playlists, liked tracks, followed
/// artists, saved albums, devices and every screen in one searchable list.
/// Only `selected` and `filter` are stored. The entries are recomputed from
/// `AppState` on demand (`quick_jump_entries`), so a fetch that completes
/// while the palette is open shows up with no invalidation to get wrong.
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
    SignOut,
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
pub const QUICK_JUMP_SCREENS: &[(&str, Screen)] = &[
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

/// Builds the pool quick jump searches, in category order (screens,
/// playlists, artists, albums, devices, tracks): substring matching has no
/// relevance score, so build order is display order. With an empty query
/// only the fixed screens are built, since the dynamic pool can run to
/// thousands of items and is only needed once the user starts typing.
pub fn quick_jump_entries(app: &AppState, filter: &ListFilter) -> Vec<QuickJumpEntry> {
    let mut entries: Vec<QuickJumpEntry> = QUICK_JUMP_SCREENS
        .iter()
        .map(|(label, screen)| QuickJumpEntry {
            label: format!("[Go] {label}"),
            kind: QuickJumpKind::Screen(*screen),
        })
        .collect();
    // Last, so the palette's default selection (the first row) never lands on it.
    entries.push(QuickJumpEntry {
        label: "[Account] Sign out".to_string(),
        kind: QuickJumpKind::SignOut,
    });
    if filter.query.is_empty() {
        return entries;
    }
    if let Fetch::Ready(items) = &app.library.playlists {
        entries.extend(items.iter().map(|p| QuickJumpEntry {
            label: format!("[Playlist] {}", p.name),
            kind: QuickJumpKind::Playlist(p.clone()),
        }));
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
        entries.extend(items.iter().map(|d| QuickJumpEntry {
            label: format!("[Device] {}", d.name),
            kind: QuickJumpKind::Device(d.clone()),
        }));
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
mod sign_out_tests {
    use super::*;
    use std::collections::HashSet;

    fn app() -> AppState {
        AppState::new(false, HashSet::new(), HashSet::new(), 0)
    }

    #[test]
    fn sign_out_is_in_the_palette_but_never_the_default_row() {
        let entries = quick_jump_entries(&app(), &ListFilter::default());
        let position = entries
            .iter()
            .position(|e| matches!(e.kind, QuickJumpKind::SignOut))
            .expect("a sign out entry");
        assert_ne!(
            position, 0,
            "Enter on a freshly opened palette must not sign out"
        );
    }

    #[test]
    fn typing_sign_finds_it() {
        let filter = ListFilter {
            query: "sign".to_string(),
            ..ListFilter::default()
        };
        let entries = quick_jump_entries(&app(), &filter);
        assert!(entries.iter().any(|e| e.label.contains("Sign out")));
    }

    #[test]
    fn signing_out_asks_first_as_a_warning() {
        assert!(matches!(
            ConfirmAction::SignOut.severity(),
            ConfirmSeverity::Warn
        ));
    }
}
