//! Japanese -> Hepburn-style romaji. Kanji need real readings, which depend
//! on the surrounding words, so this runs `lindera` (IPADIC, embedded) to
//! split the line into words with dictionary readings, then turns the
//! katakana readings into romaji. Long vowels are written with macrons
//! (Hepburn: `kyō`, `tōkyō`, `kōhī`), the particles は/へ are read `wa`/`e`,
//! and verb endings and te/ba particles are joined onto their verb
//! (`wakatta`, `itatte`).

use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use std::borrow::Cow;
use std::sync::OnceLock;
use unicode_normalization::UnicodeNormalization;
use wana_kana::ConvertJapanese;

/// The dictionary is ~47 MB embedded in the binary and takes a moment to
/// load, so it is built once, on first use, off the render thread.
fn segmenter() -> Option<&'static Segmenter> {
    static SEGMENTER: OnceLock<Option<Segmenter>> = OnceLock::new();
    SEGMENTER
        .get_or_init(|| {
            let dictionary = load_dictionary("embedded://ipadic").ok()?;
            Some(Segmenter::new(Mode::Normal, dictionary, None))
        })
        .as_ref()
}

/// One dictionary word (or unknown span), with what the joining rules need.
struct Word {
    /// Katakana reading, or the surface itself when there is none.
    text: String,
    pos1: String,
    pos2: String,
    surface: String,
}

const IPADIC_POS1: usize = 0;
const IPADIC_POS2: usize = 1;
const IPADIC_CONJUGATION_FORM: usize = 5;
const IPADIC_READING: usize = 7;
const IPADIC_PRONUNCIATION: usize = 8;

fn field(details: &[&str], index: usize) -> Option<String> {
    details.get(index).map(|d| d.to_string()).filter(|d| d != "*")
}

fn is_verbal(pos1: &str) -> bool {
    matches!(pos1, "\u{52d5}\u{8a5e}" | "\u{5f62}\u{5bb9}\u{8a5e}" | "\u{52a9}\u{52d5}\u{8a5e}")
}

/// Whether `word` continues the word before it (no space): verb endings and
/// the te/ba particles onto their verb, suffixes onto their noun.
fn attaches_to_previous(previous: &Word, word: &Word) -> bool {
    let auxiliary = word.pos1 == "\u{52a9}\u{52d5}\u{8a5e}";
    // The te/ba forms, plus the colloquial contractions of te-iru / te-shimau
    // (愛してる, 言っちゃう), which IPADIC tags as independent verbs.
    let te_form = matches!(
        word.surface.as_str(),
        "\u{3066}"
            | "\u{3067}"
            | "\u{3070}"
            | "\u{3063}\u{3066}"
            | "\u{3061}\u{3083}"
            | "\u{3066}\u{308b}"
            | "\u{3067}\u{308b}"
            | "\u{3061}\u{3083}\u{3046}"
            | "\u{3058}\u{3083}\u{3046}"
    );
    let suffix = word.pos2 == "\u{63a5}\u{5c3e}";
    ((auxiliary || te_form) && is_verbal(&previous.pos1)) || suffix
}

fn ascii_punctuation(c: char) -> Option<char> {
    Some(match c {
        '\u{3001}' => ',',
        '\u{3002}' => '.',
        '\u{300C}' | '\u{300D}' | '\u{300E}' | '\u{300F}' => '"',
        _ => return None,
    })
}

fn is_punctuation(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| ascii_punctuation(c).is_some() || c.is_ascii_punctuation())
}

fn macron(vowel: char) -> char {
    match vowel {
        'a' => '\u{101}',
        'i' => '\u{12b}',
        'u' => '\u{16b}',
        'e' => '\u{113}',
        'o' => '\u{14d}',
        other => other,
    }
}

/// Katakana -> romaji, with each `ー` written as a macron on the vowel before
/// it. (`wana_kana` doubles the vowel instead, so the stretch before each `ー`
/// is romanized on its own and its last vowel gets the macron.)
fn kana_to_romaji(kana: &str) -> String {
    let mut out = String::with_capacity(kana.len() * 2);
    let mut stretch = String::new();
    for c in kana.chars() {
        if c != '\u{30fc}' {
            stretch.push(c);
            continue;
        }
        let mut romaji = std::mem::take(&mut stretch).to_romaji();
        if let Some(vowel) = romaji.pop() {
            out.push_str(&romaji);
            out.push(macron(vowel));
        }
    }
    out.push_str(&stretch.to_romaji());
    out
}

