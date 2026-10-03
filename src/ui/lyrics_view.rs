use super::*;

/// What a lyric line shows: its romanization (text and re-timed words) when
/// romanized lyrics are on and this line has one, else the native line. A
/// romanized line whose words couldn't be re-timed is drawn whole rather than
/// swept with the native words, which would colour the wrong text.
pub(super) fn display_line<'a>(
    line: &'a crate::lyrics::LyricLine,
    roman: Option<&'a crate::lyrics::romanize::RomanLine>,
    romanize: bool,
) -> (&'a str, &'a [crate::lyrics::WordSeg]) {
    match roman {
        Some(roman) if romanize => (roman.text.as_str(), roman.words.as_slice()),
        _ => (line.text.as_str(), line.words.as_slice()),
    }
}

/// The text lines of unsynced lyrics as they should be shown: romanized where
/// romanization is on and a line has one, native otherwise. Blank lines stay
/// so the layout doesn't shift, and a result that doesn't line up with the
/// text (a different number of lines) is ignored rather than misplaced.
pub(super) fn plain_display_lines(
    text: &str,
    roman: Option<&[Option<crate::lyrics::romanize::RomanLine>]>,
    romanize: bool,
) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let roman = roman.filter(|r| romanize && r.len() == lines.len());
    lines
        .iter()
        .enumerate()
        .map(|(i, native)| roman.and_then(|r| r[i].as_ref()).map_or_else(|| (*native).to_string(), |r| r.text.clone()))
        .collect()
}

pub(super) fn body_lines(app: &AppState) -> Vec<Line<'static>> {
    match &app.lyrics {
        // `header()` (this screen's title line, and the persistent
        // playback bar's idle text) already carries the "press / to
        // search" instruction -- this used to repeat the identical
        // sentence here too, so an idle Now Playing screen showed it
        // twice in the same frame. This says something lyrics-area-
        // appropriate instead, matching the tone of the other
        // non-synced states below (e.g. `SessionEnded`'s own distinct
        // line) rather than duplicating the header's.
        LyricsState::Idle => vec![Line::from("nothing playing yet")],
        // Was "restart spot-tui to reconnect" -- stale from before Tier 4's
        // auto-reconnect existed. The 'outer loop (main.rs) retries forever
        // with capped exponential backoff and never gives up on its own, so
        // telling the user to restart was simply wrong the whole time this
        // screen has been reachable: the fix is already in progress the
        // moment this message shows.
        LyricsState::SessionEnded => vec![
            Line::from("session disconnected -- reconnecting\u{2026}"),
            Line::from("no need to restart, this usually clears in a few seconds"),
        ],
        LyricsState::Loading => vec![Line::from("fetching lyrics\u{2026}")],
        LyricsState::Instrumental => vec![Line::from("\u{266a} instrumental")],
        LyricsState::NotFound => vec![Line::from("no lyrics found")],
        LyricsState::Plain(text) => vec![Line::from("(unsynced)")]
            .into_iter()
            .chain(
                plain_display_lines(text, app.romanized_lines.as_deref(), app.romanize_lyrics)
                    .into_iter()
                    .map(Line::from),
            )
            .collect(),
        // Shows the whole sheet, not a windowed few lines around the
        // current one -- matches official Spotify's own default lyrics
        // view. `render_now_playing_hero`/`render_fullscreen_hero` are
        // responsible for scrolling the viewport to keep the current
        // line visible (see `center_current_line`); this function just
        // decides what every line looks like, not which ones show.
        // A blank line follows every real one -- ratatui packs lines
        // edge to edge by default, which read as cramped next to the
        // reference's generous line height. Each real line occupies 2
        // rendered rows now, so `current_body_line_row` doubles the
        // current-line index to match when it centers the viewport.
        LyricsState::Synced(lines) => {
            if lines.is_empty() {
                return vec![Line::from("no lyrics found")];
            }
            let current = app.current_line.unwrap_or(0);
            let mut out = Vec::with_capacity(lines.len() * 2);
            let romanized = app.romanized_lines.as_deref().filter(|r| r.len() == lines.len());
            for (i, line) in lines.iter().enumerate() {
                let (shown, words) =
                    display_line(line, romanized.and_then(|r| r[i].as_ref()), app.romanize_lyrics);
                let text = if shown.is_empty() { "\u{266a}".to_string() } else { shown.to_string() };
                let styled = if i == current && !words.is_empty() {
                    // Word-by-word: same text, coloured by how far the voice
                    // has got. Sung = accent, still to come = white, both bold
                    // so nothing shifts as the sweep passes.
                    let sung = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
                    let unsung = Style::default().fg(Color::White).add_modifier(Modifier::BOLD);
                    Line::from(
                        sweep_runs(words, app.position.as_secs_f64())
                            .into_iter()
                            .map(|(run, fill)| Span::styled(run, if fill == Fill::Sung { sung } else { unsung }))
                            .collect::<Vec<_>>(),
                    )
                } else if i == current {
                    Line::from(Span::styled(text, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)))
                } else {
                    Line::from(Span::styled(text, Style::default().fg(lyric_tier_color(i.abs_diff(current)))))
                };
                out.push(styled);
                out.push(Line::from(""));
            }
            out
        }
    }
}

