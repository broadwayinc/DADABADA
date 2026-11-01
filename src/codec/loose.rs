//! Loose form of a string, for `"match": "loose"`. Built-in Unicode rules only, no dictionaries.
//! The steps are listed in ARCHITECTURE.md, "Loose match".

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, GeneralCategoryGroup, UnicodeGeneralCategory};
use unicode_script::{Script, UnicodeScript};

/// Bump when the rules change; `ddbd admin rebuild-loose` rewrites the keys.
pub const LOOSE_VERSION: u32 = 1;

const L_JAMO: [&str; 19] = [
    "ㄱ", "ㄲ", "ㄴ", "ㄷ", "ㄸ", "ㄹ", "ㅁ", "ㅂ", "ㅃ", "ㅅ", "ㅆ", "ㅇ", "ㅈ", "ㅉ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ",
];
const V_JAMO: [&str; 21] = [
    "ㅏ", "ㅐ", "ㅑ", "ㅒ", "ㅓ", "ㅔ", "ㅕ", "ㅖ", "ㅗ", "ㅗㅏ", "ㅗㅐ", "ㅗㅣ", "ㅛ", "ㅜ", "ㅜㅓ", "ㅜㅔ", "ㅜㅣ",
    "ㅠ", "ㅡ", "ㅡㅣ", "ㅣ",
];
const T_JAMO: [&str; 27] = [
    "ㄱ", "ㄲ", "ㄱㅅ", "ㄴ", "ㄴㅈ", "ㄴㅎ", "ㄷ", "ㄹ", "ㄹㄱ", "ㄹㅁ", "ㄹㅂ", "ㄹㅅ", "ㄹㅌ", "ㄹㅍ", "ㄹㅎ", "ㅁ",
    "ㅂ", "ㅂㅅ", "ㅅ", "ㅆ", "ㅇ", "ㅈ", "ㅊ", "ㅋ", "ㅌ", "ㅍ", "ㅎ",
];

pub struct Loose {
    /// The loose form.
    pub text: String,
    /// Korean initials form (each syllable's first consonant).
    pub initials: String,
    /// A Hangul syllable (consonant + vowel) was seen.
    pub has_syllable: bool,
    /// A Hangul consonant was seen.
    pub has_consonant: bool,
    /// A Hangul vowel was seen.
    pub has_vowel: bool,
}

impl Loose {
    /// For a query: only Korean consonants (no vowels) means "search the initials key".
    pub fn is_initials_query(&self) -> bool {
        self.has_consonant && !self.has_vowel
    }
}

fn strips_marks(s: Script) -> bool {
    matches!(
        s,
        Script::Latin | Script::Greek | Script::Cyrillic | Script::Armenian | Script::Hebrew | Script::Arabic | Script::Syriac
    )
}

fn invisible(c: char) -> bool {
    matches!(c,
        '\u{034F}' | '\u{115F}' | '\u{1160}' | '\u{3164}' | '\u{FFA0}' | '\u{00AD}'
        | '\u{180B}'..='\u{180F}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}'
        | '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}')
}

fn removable(c: char) -> bool {
    if c.is_whitespace() || c.is_control() || invisible(c) {
        return true;
    }
    if c.general_category_group() == GeneralCategoryGroup::Punctuation {
        return true;
    }
    c.general_category() == GeneralCategory::Format
}

fn letter_map(c: char) -> Option<&'static str> {
    Some(match c {
        'ø' => "o",
        'đ' => "d",
        'ł' => "l",
        'ħ' => "h",
        'ŧ' => "t",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        'ð' => "d",
        'ı' => "i",
        // Arabic, Persian, Urdu
        'ٱ' => "ا",
        'ى' | 'ی' => "ي",
        'ک' => "ك",
        'ة' => "ه",
        'ـ' => "",
        // Hebrew final letters
        'ך' => "כ",
        'ם' => "מ",
        'ן' => "נ",
        'ף' => "פ",
        'ץ' => "צ",
        // Japanese small kana (after katakana -> hiragana)
        'ぁ' => "あ",
        'ぃ' => "い",
        'ぅ' => "う",
        'ぇ' => "え",
        'ぉ' => "お",
        'っ' => "つ",
        'ゃ' => "や",
        'ゅ' => "ゆ",
        'ょ' => "よ",
        'ゎ' => "わ",
        'ゕ' => "か",
        'ゖ' => "け",
        _ => return None,
    })
}