/// The vowel a katakana ends in (its row in the kana table), for the kana
/// that can be followed by a lengthening vowel.
fn vowel_row(c: char) -> Option<char> {
    Some(match c {
        'ア' | 'カ' | 'ガ' | 'サ' | 'ザ' | 'タ' | 'ダ' | 'ナ' | 'ハ' | 'バ' | 'パ' | 'マ' | 'ヤ' | 'ラ' | 'ワ' | 'ャ'
        | 'ァ' => 'a',
        'ウ' | 'ク' | 'グ' | 'ス' | 'ズ' | 'ツ' | 'ヅ' | 'ヌ' | 'フ' | 'ブ' | 'プ' | 'ム' | 'ユ' | 'ル' | 'ュ' | 'ゥ'
        | 'ヴ' => 'u',
        'エ' | 'ケ' | 'ゲ' | 'セ' | 'ゼ' | 'テ' | 'デ' | 'ネ' | 'ヘ' | 'ベ' | 'ペ' | 'メ' | 'レ' | 'ェ' => 'e',
        'オ' | 'コ' | 'ゴ' | 'ソ' | 'ゾ' | 'ト' | 'ド' | 'ノ' | 'ホ' | 'ボ' | 'ポ' | 'モ' | 'ヨ' | 'ロ' | 'ヲ' | 'ョ'
        | 'ォ' => 'o',
        _ => return None,
    })
}

/// Rewrites the long vowels in one word's katakana reading as the prolonged
/// sound mark, which then romanizes to a single macron: `キョウ` -> `キョー`
/// -> `kyō`, `オオキイ` -> `ōkii`. Only a-a, u-u, e-e, o-o and o-u lengthen;
/// `ii` and `ei` stay as they are (Hepburn writes `sensei`, `kii`). A verb in
/// its dictionary form keeps a final う (思う is omo-u, `omou`, not `omō`).
fn mark_long_vowels(reading: &str, verb_dictionary_form: bool) -> String {
    let chars: Vec<char> = reading.chars().collect();
    let mut out = String::with_capacity(reading.len());
    for (i, &c) in chars.iter().enumerate() {
        let previous = i.checked_sub(1).and_then(|j| vowel_row(chars[j]));
        let lengthens = matches!(
            (previous, c),
            (Some('a'), 'ア') | (Some('u'), 'ウ') | (Some('e'), 'エ') | (Some('o'), 'オ') | (Some('o'), 'ウ')
        );
        let verb_ending = verb_dictionary_form && c == 'ウ' && i + 1 == chars.len();
        out.push(if lengthens && !verb_ending { 'ー' } else { c });
    }
    out
}

