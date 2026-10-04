use super::*;

/// The one accent color (progress fill, current lyric line); everything else is
/// default or dim. Indexed rather than RGB so it renders over tmux-256color.
pub(super) const ACCENT: Color = Color::Indexed(35); // a spotify-adjacent green
/// Amber: a confirmation that is easy to undo (e.g. adding a duplicate), as
/// distinct from `DANGER`, so the border alone signals the stakes.
pub(super) const WARN: Color = Color::Indexed(214); // amber, same 256-color-safe reasoning as ACCENT
/// Red: irreversible or failed (delete, remove, error box, status errors).
pub(super) const DANGER: Color = Color::Red;
/// Secondary text: captions, metadata beside a title, empty states. A single
/// tone, since terminal palettes remap dark grays unpredictably.
pub(super) const DIM: Color = Color::DarkGray;

/// `mm:ss`, minutes uncapped (a >59min track just shows e.g. "61:05"
/// rather than growing an hours field nobody needs here).
pub(super) fn format_mmss(d: Duration) -> String {
    let total_secs = d.as_secs();
    format!("{}:{:02}", total_secs / 60, total_secs % 60)
}

/// Truncates to `max` characters, adding an ellipsis if anything was cut. Counts
/// chars, not display columns, so wide (CJK) text can still overrun; fine for a
/// header, revisit with `unicode-width` if it matters.
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

/// Accent when the pane has focus, dim otherwise. Always drawn, so nothing
/// resizes when `Tab` moves focus.
pub(super) fn focus_border_style(active: bool) -> Style {
    if active {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
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
