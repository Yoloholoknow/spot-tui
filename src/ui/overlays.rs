use super::*;

/// A `Rect` of `width` x `height` centered within `area`, clamped so it
/// never exceeds `area` on a narrow/short terminal.
pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

// Shared overlay dimensions: one width for the two list overlays (picker, quick
// jump) so they read as a system, and one horizontal padding for all four.
pub(super) const OVERLAY_LIST_WIDTH: u16 = 50;
pub(super) const OVERLAY_LIST_HEIGHT: u16 = 16;
pub(super) const OVERLAY_PROMPT_WIDTH: u16 = 50;
pub(super) const OVERLAY_PROMPT_HEIGHT: u16 = 3; // 1 content row + 2 borders -- no vertical padding
pub(super) const OVERLAY_CONFIRM_MIN_WIDTH: u16 = 24;
pub(super) const OVERLAY_CONFIRM_MAX_WIDTH: u16 = 70;
/// Horizontal only: a blank row costs real height in a short list overlay, while
/// side padding keeps text off the border.
pub(super) const OVERLAY_PAD_X: u16 = 1;
/// Borders plus horizontal padding: everything between an overlay's outer width
/// and its text width. The confirm overlay predicts its wrapped height by hand
/// (the others get padding from `Block::inner`), so this is the single source for
/// its width clamp and `wrapped_line_count`. If vertical padding is added, the
/// `+ 2` in `render_confirm_overlay`'s height must become `+ 4`, or long messages
/// clip.
pub(super) const OVERLAY_CHROME_X: u16 = 2 + 2 * OVERLAY_PAD_X;

pub(super) fn render_text_prompt_overlay(frame: &mut Frame, prompt: &TextPrompt) {
    let area = centered_rect(frame.area(), OVERLAY_PROMPT_WIDTH, OVERLAY_PROMPT_HEIGHT);
    frame.render_widget(Clear, area);
    let text = cursor_text(&prompt.query, prompt.cursor);
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT))
                .padding(Padding::horizontal(OVERLAY_PAD_X))
                .title(prompt.title.clone()),
        ),
        area,
    );
}

/// Greedy word-wrap, close enough to `Paragraph`'s `Wrap` to predict it:
/// ratatui cannot report a paragraph's wrapped lines before rendering, so both
/// count and content are computed here. A word longer than `width` gets its own
/// line instead of being split.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

pub(super) fn wrapped_line_count(text: &str, width: u16) -> u16 {
    (wrap_words(text, width as usize).len() as u16).max(1)
}

#[cfg(test)]
mod wrap_words_tests {
    use super::*;

    #[test]
    fn fits_on_one_line() {
        assert_eq!(
            wrap_words("Quit spot-tui? y/n", 60),
            vec!["Quit spot-tui? y/n".to_string()]
        );
    }

    #[test]
    fn wraps_into_the_exact_pieces() {
        let text = "aaaa aaaa aaaa aaaa aaaa";
        assert_eq!(
            wrap_words(text, 10),
            vec![
                "aaaa aaaa".to_string(),
                "aaaa aaaa".to_string(),
                "aaaa".to_string()
            ]
        );
    }

    #[test]
    fn a_word_longer_than_the_width_gets_its_own_line_not_split() {
        assert_eq!(
            wrap_words("supercalifragilisticexpialidocious", 10),
            vec!["supercalifragilisticexpialidocious".to_string()]
        );
    }

    #[test]
    fn empty_text_is_one_empty_line_not_zero() {
        assert_eq!(wrap_words("", 20), vec![String::new()]);
    }
}

/// A `Fetch::Failed` message in a bordered box rather than a bare line: API
/// errors are common enough to deserve a legible state of their own.
pub(super) fn render_fetch_error(frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(format!("failed to load: {message}"))
            .style(Style::default().fg(DANGER))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(DANGER)),
            ),
        area,
    );
}

/// The app's one loading state -- previously inlined separately at every
/// call site with three different treatments (bare default-color text,
/// bare text in the body while a bold "loading…" sat in the header row,
/// or nothing styled at all). One dim, lowercase, wordless-except-the-
/// ellipsis line, matching `render_fetch_error`'s own restraint.
pub(super) fn render_loading(frame: &mut Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new("loading\u{2026}").style(Style::default().fg(DIM)),
        area,
    );
}

