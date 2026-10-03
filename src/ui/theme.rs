use super::*;

/// The one deliberate accent color (progress bar fill + current lyric
/// line). Everything else stays default/dim -- restraint per
/// fable-ui-design: one bold moment, not color everywhere. Indexed
/// (not RGB) so it renders correctly over plain tmux-256color, not just
/// true-color terminals.
pub(super) const ACCENT: Color = Color::Indexed(35); // a spotify-adjacent green
/// Confirm-overlay severity tier for a heads-up that's easy to undo
/// (e.g. adding a duplicate track) -- distinct from `Color::Red`
/// (irreversible: delete playlist, remove track) so the border color
/// alone signals how carefully to read the message before answering,
/// instead of every confirm using the same red regardless of stakes.
pub(super) const WARN: Color = Color::Indexed(214); // amber, same 256-color-safe reasoning as ACCENT
/// Irreversible/failed -- the severity tier above WARN. Named rather than
/// a new value: this is the exact `Color::Red` the error box, the Danger
/// confirm tier, and the status line already used literally, so naming
/// it changes zero pixels and makes the next call site that needs it
/// obvious rather than another bare `Color::Red`.
pub(super) const DANGER: Color = Color::Red;
/// Secondary/dim text: captions, meta facts beside a title, empty-state
/// copy. `Color::DarkGray`, matching the dozen call sites that already
/// reach for it literally. Deliberately not a second, dimmer tone
/// matching the mockup's `--text-dim`/`--text-faint` split -- both
/// terminal equivalents are theme-remapped colors (in many palettes
/// they'd be indistinguishable or inverted), so this ports the
/// hierarchy (primary vs. secondary), not the literal two-step scale.
pub(super) const DIM: Color = Color::DarkGray;

/// `mm:ss`, minutes uncapped (a >59min track just shows e.g. "61:05"
/// rather than growing an hours field nobody needs here).
pub(super) fn format_mmss(d: Duration) -> String {
    let total_secs = d.as_secs();
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// Truncates to at most `max` characters, appending an ellipsis if
/// anything was cut. Operates on chars, not bytes, so it's safe on
/// multi-byte UTF-8 (this library has CJK titles). Note: this counts
/// characters, not terminal display columns -- a CJK title truncated to
/// `max` chars can still render wider than `max` columns, since each such
/// glyph is double-width. Acceptable approximation for a header line;
/// revisit with `unicode-width` if it ever looks wrong in practice.
pub(super) fn truncate_ellipsis(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn format_mmss_pads_single_digit_seconds() {
        assert_eq!(format_mmss(Duration::from_secs(5)), "0:05");
    }

    #[test]
    fn format_mmss_zero() {
        assert_eq!(format_mmss(Duration::ZERO), "0:00");
    }

    #[test]
    fn format_mmss_rolls_minutes() {
        assert_eq!(format_mmss(Duration::from_secs(65)), "1:05");
    }

    #[test]
    fn format_mmss_uncapped_minutes_past_an_hour() {
        assert_eq!(format_mmss(Duration::from_secs(3661)), "61:01");
    }

    #[test]
    fn truncate_short_string_is_unchanged() {
        assert_eq!(truncate_ellipsis("abc", 10), "abc");
    }

    #[test]
    fn truncate_exact_length_boundary_is_unchanged() {
        assert_eq!(truncate_ellipsis("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_long_ascii_string_gets_ellipsis() {
        // 4 chars kept + ellipsis = 5 chars total, matching `max`.
        assert_eq!(truncate_ellipsis("abcdefgh", 5), "abcd\u{2026}");
        assert_eq!(truncate_ellipsis("abcdefgh", 5).chars().count(), 5);
    }

    #[test]
    fn truncate_is_char_boundary_safe_on_multibyte_text() {
        // Real title from the library this was tested against -- must not
        // panic by slicing mid-codepoint.
        let title = "友人A君を私の伴奏者に任命しま";
        let result = truncate_ellipsis(title, 5);
        assert_eq!(result.chars().count(), 5);
        assert!(result.ends_with('\u{2026}'));
    }

    #[test]
    fn truncate_max_zero_is_empty() {
        assert_eq!(truncate_ellipsis("anything", 0), "");
    }
}

/// Accent when this pane currently has focus, dim otherwise -- always
/// present (never fully absent) so nothing changes size or jumps when
/// `Tab` toggles which pane it is. Only one pane is ever accented at a
/// time: the sidebar's vertical divider previously changing color was
/// easy to miss as the sole focus cue; this gives Main pane an equally
/// visible signal of its own, reported live as missing entirely.
pub(super) fn focus_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
}

