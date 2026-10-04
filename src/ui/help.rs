use super::*;

/// Fixed key column width: an aligned key column makes a dense reference
/// scannable, so this is the one place that breaks the spacing grid.
pub(super) const HELP_KEY_WIDTH: usize = 14;
/// Below this main-area width Help is one scrolling column instead of two: two
/// columns need roughly 118 terminal columns once the sidebar is counted.
pub(super) const HELP_TWO_COLUMN_MIN_WIDTH: u16 = 96;

/// Keep in sync with the key handlers in `input/`. Search is excluded from
/// opening Help with `?` (every printable key must reach the query box), so
/// the reference does not claim `?` works everywhere.
pub(super) const HELP_SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Global",
        &[
            ("Tab", "switch focus between Sidebar and Main"),
            ("Esc", "back one level; at the root, focus moves to Sidebar"),
            ("?", "this screen (not while typing in Search or a filter)"),
            (
                "Shift+Q",
                "quit -- asks \"Quit spot-tui? y/n\" first by default; set confirm_quit = false in config.toml for immediate quit (not while typing in Search). Plain q is add-to-queue, not quit -- see Queue below",
            ),
            (
                "Ctrl+C",
                "quit immediately, never confirms -- works everywhere, including while typing",
            ),
            (
                "Space / n / p / + / -",
                "play-pause / next / previous / volume -- works from any screen, including while browsing a list, not just Now Playing (not while typing in Search)",
            ),
            (
                "m",
                "mute / unmute -- restores the exact volume it muted, not a fixed default. Works from any screen except Playlist Detail, where m already means enter move-mode (not while typing in Search)",
            ),
            (
                "/",
                "jump to Search (Sidebar, Now Playing) or open a list's filter",
            ),
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
                "s",
                "cycle shuffle like Spotify's own button: off, then shuffle, then smart shuffle (\u{2726} on the playbar), then off. Works from any screen (not while typing in Search or a filter). Shuffle stays on when you start a different playlist or album. Smart shuffle mixes recommended songs from outside the playlist into the order, like Spotify's own apps (playlists only)",
            ),
            (
                "r",
                "cycle repeat: off, then the whole album/playlist, then this one song, then off. The playbar always shows \u{21c4} (shuffle) and \u{21bb} (repeat, \u{21bb}1 for this song) -- bright when on, dim when off",
            ),
        ],
    ),
    (
        "Now Playing",
        &[
            ("\u{2190} / \u{2192}", "seek \u{00b1}5s"),
            ("\u{2191} / \u{2193}", "volume (same as +/-)"),
            ("v", "open the currently-playing track's album"),
            ("Shift+V", "open the currently-playing track's artist"),
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
            (
                "Enter / \u{2192}",
                "open (Your Playlists \u{2192} Playlist Detail only)",
            ),
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
            (
                "c",
                "create a new playlist -- works from any screen except Search",
            ),
            (
                "Shift+R",
                "rename -- Your Playlists: the selected playlist; Playlist Detail: the open playlist",
            ),
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
                "Shift+S",
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
                "q",
                "add the selected track to the queue -- from Liked Songs, Playlist Detail, or Album Detail (Alt+\u{2193} from Search, where every letter types into the query box)",
            ),
            (
                "refreshes",
                "automatically every 5s while this screen is open",
            ),
        ],
    ),
    (
        "Devices",
        &[
            ("\u{2191} / \u{2193}", "move selection"),
            (
                "Enter",
                "transfer playback here (keeps current play/pause state)",
            ),
            ("Shift+R", "refresh the device list"),
        ],
    ),
    (
        "Artist Detail (Followed Artists, or `v`/Ctrl+\u{2192} from a track)",
        &[
            (
                "\u{2191} / \u{2193}",
                "move selection among the artist's albums",
            ),
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
            (
                "\u{2191} / \u{2193}",
                "move selection among the album's tracks",
            ),
            (
                "Enter",
                "play the album as context, starting from the selected track",
            ),
            ("a", "add the selected track to a playlist"),
            ("v", "view this album's artist"),
        ],
    ),
    (
        "View an item's artist/album",
        &[
            (
                "v",
                "open the selected track's album -- Liked Songs, Playlist Detail, Queue, Now Playing (the currently-playing track; on Album Detail, opens the album's own artist instead -- there's no separate album to open from inside one)",
            ),
            (
                "Shift+V",
                "open the selected track's artist -- Liked Songs, Playlist Detail, Queue, Album Detail, Now Playing",
            ),
            (
                "Ctrl+\u{2192} / Alt+\u{2192}",
                "open the selected result's album / artist -- Search only (plain letters all type into the query box there)",
            ),
            (
                "Ctrl+\u{2193}",
                "add the selected result to a playlist -- Search only, opens the picker without needing to play or leave first",
            ),
            (
                "Enter / \u{2192}",
                "open the album from Saved Albums; open the artist from Followed Artists",
            ),
        ],
    ),
    (
        "Move mode (Playlist Detail, after `m`)",
        &[
            (
                "\u{2191} / \u{2193}",
                "relocate the track one slot at a time, locally -- no network call per keystroke",
            ),
            (
                "g",
                "jump the track straight to a typed position (1 = top) instead of nudging it slot by slot -- still local, still confirmed or cancelled with Enter/Esc afterward",
            ),
            (
                "Enter",
                "confirm -- one reorder call for the net displacement",
            ),
            ("Esc", "cancel -- walks the track back to where it started"),
        ],
    ),
    (
        "Prompt / confirm / picker overlays",
        &[
            (
                "Enter",
                "prompt: submit. picker: add to the selected playlist. confirm: same as y",
            ),
            ("y / n", "confirm: y does it, n cancels"),
            ("Esc", "cancel and close, no exceptions"),
            (
                "\u{2190} / \u{2192}",
                "prompt: move the cursor within the text. picker: move the cursor within its filter",
            ),
            ("\u{2191} / \u{2193}", "picker: move the selected playlist"),
            (
                "any letter/number",
                "picker: narrows the list by name -- always live, no separate key to start typing",
            ),
            (
                "Backspace",
                "picker: delete the character before the cursor in its filter",
            ),
        ],
    ),
    (
        "While typing (a filter, or Search's query)",
        &[
            (
                "\u{2191} / \u{2193}",
                "move the highlighted track (filters only -- keeps working while typing)",
            ),
            ("\u{2190} / \u{2192}", "move the cursor within the text"),
            ("Backspace", "delete the character before the cursor"),
            (
                "Enter",
                "commit (Search: run the search; filters: stop editing, keep the narrowed list)",
            ),
            (
                "Esc",
                "Search: back. Filters: stop editing AND clear the filter back to the full list",
            ),
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

/// One section's lines: an accent bold title, then a row per binding with the key
/// padded to `HELP_KEY_WIDTH` and the description dim. Wrapped by hand with a
/// hanging indent so continuation lines stay under the description, which also
/// makes the rendered height exactly `lines.len()`, which the scroll clamp in
/// `render_help` relies on.
pub(super) fn help_section_lines(
    title: &str,
    rows: &[(&str, &str)],
    width: usize,
) -> Vec<Line<'static>> {
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
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(piece, Style::default().fg(DIM)),
                ]));
            }
        }
    }
    lines
}

/// Index of the first section in column 2, chosen so the columns have as equal a
/// rendered height as possible (sections run 2 to 10 rows, so an even section
/// count is not an even height). Greedy: fill column 1 until adding a section
/// would reach half the total height.
pub(super) fn help_column_split(section_heights: &[usize]) -> usize {
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

pub(super) fn render_help(frame: &mut Frame, area: Rect, offset: &mut u16) {
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);
    frame.render_widget(
        Paragraph::new(screen_header_line("Keybinds", None)),
        shell[0],
    );
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
        .constraints([
            Constraint::Min(1),
            Constraint::Length(4),
            Constraint::Min(1),
        ])
        .split(body_area);
    let col_width = cols[0].width as usize;

    let section_lines: Vec<Vec<Line>> = HELP_SECTIONS
        .iter()
        .map(|&(title, rows)| help_section_lines(title, rows, col_width))
        .collect();
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
