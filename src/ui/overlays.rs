use super::*;

/// A `Rect` of `width` x `height` centered within `area`, clamped so it
/// never exceeds `area` on a narrow/short terminal.
pub(super) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

// Every overlay used its own bare width/height literals (40x13, 50x16,
// 50x3, and confirm's own self-sizing) with nothing shared -- one width
// for the two list-shaped overlays (picker, quick jump) so they read as
// one system, and a named horizontal-padding amount applied to all four.
pub(super) const OVERLAY_LIST_WIDTH: u16 = 50;
pub(super) const OVERLAY_LIST_HEIGHT: u16 = 16;
pub(super) const OVERLAY_PROMPT_WIDTH: u16 = 50;
pub(super) const OVERLAY_PROMPT_HEIGHT: u16 = 3; // 1 content row + 2 borders -- no vertical padding
pub(super) const OVERLAY_CONFIRM_MIN_WIDTH: u16 = 24;
pub(super) const OVERLAY_CONFIRM_MAX_WIDTH: u16 = 70;
/// Horizontal-only: a blank row costs real percentage height in a
/// 13-16-row list overlay for no benefit the border doesn't already
/// give; horizontal has a real payoff since content otherwise sits flush
/// against the border everywhere else in the app doesn't.
pub(super) const OVERLAY_PAD_X: u16 = 1;
/// Borders (2) + horizontal padding (2x `OVERLAY_PAD_X`) -- everything
/// between an overlay's outer width and its usable text width. The
/// confirm overlay predicts its own wrapped height by hand rather than
/// going through `Block::inner` (the other three overlays get padding
/// subtracted for free), so this constant must stay the single source
/// of truth for both its width-clamp formula and the width it feeds to
/// `wrapped_line_count` -- if vertical padding is ever added, the `+ 2`
/// in `render_confirm_overlay`'s height formula must become `+ 4` at the
/// same time, or long messages clip again.
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

/// Greedy word-wrap into the actual line strings, matching `Paragraph`'s
/// own `Wrap` behavior closely enough to predict it -- there's no way to
/// ask ratatui how many lines (or which lines) a `Paragraph` will wrap to
/// before rendering it, so both count and content are predicted here
/// separately. A word longer than `width` still gets its own line rather
/// than being split mid-word, matching `Wrap`'s own behavior.
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
        assert_eq!(wrap_words("Quit spot-tui? y/n", 60), vec!["Quit spot-tui? y/n".to_string()]);
    }

    #[test]
    fn wraps_into_the_exact_pieces() {
        let text = "aaaa aaaa aaaa aaaa aaaa";
        assert_eq!(
            wrap_words(text, 10),
            vec!["aaaa aaaa".to_string(), "aaaa aaaa".to_string(), "aaaa".to_string()]
        );
    }

    #[test]
    fn a_word_longer_than_the_width_gets_its_own_line_not_split() {
        assert_eq!(wrap_words("supercalifragilisticexpialidocious", 10), vec!["supercalifragilisticexpialidocious".to_string()]);
    }

    #[test]
    fn empty_text_is_one_empty_line_not_zero() {
        assert_eq!(wrap_words("", 20), vec![String::new()]);
    }
}

/// Wraps a `Fetch::Failed` message in a bordered box instead of a bare
/// line of text -- designed as its own state (per fable-ui-design), not
/// a stripped-down list. A real 400 was this project's single most-
/// repeated live bug class; it deserves to be legible, not just present.
pub(super) fn render_fetch_error(frame: &mut Frame, area: Rect, message: &str) {
    frame.render_widget(
        Paragraph::new(format!("failed to load: {message}"))
            .style(Style::default().fg(DANGER))
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(DANGER))),
        area,
    );
}

/// The app's one loading state -- previously inlined separately at every
/// call site with three different treatments (bare default-color text,
/// bare text in the body while a bold "loading…" sat in the header row,
/// or nothing styled at all). One dim, lowercase, wordless-except-the-
/// ellipsis line, matching `render_fetch_error`'s own restraint.
pub(super) fn render_loading(frame: &mut Frame, area: Rect) {
    frame.render_widget(Paragraph::new("loading\u{2026}").style(Style::default().fg(DIM)), area);
}