/// The 4-tier fade by distance from the current line -- `Color::DarkGray`
/// alone read as ~1.4:1 contrast against this app's near-black
/// background, functionally unreadable for a screen built to show the
/// whole sheet, not just the current line. Used by every `body_lines`
/// caller, compact and fullscreen alike, so they can't drift apart.
pub(super) fn lyric_tier_color(distance: usize) -> Color {
    match distance {
        0 => ACCENT,
        1 => Color::White,
        2..=3 => Color::Gray,
        _ => DIM,
    }
}

/// Whether a stretch of the current line has been sung yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    Sung,
    Unsung,
}

/// The current line as contiguous runs of sung / unsung text at `pos_secs`,
/// for word-by-word highlighting. Inside the word being sung, the sung part
/// is `floor(fraction * chars)` characters -- a real sweep, since a terminal
/// can colour per character -- and a word's trailing space is only sung once
/// the word is finished. Each segment is judged on its own clock, so a
/// background vocal that overlaps the lead sweeps independently. The runs
/// always concatenate to exactly the segments' text (the renderer must never
/// change what is drawn or how it wraps), and adjacent runs with the same
/// fill are merged.
pub fn sweep_runs(words: &[crate::lyrics::WordSeg], pos_secs: f64) -> Vec<(String, Fill)> {
    fn push(runs: &mut Vec<(String, Fill)>, text: &str, fill: Fill) {
        if text.is_empty() {
            return;
        }
        match runs.last_mut() {
            Some((last, last_fill)) if *last_fill == fill => last.push_str(text),
            _ => runs.push((text.to_string(), fill)),
        }
    }

    let mut runs = Vec::new();
    for word in words {
        if pos_secs >= word.end && pos_secs >= word.start {
            push(&mut runs, &word.text, Fill::Sung);
        } else if pos_secs <= word.start {
            push(&mut runs, &word.text, Fill::Unsung);
        } else {
            let body = word.text.trim_end();
            let chars = body.chars().count();
            let fraction = (pos_secs - word.start) / (word.end - word.start);
            let sung = ((fraction * chars as f64).floor() as usize).min(chars);
            let split = body.char_indices().nth(sung).map_or(body.len(), |(i, _)| i);
            push(&mut runs, &word.text[..split], Fill::Sung);
            push(&mut runs, &word.text[split..], Fill::Unsung);
        }
    }
    runs
}

/// True while a word sweep is actually moving: playing, and the current line
/// has word timing. Only then does the event loop redraw faster than its
/// normal tick, so nothing else (line-level lyrics, paused playback) pays for
/// the extra frames.
pub fn word_sweep_active(lyrics: &LyricsState, current_line: Option<usize>, playing: Option<bool>) -> bool {
    if playing != Some(true) {
        return false;
    }
    match (lyrics, current_line) {
        (LyricsState::Synced(lines), Some(i)) => lines.get(i).is_some_and(|line| !line.words.is_empty()),
        _ => false,
    }
}

