//! Korean (Hangul) -> Revised Romanization of Korean, with the pronunciation
//! rules that change how a syllable is written next to its neighbour
//! (liaison, nasalization, ㄹ assimilation, palatalization, ㅎ aspiration,
//! compound final consonants). Revised Romanization deliberately does not
//! transcribe tensification (학교 is `hakgyo`), which keeps the rule set small.
//! Written in-repo because the only crate for this is GPL-3.0.

const S_BASE: u32 = 0xAC00;
const S_LAST: u32 = 0xD7A3;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
/// Index of the vowel ㅣ, which triggers palatalization of a preceding ㄷ/ㅌ.
const VOWEL_I: usize = 20;

const ONSETS: [char; 19] = [
    '\u{3131}', '\u{3132}', '\u{3134}', '\u{3137}', '\u{3138}', '\u{3139}', '\u{3141}', '\u{3142}', '\u{3143}',
    '\u{3145}', '\u{3146}', '\u{3147}', '\u{3148}', '\u{3149}', '\u{314A}', '\u{314B}', '\u{314C}', '\u{314D}',
    '\u{314E}',
];

const VOWELS: [&str; 21] = [
    "a", "ae", "ya", "yae", "eo", "e", "yeo", "ye", "o", "wa", "wae", "oe", "yo", "u", "wo", "we", "wi", "yu", "eu",
    "ui", "i",
];

/// Final consonants by index (0 = none), as their jamo.
const CODAS: [&[char]; 28] = [
    &[],
    &['\u{3131}'],
    &['\u{3132}'],
    &['\u{3131}', '\u{3145}'],
    &['\u{3134}'],
    &['\u{3134}', '\u{3148}'],
    &['\u{3134}', '\u{314E}'],
    &['\u{3137}'],
    &['\u{3139}'],
    &['\u{3139}', '\u{3131}'],
    &['\u{3139}', '\u{3141}'],
    &['\u{3139}', '\u{3142}'],
    &['\u{3139}', '\u{3145}'],
    &['\u{3139}', '\u{314C}'],
    &['\u{3139}', '\u{314D}'],
    &['\u{3139}', '\u{314E}'],
    &['\u{3141}'],
    &['\u{3142}'],
    &['\u{3142}', '\u{3145}'],
    &['\u{3145}'],
    &['\u{3146}'],
    &['\u{3147}'],
    &['\u{3148}'],
    &['\u{314A}'],
    &['\u{314B}'],
    &['\u{314C}'],
    &['\u{314D}'],
    &['\u{314E}'],
];

const G: char = '\u{3131}';
const N: char = '\u{3134}';
const D: char = '\u{3137}';
const R: char = '\u{3139}';
const M: char = '\u{3141}';
const B: char = '\u{3142}';
const NG: char = '\u{3147}';
const J: char = '\u{3148}';
const H: char = '\u{314E}';

/// One syllable block, decomposed. `onset` is `NG` (ㅇ) when silent.
#[derive(Clone)]
struct Syl {
    onset: char,
    vowel: usize,
    coda: Vec<char>,
}

fn decompose(c: char) -> Option<Syl> {
    let code = c as u32;
    if !(S_BASE..=S_LAST).contains(&code) {
        return None;
    }
    let index = code - S_BASE;
    let onset = ONSETS[(index / (V_COUNT * T_COUNT)) as usize];
    let vowel = ((index % (V_COUNT * T_COUNT)) / T_COUNT) as usize;
    let coda = CODAS[(index % T_COUNT) as usize].to_vec();
    Some(Syl { onset, vowel, coda })
}

fn onset_text(c: char) -> &'static str {
    match c {
        '\u{3131}' => "g",
        '\u{3132}' => "kk",
        '\u{3134}' => "n",
        '\u{3137}' => "d",
        '\u{3138}' => "tt",
        '\u{3139}' => "r",
        '\u{3141}' => "m",
        '\u{3142}' => "b",
        '\u{3143}' => "pp",
        '\u{3145}' => "s",
        '\u{3146}' => "ss",
        '\u{3148}' => "j",
        '\u{3149}' => "jj",
        '\u{314A}' => "ch",
        '\u{314B}' => "k",
        '\u{314C}' => "t",
        '\u{314D}' => "p",
        '\u{314E}' => "h",
        _ => "",
    }
}

fn aspirated(c: char) -> char {
    match c {
        G => '\u{314B}',
        D => '\u{314C}',
        B => '\u{314D}',
        J => '\u{314A}',
        other => other,
    }
}

