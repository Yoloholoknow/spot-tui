//! Behaviour shared by every filterable list screen.

use crate::state::ListFilter;
use crossterm::event::KeyCode;

/// While a filter is being typed, it owns text editing: characters,
/// Backspace, Enter (keep the filter), Esc (drop it) and Left/Right (move
/// the cursor). Up/Down are deliberately left to the screen so the
/// highlight still moves while typing. Returns whether the key was consumed.
pub fn edit_filter(filter: &mut ListFilter, selected: &mut usize, code: KeyCode) -> bool {
    if !filter.editing {
        return false;
    }
    match code {
        KeyCode::Char(c) => {
            filter.insert_at_cursor(c);
            *selected = 0;
        }
        KeyCode::Backspace => {
            filter.backspace_at_cursor();
            *selected = 0;
        }
        KeyCode::Enter => filter.editing = false,
        KeyCode::Esc => {
            filter.cancel_editing();
            *selected = 0;
        }
        KeyCode::Left => filter.cursor_left(),
        KeyCode::Right => filter.cursor_right(),
        _ => return false,
    }
    true
}

/// `/` starts a filter, `o` toggles alphabetical sort, and Esc clears an
/// applied filter. That last one must come before the screen's own Esc,
/// otherwise Esc would leave with the filter still narrowing the list next
/// time it opens.
pub fn filter_hotkeys(filter: &mut ListFilter, selected: &mut usize, code: KeyCode) -> bool {
    match code {
        KeyCode::Esc if !filter.query.is_empty() => {
            filter.query.clear();
            filter.cursor = 0;
            *selected = 0;
        }
        KeyCode::Char('/') => filter.start_editing(),
        KeyCode::Char('o') => filter.sort_alpha = !filter.sort_alpha,
        _ => return false,
    }
    true
}

/// Up/Down over a list of `len` rows.
pub fn move_selection(selected: &mut usize, len: usize, code: KeyCode) -> bool {
    match code {
        KeyCode::Up => *selected = selected.saturating_sub(1),
        KeyCode::Down => {
            if len > 0 {
                *selected = (*selected + 1).min(len - 1);
            }
        }
        _ => return false,
    }
    true
}