/// Only `Synced` has a real "current line" to center on -- every other
/// `LyricsState` (idle/instrumental/not-found/plain/loading) has no
/// notion of a current line at all, so they always render from the top.
/// `*2`: `body_lines` interleaves a blank spacer after every real line,
/// so the current line's actual row in the rendered `Vec` is twice its
/// index into the raw synced-lyrics data.
pub(super) fn current_body_line_row(app: &AppState) -> Option<usize> {
    match &app.lyrics {
        LyricsState::Synced(lines) if !lines.is_empty() => Some(app.current_line.unwrap_or(0) * 2),
        _ => None,
    }
}

/// A `Line`'s real on-screen height once `Paragraph`'s own `Wrap` gets to
/// it -- 1 for a blank spacer (nothing to wrap), otherwise the same
/// greedy word-wrap `wrapped_line_count` already uses to size the
/// confirm overlay. Needed because `Paragraph::scroll`'s `y` counts
/// *wrapped* rows, not logical `Line`s (confirmed by reading
/// `ratatui-widgets`' `Paragraph::render_paragraph`: "the scroll offset
/// is applied after the text is wrapped") -- a centering formula that
/// assumes 1 row per `Line` silently drifts further off-center every
/// time an earlier line actually wraps to more than one row, which is
/// exactly what a long lyric line in a narrower pane does. Reported
/// live as the current line reading progressively lower down the screen
/// the further into the song it got -- each additional wrapped line
/// above it added rows this function wasn't accounting for.
pub(super) fn line_row_height(line: &Line<'static>, width: u16) -> usize {
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    if text.trim().is_empty() { 1 } else { wrapped_line_count(&text, width) as usize }
}