/// Digit value of any Unicode decimal digit. Decimal digits come in runs of ten, so walk back
/// to the start of the run.
fn digit_value(c: char) -> Option<char> {
    if c.is_ascii_digit() {
        return Some(c);
    }
    if c.general_category() != GeneralCategory::DecimalNumber {
        return None;
    }
    let mut start = c as u32;
    while start > 0 {
        match char::from_u32(start - 1) {
            Some(p) if p.general_category() == GeneralCategory::DecimalNumber => start -= 1,
            _ => break,
        }
    }
    let v = (c as u32 - start) % 10;
    char::from_digit(v, 10)
}

pub fn loosen(s: &str) -> Loose {
    // 1. compatibility forms, 2. full case folding, then split into base + marks
    let nfkc: String = s.nfkc().collect();
    let folded = caseless::default_case_fold_str(&nfkc);
    let mut out = String::with_capacity(folded.len());
    let mut ini = String::new();
    let mut base = Script::Common;
    let mut after_initial = false;
    let mut has_syllable = false;
    let mut has_consonant = false;
    let mut has_vowel = false;

    for c in folded.nfd() {
        let gc = c.general_category();
        if matches!(gc, GeneralCategory::NonspacingMark | GeneralCategory::EnclosingMark | GeneralCategory::SpacingMark) {
            // 3. drop accents only where they are accents
            if invisible(c) || (gc != GeneralCategory::SpacingMark && strips_marks(base)) {
                continue;
            }
            out.push(c);
            ini.push(c);
            continue;
        }
        base = c.script();
        let cp = c as u32;
        // 7. Hangul as typed letters
        if (0x1100..=0x1112).contains(&cp) {
            let j = L_JAMO[(cp - 0x1100) as usize];
            out.push_str(j);
            ini.push_str(j);
            after_initial = true;
            has_consonant = true;
            continue;
        }
        if (0x1161..=0x1175).contains(&cp) {
            out.push_str(V_JAMO[(cp - 0x1161) as usize]);
            if after_initial {
                has_syllable = true;
            } else {
                ini.push_str(V_JAMO[(cp - 0x1161) as usize]);
            }
            has_vowel = true;
            continue;
        }
        if (0x11A8..=0x11C2).contains(&cp) {
            out.push_str(T_JAMO[(cp - 0x11A8) as usize]);
            if !after_initial {
                ini.push_str(T_JAMO[(cp - 0x11A8) as usize]);
                has_consonant = true;
            }
            continue;
        }
        after_initial = false;
        // 8. spaces, punctuation, invisible characters
        if removable(c) {
            continue;
        }
        // 6. digits of every script
        if let Some(d) = digit_value(c) {
            out.push(d);
            ini.push(d);
            continue;
        }
        // 5. katakana -> hiragana
        let c = if (0x30A1..=0x30F6).contains(&cp) { char::from_u32(cp - 0x60).unwrap_or(c) } else { c };
        // 4. letters without a decomposition, script letter forms
        match letter_map(c) {
            Some(m) => {
                out.push_str(m);
                ini.push_str(m);
            }
            None => {
                out.push(c);
                ini.push(c);
            }
        }
    }
    // 9. stable bytes
    Loose {
        text: out.nfc().collect(),
        initials: ini.nfc().collect(),
        has_syllable,
        has_consonant,
        has_vowel,
    }
}
