//! Romanization of CJK lyrics (Japanese, Chinese, Korean) into Latin script,
//! computed locally: the Spicy Lyrics API carries romanization for only a
//! small fraction of tracks (2 of 14 sampled), so it cannot be the main path.

pub mod chinese;
pub mod japanese;
pub mod korean;

/// Which language a sheet's Han characters (kanji / hanzi) belong to. They
/// look identical but read completely differently, so it is decided once per
/// sheet from the one unambiguous signal: any kana at all means Japanese.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HanLanguage {
    Japanese,
    Chinese,
}

fn is_kana(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x309F | 0x30A0..=0x30FF | 0xFF66..=0xFF9F)
}

fn is_hangul(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7A3)
}

fn is_han(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x2FDF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF)
}

/// Whether `text` has any Japanese, Chinese or Korean *letters* (punctuation
/// alone doesn't count).
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| is_kana(c) || is_hangul(c) || is_han(c))
}

pub fn han_language<S: AsRef<str>>(lines: &[S]) -> HanLanguage {
    if lines.iter().any(|line| line.as_ref().chars().any(is_kana)) {
        HanLanguage::Japanese
    } else {
        HanLanguage::Chinese
    }
}

/// One line romanized, or `None` when it has no CJK letters (an English line
/// is left as it is) or nothing changed. Hangul runs go to the Korean
/// romanizer first, so a mixed line keeps its Latin words; then any kana or
/// Han characters go to Japanese or Chinese according to `han`.
pub fn romanize_line(text: &str, han: HanLanguage) -> Option<String> {
    if !has_cjk(text) {
        return None;
    }
    let mut result = if text.chars().any(is_hangul) { korean::romanize(text) } else { text.to_string() };
    if result.chars().any(|c| is_kana(c) || is_han(c)) {
        result = match han {
            HanLanguage::Japanese => japanese::romanize(&result),
            HanLanguage::Chinese => chinese::romanize(&result),
        };
    }
    (result != text).then_some(result)
}

use crate::lyrics::WordSeg;

/// How much of a line's romanization each native segment should get: its own
/// romanized length, or its native length when it has nothing to romanize.
/// Read on its own (no context), which is fine for a *share*, not a reading.
pub fn segment_weights(words: &[WordSeg], han: HanLanguage) -> Vec<usize> {
    words
        .iter()
        .map(|word| romanize_line(&word.text, han).unwrap_or_else(|| word.text.clone()).chars().count())
        .collect()
}

/// Re-times a romanized line for the word-by-word sweep. The native segments'
/// characters don't correspond to the romanized ones, so the romanized text
/// is sliced in proportion to `weights` (rounded, by character, never losing
/// or repeating one) and each slice keeps its segment's timing. The slices
/// always concatenate to exactly `romanized`, so the sweep can colour them
/// without changing what is drawn. A segment that gets no characters is
/// dropped.
pub fn remap_words(romanized: &str, words: &[WordSeg], weights: &[usize]) -> Vec<WordSeg> {
    let chars: Vec<char> = romanized.chars().collect();
    let total = chars.len();
    if words.is_empty() || total == 0 {
        return Vec::new();
    }
    let weight = |i: usize| weights.get(i).copied().unwrap_or(1);
    let mut sum: usize = (0..words.len()).map(weight).sum();
    let even = sum == 0;
    if even {
        sum = words.len();
    }

    let mut out = Vec::with_capacity(words.len());
    let (mut cumulative, mut previous) = (0usize, 0usize);
    for (i, word) in words.iter().enumerate() {
        cumulative += if even { 1 } else { weight(i) };
        let boundary = if i + 1 == words.len() { total } else { ((total * cumulative + sum / 2) / sum).min(total) };
        if boundary > previous {
            out.push(WordSeg { text: chars[previous..boundary].iter().collect(), start: word.start, end: word.end });
            previous = boundary;
        }
    }
    out
}

/// A line's romanization for display, with its word timing re-mapped onto the
/// romanized text (empty when the native line had none).
#[derive(Clone, Debug, PartialEq)]
pub struct RomanLine {
    pub text: String,
    pub words: Vec<WordSeg>,
}

/// Whether any line of a sheet has CJK letters (i.e. romanizing is worth
/// doing at all).
pub fn sheet_has_cjk(lines: &[crate::lyrics::LyricLine]) -> bool {
    lines.iter().any(|line| has_cjk(&line.text))
}