/// A designed empty state: a dim headline naming what's absent, then an
/// optional fainter-in-spirit (same DIM color, second line) hint naming
/// the actual key that fixes it. `hint` is `None` where no key genuinely
/// applies -- never a fabricated "press X" that would be a lie at that
/// call site. Lowercase throughout, matching the mockup's own copy
/// convention (`ui.rs`'s existing "no matches"/"nothing here yet" now
/// route through this instead of being bare unstyled strings).
pub(super) fn render_empty_state(frame: &mut Frame, area: Rect, headline: &str, hint: Option<&str>) {
    let mut lines = vec![Line::from(Span::styled(headline.to_string(), Style::default().fg(DIM)))];
    if let Some(hint) = hint {
        lines.push(Line::from(Span::styled(hint.to_string(), Style::default().fg(DIM))));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

pub(super) fn render_confirm_overlay(frame: &mut Frame, confirm: &PendingConfirm) {
    let frame_area = frame.area();
    // Fixed at a max of 60 cols with no wrapping originally -- fine for
    // short messages ("Quit spot-tui? y/n") but the newer, longer ones
    // (duplicate-track warnings naming both the track and the playlist)
    // ran off both edges of the box with no way to read the rest,
    // reported live as "completely cutoff." Now wraps, and the box grows
    // to fit however many lines that takes instead of assuming one.
    let max_width = frame_area.width.saturating_sub(4).clamp(OVERLAY_CONFIRM_MIN_WIDTH, OVERLAY_CONFIRM_MAX_WIDTH);
    let width = (confirm.message.chars().count() as u16 + 4).clamp(OVERLAY_CONFIRM_MIN_WIDTH, max_width);
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
        assert_eq!(wrapped_line_count("supercalifragilisticexpialidocious", 10), 1);
    }

    #[test]
    fn empty_message_is_one_line_not_zero() {
        assert_eq!(wrapped_line_count("", 20), 1);
    }
}

/// Reuses `render_display_list` (the same helper every other list in the
/// app already uses) specifically for its `ListState`-backed scrolling --
/// the picker's first version built its rows as a plain `Paragraph`,
/// which never scrolls at all, so a playlist past the visible height was
/// simply unreachable (reported live).
/// Whether `playlist_uri` is known to already contain `track_uri`, per
/// `AppState::playlist_membership`'s own doc comment on why "unknown" is
/// a real, distinct third answer here, not just "no" -- an incomplete
/// cache must never claim a track is confirmed absent from a playlist
/// nobody's looked inside yet this session.
pub(super) fn playlist_has_track(
    membership: &std::collections::HashMap<String, std::collections::HashSet<String>>,
    playlist_uri: &str,
    track_uri: &str,
) -> bool {
    membership.get(playlist_uri).is_some_and(|tracks| tracks.contains(track_uri))
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
    let chunks =
        Layout::default().direction(Direction::Vertical).constraints([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    frame.render_widget(Paragraph::new(format!("/{}", cursor_text(&filter.query, filter.cursor))), chunks[0]);
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
            render_empty_state(frame, body, "no playlists yet", Some("press c to create one"));
        }
        Fetch::Ready(items) => {
            let ordered =
                pinned_first(filtered_sorted(items, &picker.filter, &label), &app.pinned_playlists, |p| p.uri.as_str());
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
                let pin = if app.pinned_playlists.contains(&p.uri) { "*" } else { " " };
                let member =
                    if playlist_has_track(&app.playlist_membership, &p.uri, &picker.track_uri) { "\u{2713}" } else { " " };
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

/// Phase 12's quick-jump palette. Built on `filter_overlay_body` -- the
/// same shared chrome `render_playlist_picker_overlay` uses -- over the
/// flattened, heterogeneous pool `quick_jump_entries` builds fresh from
/// live `AppState` every render, so a background fetch (eager-triggered
/// on open) landing while this is open shows up on the very next frame
/// with no extra plumbing.
pub(super) fn render_quick_jump_overlay(frame: &mut Frame, app: &AppState, qj: &QuickJump, list_state: &mut ListState) {
    let body = filter_overlay_body(frame, "Quick jump", &qj.filter);
    let entries = quick_jump_entries(app, &qj.filter);
    let label = |e: &QuickJumpEntry| e.label.clone();
    let display = filtered_sorted(&entries, &qj.filter, &label);
    render_display_list(frame, body, &display, qj.selected, &label, qj.filter.query.is_empty(), list_state);
}

