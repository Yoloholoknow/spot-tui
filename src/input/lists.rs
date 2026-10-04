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

#[cfg(test)]
mod tests {
    use super::*;

    fn editing(query: &str) -> ListFilter {
        let mut filter = ListFilter {
            query: query.to_string(),
            ..Default::default()
        };
        filter.start_editing();
        filter
    }

    #[test]
    fn typing_narrows_the_filter_and_resets_the_selection() {
        let mut filter = editing("ab");
        let mut selected = 7;
        assert!(edit_filter(&mut filter, &mut selected, KeyCode::Char('c')));
        assert_eq!(filter.query, "abc");
        assert_eq!(selected, 0);
    }

    #[test]
    fn enter_keeps_the_filter_but_stops_editing() {
        let mut filter = editing("ab");
        let mut selected = 3;
        assert!(edit_filter(&mut filter, &mut selected, KeyCode::Enter));
        assert!(!filter.editing);
        assert_eq!(filter.query, "ab");
        assert_eq!(selected, 3);
    }

    #[test]
    fn esc_while_editing_drops_the_filter() {
        let mut filter = editing("ab");
        let mut selected = 3;
        assert!(edit_filter(&mut filter, &mut selected, KeyCode::Esc));
        assert!(!filter.editing);
        assert!(filter.query.is_empty());
        assert_eq!(selected, 0);
    }

    #[test]
    fn up_and_down_are_left_to_the_screen_while_editing() {
        let mut filter = editing("ab");
        let mut selected = 3;
        assert!(!edit_filter(&mut filter, &mut selected, KeyCode::Down));
        assert!(!edit_filter(&mut filter, &mut selected, KeyCode::Up));
    }

    #[test]
    fn nothing_is_consumed_when_not_editing() {
        let mut filter = ListFilter::default();
        let mut selected = 0;
        assert!(!edit_filter(&mut filter, &mut selected, KeyCode::Char('x')));
        assert!(filter.query.is_empty());
    }

    #[test]
    fn esc_clears_an_applied_filter_before_the_screen_can_leave() {
        let mut filter = ListFilter {
            query: "abc".into(),
            cursor: 3,
            ..Default::default()
        };
        let mut selected = 4;
        assert!(filter_hotkeys(&mut filter, &mut selected, KeyCode::Esc));
        assert!(filter.query.is_empty());
        assert_eq!(selected, 0);
    }

    #[test]
    fn esc_with_no_filter_is_left_for_the_screen_to_leave() {
        let mut filter = ListFilter::default();
        let mut selected = 4;
        assert!(!filter_hotkeys(&mut filter, &mut selected, KeyCode::Esc));
    }

    #[test]
    fn slash_starts_editing_and_o_toggles_sort() {
        let mut filter = ListFilter::default();
        let mut selected = 0;
        assert!(filter_hotkeys(
            &mut filter,
            &mut selected,
            KeyCode::Char('/')
        ));
        assert!(filter.editing);
        assert!(filter_hotkeys(
            &mut filter,
            &mut selected,
            KeyCode::Char('o')
        ));
        assert!(filter.sort_alpha);
    }

    #[test]
    fn selection_clamps_at_both_ends() {
        let mut selected = 0;
        move_selection(&mut selected, 3, KeyCode::Up);
        assert_eq!(selected, 0);
        for _ in 0..5 {
            move_selection(&mut selected, 3, KeyCode::Down);
        }
        assert_eq!(selected, 2);
    }

    #[test]
    fn selection_does_not_move_in_an_empty_list() {
        let mut selected = 0;
        assert!(move_selection(&mut selected, 0, KeyCode::Down));
        assert_eq!(selected, 0);
    }
}