/// Romanizes `text`. Anything the dictionary can't read (Latin words, digits)
/// is kept as written, and so is the source's own spacing.
pub fn romanize(text: &str) -> String {
    let normalized: String = text.nfkc().collect();
    let Some(segmenter) = segmenter() else {
        return normalized;
    };
    let Ok(tokens) = segmenter.segment(Cow::Borrowed(normalized.as_str())) else {
        return normalized;
    };

    let mut out = String::with_capacity(normalized.len() * 2);
    let mut current: Option<Word> = None;
    let mut group = String::new();
    let mut last_end = 0usize;

    fn flush(group: &mut String, out: &mut String) {
        if group.is_empty() {
            return;
        }
        out.push_str(&kana_to_romaji(group));
        group.clear();
    }

    for token in &tokens {
        // The source's whitespace between tokens: keep it, and it ends a word.
        if token.byte_start > last_end {
            flush(&mut group, &mut out);
            out.push_str(&normalized[last_end..token.byte_start]);
            current = None;
        }
        last_end = token.byte_end;

        let surface = token.surface.to_string();
        let details = if token.word_id.is_system() && token.word_id.id() != u32::MAX {
            token.dictionary.word_details(token.word_id.id() as usize)
        } else {
            Vec::new()
        };
        let pos1 = field(&details, IPADIC_POS1).unwrap_or_default();
        let pos2 = field(&details, IPADIC_POS2).unwrap_or_default();
        // The particles は and へ are spoken wa and e; the pronunciation field
        // knows, the reading field doesn't.
        let particle = pos1 == "\u{52a9}\u{8a5e}" && matches!(surface.as_str(), "\u{306f}" | "\u{3078}");
        let reading = if particle { field(&details, IPADIC_PRONUNCIATION) } else { field(&details, IPADIC_READING) };
        let verb_dictionary_form = pos1 == "\u{52d5}\u{8a5e}"
            && field(&details, IPADIC_CONJUGATION_FORM).as_deref() == Some("\u{57fa}\u{672c}\u{5f62}");
        let text = mark_long_vowels(&reading.unwrap_or_else(|| surface.clone()), verb_dictionary_form);
        let word = Word { text, pos1, pos2, surface };

        if is_punctuation(&word.surface) {
            flush(&mut group, &mut out);
            out.extend(word.surface.chars().map(|c| ascii_punctuation(c).unwrap_or(c)));
            current = None;
            continue;
        }

        let joins = current.as_ref().is_some_and(|previous| attaches_to_previous(previous, &word));
        if !joins {
            flush(&mut group, &mut out);
            if out.chars().last().is_some_and(|last| !last.is_whitespace() && !matches!(last, '(' | '"')) {
                out.push(' ');
            }
        }
        group.push_str(&word.text);
        current = Some(word);
    }
    flush(&mut group, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiragana_only_lines() {
        assert_eq!(romanize("さよなら だけ だった"), "sayonara dake datta");
        assert_eq!(romanize("ずっとそばにいたって"), "zutto soba ni itatte");
    }

    #[test]
    fn kanji_get_their_contextual_readings() {
        assert_eq!(romanize("夜に駆ける"), "yoru ni kakeru");
        assert_eq!(romanize("君の名前を呼んだ"), "kimi no namae wo yonda");
        assert_eq!(romanize("あの日見た夢を"), "ano hi mita yume wo");
    }

    #[test]
    fn verb_endings_join_the_verb() {
        assert_eq!(romanize("その一言で全てが分かった"), "sono hitokoto de subete ga wakatta");
        assert_eq!(romanize("いざ始まればひとり芝居だ"), "iza hajimareba hitori shibai da");
    }

    #[test]
    fn the_topic_particle_is_wa_and_long_vowels_get_macrons() {
        assert_eq!(romanize("僕は今日も生きている"), "boku wa kyō mo ikite iru");
        assert_eq!(romanize("海のような君の笑顔"), "umi no yō na kimi no egao");
    }

    #[test]
    fn kangxi_radicals_are_normalized_before_lookup() {
        // Apple Music writes 一 and 言 as the radicals U+2F00 and U+2F94.
        assert_eq!(romanize("\u{2F00}\u{2F94}"), "hitokoto");
    }

    #[test]
    fn punctuation_attaches_to_the_word_before_it() {
        assert_eq!(romanize("大丈夫、愛してる"), "daijōbu, aishiteru");
        assert_eq!(romanize("君は？"), "kimi wa?");
    }

    #[test]
    fn a_long_o_written_ou_or_oo_becomes_o_with_a_macron() {
        assert_eq!(romanize("今日"), "kyō");
        assert_eq!(romanize("東京"), "tōkyō");
        assert_eq!(romanize("大きい"), "ōkii");
        assert_eq!(romanize("遠い"), "tōi");
        assert_eq!(romanize("ありがとう"), "arigatō");
    }

    #[test]
    fn a_long_a_u_or_e_gets_a_macron_but_ii_and_ei_do_not() {
        assert_eq!(romanize("ああ"), "ā");
        assert_eq!(romanize("ええ"), "ē");
        assert_eq!(romanize("先生"), "sensei");
        assert_eq!(romanize("大きい"), "ōkii");
    }

    #[test]
    fn a_verb_ending_in_u_after_o_is_not_a_long_vowel() {
        // 思う is omo-u, not a long o; Hepburn keeps `ou` here.
        assert_eq!(romanize("思う"), "omou");
        assert_eq!(romanize("食う"), "kuu");
    }

    #[test]
    fn colloquial_contractions_join_their_verb_too() {
        assert_eq!(romanize("愛してる"), "aishiteru");
        assert_eq!(romanize("知ってる"), "shitteru");
    }

    #[test]
    fn katakana_prolonged_sound_marks_become_macrons() {
        assert_eq!(romanize("コーヒーを飲む"), "kōhī wo nomu");
    }

    #[test]
    fn latin_text_and_spacing_inside_a_line_are_kept() {
        assert_eq!(romanize("Stay in the middle 君と"), "Stay in the middle kimi to");
    }

    #[test]
    fn text_with_no_japanese_is_left_alone() {
        assert_eq!(romanize("Hello, world"), "Hello, world");
        assert_eq!(romanize(""), "");
    }
}
