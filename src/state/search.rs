use super::*;
use crate::api::search::TrackResult;

pub struct SearchState {
    pub query: String,
    /// Where the next typed character goes, as a character index (not a
    /// byte offset, so it is safe on multi-byte text).
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

    /// Empties the query and its results, as when Search is (re)entered.
    pub fn clear(&mut self) {
        self.query.clear();
        self.cursor = 0;
        self.results.clear();
        self.error = None;
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
