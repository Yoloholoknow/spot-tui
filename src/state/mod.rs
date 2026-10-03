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
pub use list::{filtered_sorted, move_item_down, move_item_to, move_item_up, parse_move_position, pinned_first, Fetch, ListFilter};
pub use nav::{sidebar_rows, Focus, Nav, Screen, SidebarRow, LIBRARY_ENTRIES, SIDEBAR_ENTRIES};
pub use overlays::{
    quick_jump_entries, ConfirmAction, PendingConfirm, PlaylistPicker, QuickJump, QuickJumpEntry, QuickJumpKind,
    TextPrompt, TextPromptAction, ConfirmSeverity,
};
pub use playback::{playback_modes, PLAYBACK_MODES_WIDTH, LyricsState, RepeatMode, ShuffleMode};
pub use screens::{AlbumDetailState, ArtistDetailState, DevicesState, LibraryState, PlaylistDetailState, QueueState};
pub use search::SearchState;