/// The current line's own vertical middle, in real wrapped-row units
/// (`line_row_height`), counting from the top of `lines` -- shared by
/// both `center_current_line` (fullscreen) and `top_anchored_offset`
/// (compact), so they can't drift apart on the wrap-awareness fix even
/// though they anchor to different screen positions.
pub(super) fn anchor_row_of(lines: &[Line<'static>], current_row: usize, width: u16) -> usize {
    let heights: Vec<usize> = lines.iter().map(|l| line_row_height(l, width)).collect();
    let rows_before_current: usize = heights[..current_row.min(heights.len())].iter().sum();
    let current_height = heights.get(current_row).copied().unwrap_or(1);
    rows_before_current + current_height / 2
}

pub(super) fn total_row_height(lines: &[Line<'static>], width: u16) -> usize {
    lines.iter().map(|l| line_row_height(l, width)).sum()
}

/// Pads `lines` with `viewport_height / 2` blank rows above and below,
/// and returns the scroll offset that puts the current line's own
/// vertical middle at the exact vertical middle of the viewport --
/// measured in real wrapped rows (`anchor_row_of`), not logical `Line`
/// count, so it stays correct however many of the preceding lines
/// happen to wrap. A plain clamped scroll offset (`ideal =
/// current.saturating_sub(half); ideal.min(total - viewport)`) can't
/// center at either edge of the sheet either -- there's no real content
/// to scroll into above line 0 or below the last line, so a song's
/// opening (or closing) line rendered pinned to the top (or bottom)
/// instead of centered, also reported live. Padding with real blank
/// rows gives the offset somewhere to scroll into even there, so the
/// current line centers unconditionally, including a song's first and
/// last line and a current line that itself wraps to more than one row.
/// Fullscreen only -- see `top_anchored_offset` for the compact view,
/// which was explicitly asked *not* to center this way.
pub(super) fn center_current_line(
    lines: Vec<Line<'static>>,
    current_row: Option<usize>,
    viewport_height: u16,
    width: u16,
) -> (Vec<Line<'static>>, u16) {
    let Some(current_row) = current_row else {
        // No current line to anchor on -- this is a short status message
        // (Loading/"fetching lyrics...", Idle, Instrumental, NotFound,
        // SessionEnded), not a lyric sheet. It still renders through this
        // same fullscreen paragraph, so it needs the same vertical-center
        // treatment real lyrics get here, rather than sitting pinned to
        // the pane's top edge -- reported live ("loading lyrics text is
        // so high - center it like the actual lyrics").
        let total_rows = total_row_height(&lines, width);
        let pad_top = (viewport_height as usize).saturating_sub(total_rows) / 2;
        let mut padded = Vec::with_capacity(lines.len() + pad_top);
        padded.extend(std::iter::repeat_with(|| Line::from("")).take(pad_top));
        padded.extend(lines);
        return (padded, 0);
    };
    let anchor_row = anchor_row_of(&lines, current_row, width);
    let total_rows = total_row_height(&lines, width);

    let half = (viewport_height / 2) as usize;
    let mut padded = Vec::with_capacity(lines.len() + half * 2);
    padded.extend(std::iter::repeat_with(|| Line::from("")).take(half));
    padded.extend(lines);
    padded.extend(std::iter::repeat_with(|| Line::from("")).take(half));

    let padded_total_rows = total_rows + half * 2;
    let max_offset = padded_total_rows.saturating_sub(viewport_height as usize);
    let offset = anchor_row.min(max_offset) as u16;
    (padded, offset)
}

/// The compact (non-fullscreen) Now Playing view's lyrics scroll: keeps
/// a couple of already-seen lines visible above the current one instead
/// of forcing it to the vertical middle the way `center_current_line`
/// does -- explicitly asked for over centering ("in now playing have it
/// at the top, not middle"), since centering there ate a large, fixed
/// share of an already-small pane with blank padding on every render,
/// which is what made the actually-rendered lyric text read as smaller
/// even though nothing about its size had changed. No padding here:
/// unlike the fullscreen view, "settle at the top" (song start) and
/// "settle at the bottom" (song end, once there's more sheet than fits)
/// are both already correct, ordinary scrolling behavior, the same as
/// every other list in this app -- there's nothing to fabricate.
pub(super) fn top_anchored_offset(lines: &[Line<'static>], current_row: Option<usize>, viewport_height: u16, width: u16) -> u16 {
    const TOP_MARGIN: usize = 2;
    let Some(current_row) = current_row else { return 0 };
    let anchor_row = anchor_row_of(lines, current_row, width);
    let total_rows = total_row_height(lines, width);
    let max_offset = total_rows.saturating_sub(viewport_height as usize);
    anchor_row.saturating_sub(TOP_MARGIN).min(max_offset) as u16
}

#[cfg(test)]
mod center_current_line_tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(i.to_string())).collect()
    }

    fn line_text(l: &Line<'static>) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn no_current_row_vertically_centers_a_short_status_message() {
        // 10-row viewport, 5-row message -- pad_top = (10 - 5) / 2 = 2.
        let (out, offset) = center_current_line(lines(5), None, 10, 80);
        assert_eq!(out.len(), 2 + 5);
        assert_eq!(offset, 0);
        assert_eq!(line_text(&out[0]), "");
        assert_eq!(line_text(&out[2]), "0");
    }

    #[test]
    fn no_current_row_with_a_message_taller_than_the_viewport_pads_nothing() {
        let (out, offset) = center_current_line(lines(20), None, 10, 80);
        assert_eq!(out.len(), 20);
        assert_eq!(offset, 0);
    }

    #[test]
    fn the_very_first_line_still_centers_via_top_padding() {
        // viewport 10, half = 5 -- 5 blank rows padded above line 0 means
        // scrolling 0 rows still puts line 0 at screen row 5, dead center.
        let (out, offset) = center_current_line(lines(40), Some(0), 10, 80);
        assert_eq!(offset, 0);
        assert_eq!(out.len(), 40 + 5 + 5);
        assert_eq!(out[5].spans[0].content.as_ref(), "0");
    }

    #[test]
    fn the_very_last_line_still_centers_via_bottom_padding() {
        let (out, offset) = center_current_line(lines(40), Some(39), 10, 80);
        // padded index of line 39 is 5 (top pad) + 39 = 44; centering
        // needs it at screen row 5, so offset = 44 - 5 = 39.
        assert_eq!(offset, 39);
        let screen_row = 44 - offset as usize;
        assert_eq!(screen_row, 5);
        assert_eq!(out[44].spans[0].content.as_ref(), "39");
    }

    #[test]
    fn a_middle_line_centers_at_the_viewport_midpoint() {
        // padded index of line 20 is 5 (top pad) + 20 = 25; offset 20
        // puts it at screen row 5, dead center of a 10-row viewport.
        let (_, offset) = center_current_line(lines(40), Some(20), 10, 80);
        assert_eq!(offset, 20);
        assert_eq!(25 - offset as usize, 5);
    }

    #[test]
    fn a_wrapped_earlier_line_does_not_push_the_current_line_off_center() {
        // "one two three four five" greedy-wraps to 3 rows at width 10.
        // A Line-count-based offset (the old bug) would put line 1's
        // anchor at row 1; the real wrapped anchor is row 3 (after the
        // 3 wrapped rows line 0 actually consumes).
        let lines = vec![Line::from("one two three four five"), Line::from("current")];
        let (_, offset) = center_current_line(lines, Some(1), 10, 10);
        assert_eq!(offset, 3);
    }

    #[test]
    fn a_current_line_that_itself_wraps_anchors_at_its_own_middle() {
        // "current line" (12 chars) wraps to 2 rows against width 6;
        // anchoring at its top row (old behavior) would sit it half a
        // row high of true center -- anchor should land mid-way through
        // its own wrapped block instead.
        let lines = vec![Line::from("current line")];
        let (_, offset) = center_current_line(lines, Some(0), 10, 6);
        // current_height = wrapped_line_count("current line", 6) = 2;
        // anchor_row = 0 + 2/2 = 1.
        assert_eq!(offset, 1);
    }
}

