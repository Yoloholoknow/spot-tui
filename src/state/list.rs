use super::*;

/// State of one asynchronously fetched value. Each list carries its own, so
/// leaving one still-loading list for another never makes the second think a
/// fetch is already in flight.
pub enum Fetch<T> {
    NotStarted,
    Loading,
    Ready(T),
    Failed(String),
}

/// In-list filter and sort, shared by every list screen. Presentation only:
/// the fetched `Vec` is never touched, so clearing the filter restores the
/// original list with no refetch.
#[derive(Default)]
pub struct ListFilter {
    pub query: String,
    /// Cursor position in characters (not bytes) within `query`; Left/Right
    /// move it, as in Search.
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

    /// Exits edit mode and clears the query. (`editing = false` alone would
    /// keep what was typed applied.)
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

/// Applies `filter`'s query (case-insensitive substring match on `label`)
/// and, if `sort_alpha` is set, an alphabetical sort by label.
///
/// Returns `(original_index, item)` pairs referencing `items`. The original
/// index matters: telling Spotify to play "index N of this playlist" needs
/// the position in the real, unfiltered list, never the display row.
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

/// Playlist move mode: `Up`/`Down` relocate the selected item one slot at a
/// time, entirely locally (the network is hit once, on confirm). Returns the
/// item's new index (unchanged at either end of the list).
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