/// A whole synced sheet romanized, in step with `lines`: `None` for a line
/// that has no CJK. For a line with word timing, the words are re-timed onto
/// the romanized text so the word-by-word sweep keeps working. Loads the
/// Japanese dictionary on first use, so callers run this off the render thread.
pub fn romanize_lyric_lines(lines: &[crate::lyrics::LyricLine]) -> Vec<Option<RomanLine>> {
    let han = han_language(&lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>());
    lines
        .iter()
        .map(|line| {
            let text = romanize_line(&line.text, han)?;
            let words = if line.words.is_empty() {
                Vec::new()
            } else {
                remap_words(&text, &line.words, &segment_weights(&line.words, han))
            };
            Some(RomanLine { text, words })
        })
        .collect()
}

/// Unsynced lyrics romanized line by line, in step with `text.lines()` (which
/// is how the renderer walks them): `None` for a line with no CJK. There is no
/// word timing to re-map here. Loads the Japanese dictionary on first use, so
/// callers run this off the render thread.
pub fn romanize_plain_lines(text: &str) -> Vec<Option<RomanLine>> {
    let lines: Vec<&str> = text.lines().collect();
    let han = han_language(&lines);
    lines
        .iter()
        .map(|line| romanize_line(line, han).map(|text| RomanLine { text, words: Vec::new() }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_kana_makes_a_sheets_han_characters_japanese() {
        assert_eq!(han_language(&["Stay", "\u{3055}\u{3088}\u{306a}\u{3089}", "\u{904b}\u{547d}"]), HanLanguage::Japanese);
        assert_eq!(han_language(&["\u{30b3}\u{30fc}\u{30d2}\u{30fc}"]), HanLanguage::Japanese);
    }

    #[test]
    fn han_without_kana_is_chinese() {
        assert_eq!(han_language(&["\u{6708}\u{4eae}\u{4ee3}\u{8868}\u{6211}\u{7684}\u{5fc3}"]), HanLanguage::Chinese);
        assert_eq!(han_language::<&str>(&[]), HanLanguage::Chinese);
        assert_eq!(han_language(&["Hello"]), HanLanguage::Chinese);
    }

    #[test]
    fn only_letters_count_as_cjk_not_punctuation() {
        assert!(has_cjk("\u{4f60}\u{597d}"));
        assert!(has_cjk("\u{c548}\u{b155}"));
        assert!(has_cjk("\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"));
        assert!(has_cjk("mixed \u{4f60} text"));
        assert!(!has_cjk("Hello, world"));
        assert!(!has_cjk("\u{3001}\u{3002}\u{ff01}"));
        assert!(!has_cjk(""));
    }

    #[test]
    fn a_line_with_no_cjk_is_left_native() {
        assert_eq!(romanize_line("Stay in the middle", HanLanguage::Japanese), None);
        assert_eq!(romanize_line("", HanLanguage::Chinese), None);
    }

    #[test]
    fn each_language_goes_to_its_own_romanizer() {
        assert_eq!(romanize_line("\u{c548}\u{b155}\u{d558}\u{c138}\u{c694}", HanLanguage::Chinese).as_deref(), Some("annyeonghaseyo"));
        assert_eq!(romanize_line("\u{4f60}\u{597d}", HanLanguage::Chinese).as_deref(), Some("n\u{01d0} h\u{01ce}o"));
        assert_eq!(romanize_line("\u{541b}\u{3068}", HanLanguage::Japanese).as_deref(), Some("kimi to"));
    }

    #[test]
    fn the_same_han_characters_read_differently_by_sheet_language() {
        assert_eq!(romanize_line("\u{904b}\u{547d}", HanLanguage::Japanese).as_deref(), Some("unmei"));
        assert_eq!(romanize_line("\u{904b}\u{547d}", HanLanguage::Chinese).as_deref(), Some("y\u{00f9}n m\u{00ec}ng"));
    }

    #[test]
    fn a_mixed_korean_and_english_line_keeps_the_english() {
        assert_eq!(romanize_line("Stay \u{b108} with me", HanLanguage::Chinese).as_deref(), Some("Stay neo with me"));
    }

}

#[cfg(test)]
mod remap_tests {
    use super::*;
    use crate::lyrics::WordSeg;

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    fn joined(words: &[WordSeg]) -> String {
        words.iter().map(|w| w.text.as_str()).collect()
    }

    #[test]
    fn the_romanized_text_is_shared_out_in_proportion_to_the_weights() {
        let words = vec![seg("a", 0.0, 1.0), seg("bc", 1.0, 3.0)];
        let out = remap_words("xyz", &words, &[1, 2]);
        assert_eq!(out, vec![seg("x", 0.0, 1.0), seg("yz", 1.0, 3.0)]);
    }

    #[test]
    fn the_slices_always_re_form_the_romanized_line() {
        let words = vec![seg("a", 0.0, 1.0), seg("b", 1.0, 2.0), seg("c", 2.0, 3.0)];
        for text in ["hitokoto", "x", "wakatta ne", "n\u{01d0} h\u{01ce}o ma"] {
            for weights in [[1, 1, 1], [5, 1, 1], [1, 9, 2], [0, 3, 0]] {
                assert_eq!(joined(&remap_words(text, &words, &weights)), text, "{text} {weights:?}");
            }
        }
    }

    #[test]
    fn rounding_never_loses_or_repeats_a_character() {
        let words = vec![seg("a", 0.0, 1.0), seg("b", 1.0, 2.0), seg("c", 2.0, 3.0)];
        let out = remap_words("abcde", &words, &[1, 1, 1]);
        assert_eq!(out.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), vec!["ab", "c", "de"]);
    }

    #[test]
    fn slicing_is_by_character_not_byte() {
        let words = vec![seg("a", 0.0, 1.0), seg("b", 1.0, 2.0)];
        let out = remap_words("n\u{01d0}h\u{01ce}o", &words, &[1, 1]);
        assert_eq!(joined(&out), "n\u{01d0}h\u{01ce}o");
        assert_eq!(out[0].text.chars().count() + out[1].text.chars().count(), 5);
    }

    #[test]
    fn each_kept_slice_keeps_its_own_segments_timing() {
        let words = vec![seg("a", 1.5, 2.0), seg("b", 2.0, 4.5)];
        let out = remap_words("xy", &words, &[1, 1]);
        assert_eq!((out[0].start, out[0].end), (1.5, 2.0));
        assert_eq!((out[1].start, out[1].end), (2.0, 4.5));
    }

    #[test]
    fn a_segment_that_gets_no_characters_is_dropped() {
        let words = vec![seg("a", 0.0, 1.0), seg("b", 1.0, 2.0)];
        let out = remap_words("x", &words, &[0, 1]);
        assert_eq!(out, vec![seg("x", 1.0, 2.0)]);
    }

    #[test]
    fn nothing_to_map_gives_nothing() {
        assert!(remap_words("abc", &[], &[]).is_empty());
        assert!(remap_words("", &[seg("a", 0.0, 1.0)], &[1]).is_empty());
    }

    #[test]
    fn all_zero_weights_fall_back_to_an_even_split() {
        let words = vec![seg("a", 0.0, 1.0), seg("b", 1.0, 2.0)];
        let out = remap_words("abcd", &words, &[0, 0]);
        assert_eq!(out.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), vec!["ab", "cd"]);
    }

    #[test]
    fn weights_come_from_each_segment_romanized_on_its_own() {
        let words = vec![seg("\u{3055}\u{3088}\u{306a}\u{3089} ", 0.0, 1.0), seg("\u{3060}\u{3051}", 1.0, 2.0)];
        // sayonara (8) vs dake (4), falling back to native length for text with no CJK.
        assert_eq!(segment_weights(&words, HanLanguage::Japanese), vec![8, 4]);
        let latin = vec![seg("hey ", 0.0, 1.0)];
        assert_eq!(segment_weights(&latin, HanLanguage::Japanese), vec![4]);
    }
}