/// A designed empty state: a dim headline naming what's absent, then an
/// optional fainter-in-spirit (same DIM color, second line) hint naming
/// the actual key that fixes it. `hint` is `None` where no key genuinely
/// applies -- never a fabricated "press X" that would be a lie at that
/// call site. Lowercase throughout, matching the mockup's own copy
/// convention (`ui.rs`'s existing "no matches"/"nothing here yet" now
/// route through this instead of being bare unstyled strings).
pub(super) fn render_empty_state(
    frame: &mut Frame,
    area: Rect,
    headline: &str,
    hint: Option<&str>,
) {
    let mut lines = vec![Line::from(Span::styled(
        headline.to_string(),
        Style::default().fg(DIM),
    ))];
    if let Some(hint) = hint {
        lines.push(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(DIM),
        )));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

pub(super) fn render_confirm_overlay(frame: &mut Frame, confirm: &PendingConfirm) {
    let frame_area = frame.area();
    // Messages wrap and the box grows to fit: long ones (a duplicate-track
    // warning naming both the track and the playlist) would otherwise run off
    // both edges.
    let max_width = frame_area
        .width
        .saturating_sub(4)
        .clamp(OVERLAY_CONFIRM_MIN_WIDTH, OVERLAY_CONFIRM_MAX_WIDTH);
    let width =
        (confirm.message.chars().count() as u16 + 4).clamp(OVERLAY_CONFIRM_MIN_WIDTH, max_width);
    let inner_width = width.saturating_sub(OVERLAY_CHROME_X);
    let height = (wrapped_line_count(&confirm.message, inner_width) + 2).min(frame_area.height);
    let area = centered_rect(frame_area, width, height);
    let color = match confirm.action.severity() {
        ConfirmSeverity::Danger => DANGER,
        ConfirmSeverity::Warn => WARN,
        ConfirmSeverity::Neutral => ACCENT,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(confirm.message.clone())
            .alignment(Alignment::Center)
            .style(Style::default().fg(color))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color))
                    .padding(Padding::horizontal(OVERLAY_PAD_X)),
            ),
        area,
    );
}

#[cfg(test)]
mod confirm_overlay_tests {
    use super::*;

    #[test]
    fn short_message_fits_on_one_line() {
        assert_eq!(wrapped_line_count("Quit spot-tui? y/n", 60), 1);
    }

    #[test]
    fn long_message_wraps_to_the_expected_number_of_lines() {
        // 5 words of 4 chars each ("aaaa" x5) at width 10 fits "aaaa aaaa"
        // (9 chars) per line, one word per line beyond that -- 3 lines.
        let text = "aaaa aaaa aaaa aaaa aaaa";
        assert_eq!(wrapped_line_count(text, 10), 3);
    }

    #[test]
    fn a_word_longer_than_the_width_still_counts_as_one_line() {
        assert_eq!(
            wrapped_line_count("supercalifragilisticexpialidocious", 10),
            1
        );
    }

    #[test]
    fn empty_message_is_one_line_not_zero() {
        assert_eq!(wrapped_line_count("", 20), 1);
    }
}

/// Whether `playlist_uri` is known to already contain `track_uri`, per
/// `AppState::playlist_membership`. "Unknown" is a distinct third answer, not
/// "no": an incomplete cache must never claim a track is absent from a
/// playlist nobody has looked inside.
pub(super) fn playlist_has_track(
    membership: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    playlist_uri: &str,
    track_uri: &str,
) -> bool {
    membership
        .get(playlist_uri)
        .is_some_and(|tracks| tracks.contains(track_uri))
}

#[cfg(test)]
mod playlist_has_track_tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn known_member_returns_true() {
        let mut membership = HashMap::new();
        membership.insert("p1".to_string(), HashSet::from(["t1".to_string()]));
        assert!(playlist_has_track(&membership, "p1", "t1"));
    }

    #[test]
    fn known_playlist_without_this_track_returns_false_not_a_confirmed_claim() {
        let mut membership = HashMap::new();
        membership.insert("p1".to_string(), HashSet::from(["t2".to_string()]));
        assert!(!playlist_has_track(&membership, "p1", "t1"));
    }

    #[test]
    fn never_checked_playlist_returns_false() {
        let membership = HashMap::new();
        assert!(!playlist_has_track(&membership, "p1", "t1"));
    }
}

