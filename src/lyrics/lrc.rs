use super::{LyricLine};
use std::time::Duration;

const MAX_TAGS_PER_LINE: usize = 32;
const MAX_LINES: usize = 10_000;

/// Parses LRC-format synced lyrics. Metadata tags (`[ar:]`, `[ti:]`,
/// `[length:]`, etc.) are silently skipped -- only tags matching
/// `mm:ss(.frac)?` are treated as timestamps. A line may carry multiple
/// timestamp tags (repeated chorus lines share one text). Output is sorted
/// ascending by timestamp so callers can binary-search it.
///
/// The input is remote, so output is bounded: a line repeats its text once
/// per timestamp tag, and unbounded tags would multiply a long text.
pub fn parse_lrc(input: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();

    for raw_line in input.split('\n') {
        let raw_line = raw_line.trim_end_matches('\r');
        let mut rest = raw_line;
        let mut timestamps = Vec::new();

        while let Some(tag) = rest.strip_prefix('[') {
            let Some(end) = tag.find(']') else { break };
            let tag_content = &tag[..end];
            if let Some(ts) = parse_timestamp_tag(tag_content)
                && timestamps.len() < MAX_TAGS_PER_LINE
            {
                timestamps.push(ts);
            }
            rest = &tag[end + 1..];
        }

        if timestamps.is_empty() {
            continue;
        }

        let text = rest.trim().to_string();
        for ts in timestamps {
            if lines.len() >= MAX_LINES {
                break;
            }
            lines.push(LyricLine {
                timestamp: ts,
                text: text.clone(),
                words: Vec::new(),
            });
        }
    }

    lines.sort_by_key(|a| a.timestamp);
    lines
}

/// Parses a `mm:ss(.frac)?` tag body into a Duration. Returns `None` for
/// anything else (metadata tags like `ar:Radiohead` or `length:03:59`).
fn parse_timestamp_tag(tag: &str) -> Option<Duration> {
    let (mm_str, ss_frac_str) = tag.split_once(':')?;
    let minutes: u64 = mm_str.parse().ok()?;
    let seconds: f64 = ss_frac_str.parse().ok()?;
    // Lyrics are remote input: `inf`, `NaN` and negatives parse as f64 but
    // would make `from_secs_f64` panic.
    Duration::try_from_secs_f64(minutes as f64 * 60.0 + seconds).ok()
}

#[cfg(test)]
mod parse_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn non_finite_or_negative_timestamps_are_skipped_not_a_panic() {
        let input = "[00:inf] a\n[00:NaN] b\n[00:-5] c\n[00:01.00] ok";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "ok");
    }

    #[test]
    fn a_line_cannot_multiply_its_text_by_unbounded_tags() {
        let input = format!("{}text", "[00:01.00]".repeat(100_000));
        assert_eq!(parse_lrc(&input).len(), MAX_TAGS_PER_LINE);
    }

    #[test]
    fn total_lines_are_capped() {
        let input = "[00:01.00]x\n".repeat(MAX_LINES + 500);
        assert_eq!(parse_lrc(&input).len(), MAX_LINES);
    }

    #[test]
    fn parses_basic_two_line_lrc() {
        let input = "[00:19.16] When you were here before\n[00:24.09] Couldn't look you in the eye";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp, Duration::from_secs_f64(19.16));
        assert_eq!(lines[0].text, "When you were here before");
        assert_eq!(lines[1].timestamp, Duration::from_secs_f64(24.09));
        assert_eq!(lines[1].text, "Couldn't look you in the eye");
    }

    #[test]
    fn skips_metadata_tags_that_arent_timestamps() {
        let input = "[ar:Radiohead]\n[al:Pablo Honey]\n[ti:Creep]\n[length:03:59]\n[00:19.16] When you were here before";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "When you were here before");
    }

    #[test]
    fn multiple_timestamps_share_one_lyric_line() {
        let input = "[00:10.00][00:20.00] repeated chorus line";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].timestamp, Duration::from_secs(10));
        assert_eq!(lines[0].text, "repeated chorus line");
        assert_eq!(lines[1].timestamp, Duration::from_secs(20));
        assert_eq!(lines[1].text, "repeated chorus line");
    }

    #[test]
    fn blank_text_after_timestamp_is_kept_as_gap() {
        let input = "[00:10.00]\n[00:15.00] Next line";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "");
        assert_eq!(lines[1].text, "Next line");
    }

    #[test]
    fn handles_crlf_line_endings() {
        let input = "[00:19.16] line one\r\n[00:24.09] line two\r\n";
        let lines = parse_lrc(input);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "line one");
        assert_eq!(lines[1].text, "line two");
    }

    #[test]
    fn sorts_output_by_timestamp_even_if_input_is_out_of_order() {
        let input = "[00:30.00] later line\n[00:05.00] earlier line";
        let lines = parse_lrc(input);
        assert_eq!(lines[0].text, "earlier line");
        assert_eq!(lines[1].text, "later line");
    }

    #[test]
    fn accepts_two_or_three_digit_fractional_seconds_as_equal_duration() {
        let two_digit = parse_lrc("[00:19.16] a");
        let three_digit = parse_lrc("[00:19.160] a");
        assert_eq!(two_digit[0].timestamp, three_digit[0].timestamp);
    }
}