#[cfg(test)]
mod lyric_line_tests {
    use super::*;
    use crate::lyrics::{LyricLine, WordSeg};
    use std::time::Duration;

    fn line(text: &str, words: Vec<WordSeg>) -> LyricLine {
        LyricLine { timestamp: Duration::from_secs(1), text: text.to_string(), words }
    }

    fn seg(text: &str, start: f64, end: f64) -> WordSeg {
        WordSeg { text: text.to_string(), start, end }
    }

    #[test]
    fn only_lines_with_cjk_get_a_romanization() {
        let lines = vec![line("\u{3055}\u{3088}\u{306a}\u{3089}", vec![]), line("Stay", vec![])];
        let out = romanize_lyric_lines(&lines);
        assert_eq!(out[0].as_ref().map(|r| r.text.as_str()), Some("sayonara"));
        assert!(out[1].is_none());
    }

    #[test]
    fn a_line_with_word_timing_gets_re_timed_words_that_re_form_it() {
        let words = vec![seg("\u{3055}\u{3088}\u{306a}\u{3089} ", 1.0, 2.0), seg("\u{3060}\u{3051}", 2.0, 3.0)];
        let lines = vec![line("\u{3055}\u{3088}\u{306a}\u{3089} \u{3060}\u{3051}", words)];
        let roman = romanize_lyric_lines(&lines).remove(0).unwrap();
        assert_eq!(roman.text, "sayonara dake");
        assert_eq!(roman.words.iter().map(|w| w.text.as_str()).collect::<String>(), roman.text);
        assert_eq!(roman.words.first().map(|w| w.start), Some(1.0));
        assert_eq!(roman.words.last().map(|w| w.end), Some(3.0));
    }

