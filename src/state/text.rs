
/// Cursor editing shared by every text field (Search, list filters, the text
/// prompt, the overlays). The cursor is a character index, not a byte
/// offset, so this is correct on multi-byte UTF-8.
pub(super) fn text_char_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// Inserts `c` at `*cursor` and advances it by one character.
pub(super) fn text_insert_at_cursor(s: &mut String, cursor: &mut usize, c: char) {
    let byte_pos = text_char_byte_offset(s, *cursor);
    s.insert(byte_pos, c);
    *cursor += 1;
}

/// Deletes the character immediately before `*cursor`, if any -- not
/// always the last character in the string.
pub(super) fn text_backspace_at_cursor(s: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let start = text_char_byte_offset(s, *cursor - 1);
    let end = text_char_byte_offset(s, *cursor);
    s.replace_range(start..end, "");
    *cursor -= 1;
}

pub(super) fn text_cursor_left(cursor: &mut usize) {
    *cursor = cursor.saturating_sub(1);
}

pub(super) fn text_cursor_right(s: &str, cursor: &mut usize) {
    *cursor = (*cursor + 1).min(s.chars().count());
}