#[cfg(test)]
mod top_anchored_offset_tests {
    use super::*;

    fn lines(n: usize) -> Vec<Line<'static>> {
        (0..n).map(|i| Line::from(i.to_string())).collect()
    }

    #[test]
    fn no_current_row_is_zero() {
        assert_eq!(top_anchored_offset(&lines(5), None, 10, 80), 0);
    }

    #[test]
    fn a_song_s_opening_line_stays_pinned_to_the_actual_top() {
        // No padding, no forced centering -- current row 0 needs no
        // scroll at all, unlike `center_current_line`'s offset 0 which
        // only reads as centered because of the padding it adds.
        assert_eq!(top_anchored_offset(&lines(40), Some(0), 10, 80), 0);
    }

    #[test]
    fn a_middle_line_keeps_a_small_margin_of_context_above_it() {
        // anchor_row = 20; margin 2 -- offset settles 2 rows short of
        // the current line, not at the viewport's vertical middle.
        assert_eq!(top_anchored_offset(&lines(40), Some(20), 10, 80), 18);
    }

    #[test]
    fn near_the_end_clamps_to_the_real_bottom_not_past_it() {
        // total 40, viewport 10 -- max_offset 30. anchor_row 39 minus
        // margin 2 = 37, clamped down to 30 (ordinary scroll-to-end).
        assert_eq!(top_anchored_offset(&lines(40), Some(39), 10, 80), 30);
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use crate::lyrics::WordSeg;

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    fn hi_there() -> Vec<WordSeg> {
        vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)]
    }

    fn run(text: &str, fill: Fill) -> (String, Fill) {
        (text.to_string(), fill)
    }

    fn joined(runs: &[(String, Fill)]) -> String {
        runs.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn before_the_first_word_the_whole_line_is_unsung() {
        assert_eq!(sweep_runs(&hi_there(), 0.5), vec![run("hi there", Fill::Unsung)]);
        assert_eq!(sweep_runs(&hi_there(), 1.0), vec![run("hi there", Fill::Unsung)]);
    }

    #[test]
    fn after_the_last_word_the_whole_line_is_sung() {
        assert_eq!(sweep_runs(&hi_there(), 2.0), vec![run("hi there", Fill::Sung)]);
        assert_eq!(sweep_runs(&hi_there(), 60.0), vec![run("hi there", Fill::Sung)]);
    }

    #[test]
    fn inside_a_word_that_fraction_of_its_characters_is_sung() {
        // "there" is 1.4..2.0; at 1.7 it is half done: floor(0.5 * 5) = 2 chars.
        assert_eq!(
            sweep_runs(&hi_there(), 1.7),
            vec![run("hi th", Fill::Sung), run("ere", Fill::Unsung)]
        );
    }

    #[test]
    fn a_word_s_trailing_space_is_not_sung_until_the_word_is_finished() {
        let words = vec![seg("hi ", 1.0, 1.4), seg("there", 1.4, 2.0)];
        // Half through "hi": floor(0.5 * 2) = 1 char of "hi", the space still unsung.
        assert_eq!(sweep_runs(&words, 1.2), vec![run("h", Fill::Sung), run("i there", Fill::Unsung)]);
        // Exactly at the end of "hi ": the space is sung with it.
        assert_eq!(sweep_runs(&words, 1.4), vec![run("hi ", Fill::Sung), run("there", Fill::Unsung)]);
    }

    #[test]
    fn multibyte_text_is_split_on_characters_not_bytes() {
        let words = vec![seg("\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}", 0.0, 1.0)];
        assert_eq!(
            sweep_runs(&words, 0.5),
            vec![run("\u{3053}\u{3093}", Fill::Sung), run("\u{306b}\u{3061}\u{306f}", Fill::Unsung)]
        );
    }

    #[test]
    fn a_zero_length_word_flips_at_its_start() {
        let words = vec![seg("a ", 1.0, 1.0), seg("b", 2.0, 2.0)];
        assert_eq!(sweep_runs(&words, 0.9), vec![run("a b", Fill::Unsung)]);
        assert_eq!(sweep_runs(&words, 1.0), vec![run("a ", Fill::Sung), run("b", Fill::Unsung)]);
        assert_eq!(sweep_runs(&words, 2.0), vec![run("a b", Fill::Sung)]);
    }

    #[test]
    fn an_overlapping_background_vocal_sweeps_on_its_own_clock() {
        // The background starts while the lead is still going.
        let words = vec![seg("Hey ", 1.0, 1.4), seg("(Oh)", 1.1, 1.9)];
        assert_eq!(
            sweep_runs(&words, 1.5),
            vec![run("Hey (O", Fill::Sung), run("h)", Fill::Unsung)]
        );
    }

    #[test]
    fn the_runs_always_re_form_the_line_whatever_the_position() {
        let words = vec![seg("I ", 27.395, 27.549), seg("been ", 27.549, 27.74), seg("try", 27.74, 27.908), seg("na ", 27.908, 28.077), seg("call", 28.077, 28.96)];
        let text: String = words.iter().map(|w| w.text.as_str()).collect();
        let mut pos = 26.0;
        while pos < 30.0 {
            assert_eq!(joined(&sweep_runs(&words, pos)), text, "at {pos}");
            pos += 0.037;
        }
    }

    #[test]
    fn neighbouring_runs_with_the_same_fill_are_merged() {
        let runs = sweep_runs(&hi_there(), 60.0);
        assert_eq!(runs.len(), 1);
    }

    #[test]
    fn no_words_no_runs() {
        assert!(sweep_runs(&[], 5.0).is_empty());
    }
}