    #[test]
    fn a_line_without_word_timing_gets_none() {
        let lines = vec![line("\u{c548}\u{b155}", vec![])];
        let roman = romanize_lyric_lines(&lines).remove(0).unwrap();
        assert_eq!(roman.text, "annyeong");
        assert!(roman.words.is_empty());
    }

    #[test]
    fn the_languages_are_decided_across_the_whole_sheet() {
        // Kana on one line makes the kanji-only line Japanese.
        let lines = vec![line("\u{3055}\u{3088}\u{306a}\u{3089}", vec![]), line("\u{904b}\u{547d}", vec![])];
        let out = romanize_lyric_lines(&lines);
        assert_eq!(out[1].as_ref().map(|r| r.text.as_str()), Some("unmei"));
    }

    #[test]
    fn a_sheet_has_cjk_when_any_line_does() {
        assert!(sheet_has_cjk(&[line("Stay", vec![]), line("\u{4f60}\u{597d}", vec![])]));
        assert!(!sheet_has_cjk(&[line("Stay", vec![]), line("in", vec![])]));
        assert!(!sheet_has_cjk(&[]));
    }
}

#[cfg(test)]
mod plain_tests {
    use super::*;

    #[test]
    fn each_text_line_gets_its_own_entry_and_only_cjk_lines_are_romanized() {
        let out = romanize_plain_lines("\u{3055}\u{3088}\u{306a}\u{3089}\nStay in the middle\n\u{541b}\u{3068}");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].as_ref().map(|r| r.text.as_str()), Some("sayonara"));
        assert!(out[1].is_none());
        assert_eq!(out[2].as_ref().map(|r| r.text.as_str()), Some("kimi to"));
    }

    #[test]
    fn entries_line_up_with_str_lines_including_blank_lines() {
        // The renderer walks `text.lines()`, so the entries must too.
        let text = "\u{4f60}\u{597d}\n\n\u{4e16}\u{754c}\n";
        let out = romanize_plain_lines(text);
        assert_eq!(out.len(), text.lines().count());
        assert!(out[0].is_some());
        assert!(out[1].is_none(), "a blank line has nothing to romanize");
        assert!(out[2].is_some());
    }

    #[test]
    fn the_language_is_decided_across_the_whole_text() {
        // Kana on one line makes the kanji-only line Japanese.
        let out = romanize_plain_lines("\u{3055}\u{3088}\u{306a}\u{3089}\n\u{904b}\u{547d}");
        assert_eq!(out[1].as_ref().map(|r| r.text.as_str()), Some("unmei"));
        let out = romanize_plain_lines("\u{904b}\u{547d}");
        assert_eq!(out[0].as_ref().map(|r| r.text.as_str()), Some("y\u{00f9}n m\u{00ec}ng"));
    }

    #[test]
    fn plain_lines_carry_no_word_timing() {
        let out = romanize_plain_lines("\u{c548}\u{b155}");
        assert!(out[0].as_ref().unwrap().words.is_empty());
    }

    #[test]
    fn text_with_no_cjk_has_nothing_to_romanize() {
        assert!(romanize_plain_lines("one\ntwo").iter().all(Option::is_none));
        assert!(romanize_plain_lines("").is_empty());
    }
}
