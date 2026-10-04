//! Application state: plain data and the pure logic that operates on it.
//! Nothing here draws or does I/O, so it is all unit-testable.

mod app;
mod list;
mod nav;
mod overlays;
mod playback;
mod screens;
mod search;
mod text;

use text::{text_backspace_at_cursor, text_cursor_left, text_cursor_right, text_insert_at_cursor};

pub use app::AppState;
pub use list::{
    Fetch, ListFilter, filtered_sorted, move_item_down, move_item_to, move_item_up,
    parse_move_position, pinned_first,
};
pub use nav::{Focus, LIBRARY_ENTRIES, Nav, SIDEBAR_ENTRIES, Screen, SidebarRow, sidebar_rows};
pub use overlays::{
    ConfirmAction, ConfirmSeverity, PendingConfirm, PlaylistPicker, QuickJump, QuickJumpEntry,
    QuickJumpKind, TextPrompt, TextPromptAction, quick_jump_entries,
};
pub use playback::{LyricsState, PLAYBACK_MODES_WIDTH, RepeatMode, ShuffleMode, playback_modes};
pub use screens::{
    AlbumDetailState, ArtistDetailState, DevicesState, LibraryState, PlaylistDetailState,
    QueueState,
};
pub use search::SearchState;