#[cfg(test)]
mod sweep_active_tests {
    use super::*;
    use crate::lyrics::{LyricLine, WordSeg};
    use std::time::Duration;

    fn line(words: bool) -> LyricLine {
        LyricLine {
            timestamp: Duration::from_secs(1),
            text: "hi".to_string(),
            words: if words { vec![WordSeg { text: "hi".to_string(), start: 1.0, end: 2.0 }] } else { Vec::new() },
        }
    }

    #[test]
    fn active_only_while_playing_a_line_that_has_words() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(word_sweep_active(&lyrics, Some(0), Some(true)));
    }

    #[test]
    fn paused_or_stopped_needs_no_faster_redraw() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(false)));
        assert!(!word_sweep_active(&lyrics, Some(0), None));
    }

    #[test]
    fn a_line_level_sync_never_speeds_up_the_redraw() {
        let lyrics = LyricsState::Synced(vec![line(false)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(true)));
    }

    #[test]
    fn only_the_current_line_counts() {
        let lyrics = LyricsState::Synced(vec![line(false), line(true)]);
        assert!(!word_sweep_active(&lyrics, Some(0), Some(true)));
        assert!(word_sweep_active(&lyrics, Some(1), Some(true)));
    }

    #[test]
    fn no_current_line_or_other_lyric_states_are_inactive() {
        let lyrics = LyricsState::Synced(vec![line(true)]);
        assert!(!word_sweep_active(&lyrics, None, Some(true)));
        assert!(!word_sweep_active(&LyricsState::Loading, Some(0), Some(true)));
        assert!(!word_sweep_active(&LyricsState::Synced(vec![line(true)]), Some(9), Some(true)));
    }
}