/// A double final reduced to the one consonant that is actually sounded
/// before a consonant or at the end of a word, then to its representative
/// sound (the seven a final can really be).
fn sounded_final(coda: &[char]) -> Option<char> {
    let first = *coda.first()?;
    let simplified = match coda {
        ['\u{3131}', '\u{3145}'] | ['\u{3139}', '\u{3131}'] => G,
        ['\u{3134}', _] => N,
        ['\u{3139}', '\u{3141}'] => M,
        ['\u{3139}', '\u{314D}'] | ['\u{3142}', '\u{3145}'] => B,
        ['\u{3139}', _] => R,
        _ => first,
    };
    Some(match simplified {
        '\u{3131}' | '\u{3132}' | '\u{314B}' => G,
        '\u{3137}' | '\u{3145}' | '\u{3146}' | '\u{3148}' | '\u{314A}' | '\u{314C}' | '\u{314E}' => D,
        '\u{3142}' | '\u{314D}' => B,
        other => other,
    })
}

fn coda_text(c: char) -> &'static str {
    match c {
        G => "k",
        N => "n",
        D => "t",
        R => "l",
        M => "m",
        B => "p",
        NG => "ng",
        _ => "",
    }
}

/// How the end of `cur` and the start of `next` change each other.
fn join(cur: &mut Syl, next: &mut Syl) {
    if cur.coda.is_empty() {
        return;
    }
    if next.onset == NG {
        // A silent onset: the final consonant moves onto it (liaison), a final
        // ㅎ is silent, and ㅇ (-ng) stays put.
        if cur.coda.last() == Some(&H) {
            cur.coda.pop();
        }
        if cur.coda.is_empty() || cur.coda.last() == Some(&NG) {
            return;
        }
        let moved = cur.coda.pop().unwrap_or(NG);
        next.onset = match moved {
            // ㄷ/ㅌ before the vowel ㅣ is palatalized (같이 -> gachi).
            D if next.vowel == VOWEL_I && cur.coda.is_empty() => J,
            '\u{314C}' if next.vowel == VOWEL_I && cur.coda.is_empty() => '\u{314A}',
            other => other,
        };
        return;
    }

    // A consonant follows. ㅎ next to ㄱ/ㄷ/ㅈ aspirates it; a stop before ㅎ
    // aspirates ㅎ instead.
    if cur.coda.last() == Some(&H) && matches!(next.onset, G | D | J) {
        cur.coda.pop();
        next.onset = aspirated(next.onset);
    } else if next.onset == H && cur.coda.len() == 1 && matches!(cur.coda[0], G | D | B | J) {
        next.onset = aspirated(cur.coda[0]);
        cur.coda.clear();
    }
    let Some(mut sound) = sounded_final(&cur.coda) else {
        return;
    };
    // ㄹ after -m, -ng, -k, -p is pronounced ㄴ.
    if next.onset == R && matches!(sound, M | NG | G | B) {
        next.onset = N;
    }
    // Nasalization of a stop before ㄴ/ㅁ.
    if matches!(next.onset, N | M) {
        sound = match sound {
            G => NG,
            D => N,
            B => M,
            other => other,
        };
    }
    // ㄴ/ㄹ next to ㄹ/ㄴ both become ㄹ.
    if sound == N && next.onset == R {
        sound = R;
    } else if sound == R && next.onset == N {
        next.onset = R;
    }
    cur.coda = vec![sound];
}

fn render(syls: &[Syl], out: &mut String) {
    let mut previous_final_l = false;
    for syl in syls {
        let onset = if syl.onset == R && previous_final_l { "l" } else { onset_text(syl.onset) };
        out.push_str(onset);
        out.push_str(VOWELS[syl.vowel]);
        let sound = sounded_final(&syl.coda);
        if let Some(sound) = sound {
            out.push_str(coda_text(sound));
        }
        previous_final_l = sound == Some(R);
    }
}

/// A lone jamo (`ㅋㅋㅋ`, `ㅠㅠ`) sounded out.
fn jamo_text(c: char) -> Option<&'static str> {
    let code = c as u32;
    if (0x3131..=0x314E).contains(&code) {
        return Some(match c {
            NG => "ng",
            other => onset_text(other),
        });
    }
    let vowel = match c {
        '\u{314F}' => "a",
        '\u{3150}' => "ae",
        '\u{3151}' => "ya",
        '\u{3152}' => "yae",
        '\u{3153}' => "eo",
        '\u{3154}' => "e",
        '\u{3155}' => "yeo",
        '\u{3156}' => "ye",
        '\u{3157}' => "o",
        '\u{315B}' => "yo",
        '\u{315C}' => "u",
        '\u{3160}' => "yu",
        '\u{3161}' => "eu",
        '\u{3163}' => "i",
        _ => return None,
    };
    Some(vowel)
}