/// Draws the shared shape both list-style overlays (picker, quick jump)
/// use -- `Clear`, a bordered+padded block with a title, and the live
/// filter line with a real mid-string cursor (previously trailing-only
/// on both, which actively lied: both key handlers already support real
/// `Left`/`Right` cursor movement) -- and hands back the `Rect` the
/// caller should draw its own genuinely-different body into.
/// Deliberately stops at the chrome: the picker has real `Fetch`-state
/// arms of its own and quick jump doesn't, so pushing that into a shared
/// enum would be more machinery than the duplication it removes.
pub(super) fn filter_overlay_body(frame: &mut Frame, title: &str, filter: &ListFilter) -> Rect {
    let area = centered_rect(frame.area(), OVERLAY_LIST_WIDTH, OVERLAY_LIST_HEIGHT);
    frame.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .padding(Padding::horizontal(OVERLAY_PAD_X))
        .title(title.to_string());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    frame.render_widget(
        Paragraph::new(format!("/{}", cursor_text(&filter.query, filter.cursor))),
        chunks[0],
    );
    chunks[1]
}

pub(super) fn render_playlist_picker_overlay(
    frame: &mut Frame,
    app: &AppState,
    picker: &PlaylistPicker,
    list_state: &mut ListState,
) {
    let body = filter_overlay_body(frame, "Add to playlist", &picker.filter);

    let label = |p: &crate::api::library::PlaylistSummary| p.name.clone();
    match &app.library.playlists {
        Fetch::NotStarted | Fetch::Loading => {
            render_loading(frame, body);
        }
        Fetch::Failed(e) => {
            render_fetch_error(frame, body, e);
        }
        Fetch::Ready(items) if items.is_empty() => {
            render_empty_state(
                frame,
                body,
                "no playlists yet",
                Some("press c to create one"),
            );
        }
        Fetch::Ready(items) => {
            let ordered = pinned_first(
                filtered_sorted(items, &picker.filter, &label),
                &app.pinned_playlists,
                |p| p.uri.as_str(),
            );
            // Two fixed marker columns, never overlapping: pin first
            // (unchanged), membership second. The membership marker only
            // ever makes a *positive* claim -- "known absent" and "never
            // checked" both render blank, so an incomplete cache can
            // never state something false. Plain text, not a styled
            // `Span` -- `render_display_list` already colors the whole
            // selected row ACCENT+BOLD, so this inherits that for free
            // instead of needing its own color (and risking the same
            // pin-marker-vs-selection clash already fixed once this
            // session by making selection win).
            let pin_label = |p: &crate::api::library::PlaylistSummary| {
                let pin = if app.pinned_playlists.contains(&p.uri) {
                    "*"
                } else {
                    " "
                };
                let member =
                    if playlist_has_track(&app.playlist_membership, &p.uri, &picker.track_uri) {
                        "\u{2713}"
                    } else {
                        " "
                    };
                format!("{pin}{member} {}", label(p))
            };
            render_display_list(
                frame,
                body,
                &ordered,
                picker.selected,
                &pin_label,
                picker.filter.query.is_empty(),
                list_state,
            );
        }
    }
}

/// The quick-jump palette. Shares `filter_overlay_body` with the playlist
/// picker, over the pool `quick_jump_entries` rebuilds from live `AppState`
/// each frame, so a fetch landing while it is open shows up at once.
pub(super) fn render_quick_jump_overlay(
    frame: &mut Frame,
    app: &AppState,
    qj: &QuickJump,
    list_state: &mut ListState,
) {
    let body = filter_overlay_body(frame, "Quick jump", &qj.filter);
    let entries = quick_jump_entries(app, &qj.filter);
    let label = |e: &QuickJumpEntry| e.label.clone();
    let display = filtered_sorted(&entries, &qj.filter, &label);
    render_display_list(
        frame,
        body,
        &display,
        qj.selected,
        &label,
        qj.filter.query.is_empty(),
        list_state,
    );
}
