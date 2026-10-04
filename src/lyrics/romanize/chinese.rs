//! Chinese (Han) -> pinyin with tone marks, via the `pinyin` crate. It picks
//! a character's most common reading, so a polyphone in a rare sense
//! (重 in 重新 is `chóng`, not `zhòng`) comes out wrong; that is a known and
//! cosmetic limit, not worth a segmenter here.

use pinyin::ToPinyin;

/// Full-width punctuation as it is written in Latin text.
fn ascii_punctuation(c: char) -> Option<char> {
    Some(match c {
        '\u{FF0C}' | '\u{3001}' => ',',
        '\u{3002}' => '.',
        '\u{FF1F}' => '?',
        '\u{FF01}' => '!',
        '\u{FF1A}' => ':',
        '\u{FF1B}' => ';',
        '\u{FF08}' => '(',
        '\u{FF09}' => ')',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2018}' | '\u{2019}' => '\'',
        _ => return None,
    })
}

/// Han characters -> pinyin syllables (tone marks), separated by spaces;
/// everything else is kept, with full-width punctuation made ASCII and
/// attached to the word before it.
pub fn romanize(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut last_was_syllable = false;
    for c in text.chars() {
        if let Some(syllable) = c.to_pinyin() {
            let needs_space = out
                .chars()
                .last()
                .is_some_and(|last| !last.is_whitespace() && !matches!(last, '(' | '"'));
            if needs_space {
                out.push(' ');
            }
            out.push_str(syllable.with_tone());
            last_was_syllable = true;
            continue;
        }
        let c = ascii_punctuation(c).unwrap_or(c);
        if last_was_syllable && c.is_alphanumeric() {
            out.push(' ');
        }
        out.push(c);
        last_was_syllable = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_becomes_spaced_syllables_with_tone_marks() {
        assert_eq!(romanize("月亮代表我的心"), "yuè liàng dài biǎo wǒ de xīn");
    }

    #[test]
    fn full_width_punctuation_becomes_ascii_attached_to_the_word() {
        assert_eq!(
            romanize("你问我爱你有多深，我爱你有几分"),
            "nǐ wèn wǒ ài nǐ yǒu duō shēn, wǒ ài nǐ yǒu jǐ fēn"
        );
        assert_eq!(romanize("真的吗？好！"), "zhēn de ma? hǎo!");
    }

    #[test]
    fn non_chinese_text_is_kept_and_separated_from_syllables() {
        assert_eq!(romanize("I love 你"), "I love nǐ");
        assert_eq!(romanize("你 love me"), "nǐ love me");
        assert_eq!(romanize("OK，好"), "OK, hǎo");
    }

    #[test]
    fn existing_spacing_is_preserved_not_doubled() {
        assert_eq!(romanize("我 爱 你"), "wǒ ài nǐ");
    }

    #[test]
    fn text_without_han_is_returned_unchanged() {
        assert_eq!(romanize("Hello, world 123"), "Hello, world 123");
        assert_eq!(romanize(""), "");
    }
}