/// Romanizes the Hangul in `text` and leaves everything else exactly as it
/// was. Pronunciation rules apply between neighbouring syllables of one
/// word (an unbroken run of Hangul); a space or any other character ends it.
pub fn romanize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<Syl> = Vec::new();
    let flush = |run: &mut Vec<Syl>, out: &mut String| {
        if run.is_empty() {
            return;
        }
        for i in 0..run.len() - 1 {
            let (left, right) = run.split_at_mut(i + 1);
            join(&mut left[i], &mut right[0]);
        }
        render(run, out);
        run.clear();
    };
    for c in text.chars() {
        if let Some(syl) = decompose(c) {
            run.push(syl);
            continue;
        }
        flush(&mut run, &mut out);
        match jamo_text(c) {
            Some(sound) => out.push_str(sound),
            None => out.push(c),
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(text: &str) -> String {
        romanize(text)
    }

    #[test]
    fn plain_syllables() {
        assert_eq!(rr("안녕하세요"), "annyeonghaseyo");
        assert_eq!(rr("사랑해"), "saranghae");
        assert_eq!(rr("오늘"), "oneul");
        assert_eq!(rr("이름"), "ireum");
        assert_eq!(rr("꿈"), "kkum");
        assert_eq!(rr("밥"), "bap");
        assert_eq!(rr("뭐"), "mwo");
    }

    #[test]
    fn a_word_final_consonant_takes_its_representative_sound() {
        assert_eq!(rr("한국"), "hanguk");
        assert_eq!(rr("빛"), "bit");
        assert_eq!(rr("값"), "gap");
        assert_eq!(rr("좋"), "jot");
    }

    #[test]
    fn a_final_consonant_moves_onto_a_following_vowel() {
        assert_eq!(rr("먹어"), "meogeo");
        assert_eq!(rr("믿어"), "mideo");
        assert_eq!(rr("옷이"), "osi");
        assert_eq!(rr("꽃이"), "kkochi");
        assert_eq!(rr("낮에"), "naje");
        assert_eq!(rr("부엌에"), "bueoke");
    }

    #[test]
    fn a_double_final_splits_and_the_second_consonant_moves() {
        assert_eq!(rr("읽어"), "ilgeo");
        assert_eq!(rr("값이"), "gapsi");
        assert_eq!(rr("앉아"), "anja");
    }

    #[test]
    fn a_double_final_ss_becomes_ss_before_a_vowel() {
        assert_eq!(rr("있어"), "isseo");
    }

    #[test]
    fn a_final_h_is_silent_before_a_vowel() {
        assert_eq!(rr("좋아"), "joa");
        assert_eq!(rr("많아"), "mana");
        assert_eq!(rr("싫어"), "sireo");
    }

    #[test]
    fn nasalization_of_k_t_p_before_n_or_m() {
        assert_eq!(rr("국물"), "gungmul");
        assert_eq!(rr("합니다"), "hamnida");
        assert_eq!(rr("감사합니다"), "gamsahamnida");
    }

    #[test]
    fn l_assimilation() {
        assert_eq!(rr("신라"), "silla");
        assert_eq!(rr("종로"), "jongno");
        assert_eq!(rr("설날"), "seollal");
        assert_eq!(rr("협력"), "hyeomnyeok");
    }

    #[test]
    fn t_and_th_before_i_are_palatalized() {
        assert_eq!(rr("같이"), "gachi");
        assert_eq!(rr("굳이"), "guji");
    }

    #[test]
    fn h_aspirates_a_neighbouring_stop() {
        assert_eq!(rr("놓고"), "noko");
        assert_eq!(rr("좋다"), "jota");
        assert_eq!(rr("축하"), "chuka");
        assert_eq!(rr("잡혀"), "japyeo");
    }

    #[test]
    fn revised_romanization_does_not_transcribe_tensification() {
        assert_eq!(rr("학교"), "hakgyo");
    }

    #[test]
    fn a_final_h_plus_double_final_before_a_vowel() {
        assert_eq!(rr("있잖아"), "itjana");
    }

    #[test]
    fn the_diphthong_ui_is_always_ui() {
        assert_eq!(rr("나의"), "naui");
        assert_eq!(rr("의사"), "uisa");
    }

    #[test]
    fn rules_apply_inside_a_word_not_across_a_space() {
        assert_eq!(rr("너를 사랑해"), "neoreul saranghae");
        assert_eq!(rr("집 앞"), "jip ap");
    }

    #[test]
    fn non_hangul_text_passes_through_untouched() {
        assert_eq!(rr("Stay 너 with me"), "Stay neo with me");
        assert_eq!(rr("Hello, world! 123"), "Hello, world! 123");
        assert_eq!(rr(""), "");
    }

    #[test]
    fn standalone_jamo_are_sounded_out() {
        assert_eq!(rr("ㅋㅋㅋ"), "kkk");
        assert_eq!(rr("ㅎㅎ"), "hh");
    }

    #[test]
    fn a_lyric_line_reads_naturally() {
        assert_eq!(rr("내 마음속에 있어"), "nae maeumsoge isseo");
    }
}