#[cfg(test)]
mod display_line_tests {
    use super::*;
    use crate::lyrics::{LyricLine, WordSeg};
    use crate::lyrics::romanize::RomanLine;
    use std::time::Duration;

    fn seg(text: &str) -> WordSeg {
        WordSeg { text: text.to_string(), start: 1.0, end: 2.0 }
    }

    fn native() -> LyricLine {
        LyricLine { timestamp: Duration::from_secs(1), text: "\u{541b}".to_string(), words: vec![seg("\u{541b}")] }
    }

    fn roman() -> RomanLine {
        RomanLine { text: "kimi".to_string(), words: vec![seg("kimi")] }
    }

    #[test]
    fn the_native_line_shows_when_romanization_is_off() {
        let (line, roman) = (native(), roman());
        assert_eq!(display_line(&line, Some(&roman), false), ("\u{541b}", &line.words[..]));
    }

    #[test]
    fn the_romanized_line_and_its_words_replace_it_when_on() {
        let (line, roman) = (native(), roman());
        assert_eq!(display_line(&line, Some(&roman), true), ("kimi", &roman.words[..]));
    }

    #[test]
    fn a_line_with_no_romanization_stays_native_even_when_on() {
        let line = native();
        assert_eq!(display_line(&line, None, true), ("\u{541b}", &line.words[..]));
    }

    #[test]
    fn a_romanized_line_without_re_timed_words_is_drawn_whole_not_swept() {
        // Falling back to the native words would sweep the wrong text.
        let line = native();
        let plain = RomanLine { text: "kimi".to_string(), words: Vec::new() };
        let (text, words) = display_line(&line, Some(&plain), true);
        assert_eq!(text, "kimi");
        assert!(words.is_empty());
    }
}

#[cfg(test)]
mod plain_display_tests {
    use super::*;
    use crate::lyrics::romanize::RomanLine;

    fn roman(text: &str) -> Option<RomanLine> {
        Some(RomanLine { text: text.to_string(), words: Vec::new() })
    }

    #[test]
    fn native_text_shows_when_romanization_is_off() {
        let r = vec![roman("kimi")];
        assert_eq!(plain_display_lines("\u{541b}", Some(&r), false), vec!["\u{541b}"]);
    }

    #[test]
    fn romanized_lines_replace_native_ones_when_on() {
        let r = vec![roman("kimi"), None, roman("sayonara")];
        assert_eq!(
            plain_display_lines("\u{541b}\nStay\n\u{3055}\u{3088}\u{306a}\u{3089}", Some(&r), true),
            vec!["kimi", "Stay", "sayonara"]
        );
    }

    #[test]
    fn blank_lines_are_kept_so_the_layout_does_not_shift() {
        let r = vec![roman("kimi"), None, roman("nani")];
        assert_eq!(plain_display_lines("\u{541b}\n\n\u{4f55}", Some(&r), true), vec!["kimi", "", "nani"]);
    }

    #[test]
    fn nothing_computed_yet_shows_native_text() {
        assert_eq!(plain_display_lines("a\nb", None, true), vec!["a", "b"]);
    }

    #[test]
    fn a_result_that_does_not_line_up_is_ignored_rather_than_misplaced() {
        // Defensive: entries for a different number of lines than the text has.
        let r = vec![roman("kimi")];
        assert_eq!(plain_display_lines("a\nb\nc", Some(&r), true), vec!["a", "b", "c"]);
    }
}

