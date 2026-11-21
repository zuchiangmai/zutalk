//! What differs between languages, as data: one language is one row of `ALL`. Nothing else
//! in the app knows a language by name; it asks the row.

use icu_normalizer::ComposingNormalizerBorrowed as Normalizer;
use icu_segmenter::{GraphemeClusterSegmenter as Graphemes, WordSegmenter as Words};
use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    Latin,
    Han,
    Kana,
    Hangul,
    Thai,
}

/// What captions reveal, wrap and count by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Word, // set apart by spaces
    Char,
    Icu, // the words of a dictionary, where a script runs them together
}

/// One step of setting the service's text the way people write it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Norm {
    Nfc,
    SaraAm, // the service spells Thai SARA AM as two letters; every keyboard types one
    /// Drops the service's spaces beside punctuation that brings its own. The ranges are
    /// listed: a character class would take Thai vowel signs and tone marks along.
    Punct(&'static [RangeInclusive<u32>]),
}

#[derive(Debug, Clone, Copy)]
pub struct Profile {
    pub lang: &'static str,
    pub scripts: &'static [Script], // their letters name it, if no other spoken language's do
    pub unit: Unit,
    pub spaced: bool,        // words are set apart by spaces
    pub reveal_per_s: f32,   // units a caption reveals each second
    pub cue_max: u16,        // the most units in one subtitle cue
    pub stops: &'static str, // marks that end a sentence
    pub norm: &'static [Norm],
    pub prompt: &'static str, // said to the translator when translating into it
    pub forbid: &'static [&'static str], // words a translation into it must not contain
}

const WIDE: &[RangeInclusive<u32>] =
    &[0x3000..=0x303F, 0xFF01..=0xFF20, 0xFF3B..=0xFF40, 0xFF5B..=0xFF65];

#[rustfmt::skip]
pub const LATIN: Profile = Profile { lang: "en", scripts: &[Script::Latin], unit: Unit::Word,
    spaced: true, reveal_per_s: 4.0, cue_max: 14, stops: ".?!", norm: &[Norm::Nfc], prompt: "",
    forbid: &[] };
#[rustfmt::skip]
pub const HAN: Profile = Profile { lang: "zh", scripts: &[Script::Han], unit: Unit::Char,
    spaced: false, reveal_per_s: 7.0, cue_max: 22, stops: "。？！",
    norm: &[Norm::Nfc, Norm::Punct(WIDE)], ..LATIN };

/// Every language the app knows. A new one is one row: Vietnamese would be
/// `Profile { lang: "vi", prompt: "Use tôi/bạn by default.", ..LATIN }`.
#[rustfmt::skip]
pub static ALL: &[Profile] = &[
    LATIN,
    Profile { lang: "fr", ..LATIN },
    Profile { lang: "es", ..LATIN },
    Profile { lang: "de", cue_max: 12, ..LATIN },
    HAN,
    Profile { lang: "ja", scripts: &[Script::Kana, Script::Han], prompt: "Use です/ます form.", ..HAN },
    Profile { lang: "ko", scripts: &[Script::Hangul], reveal_per_s: 3.5, cue_max: 10,
        prompt: "Use 합쇼체.", ..LATIN },
    Profile { lang: "th", scripts: &[Script::Thai], unit: Unit::Icu, spaced: false,
        reveal_per_s: 5.0, cue_max: 10, stops: "", norm: &[Norm::Nfc, Norm::SaraAm],
        prompt: "Polite neutral register. The speaker's gender is unknown: \
                 never use ผม, ฉัน, ดิฉัน, ครับ, ค่ะ, คะ.",
        forbid: &["ครับ", "ค่ะ", "ผม", "ดิฉัน"] },
];

/// The row for a language tag. A language without one is set like English.
pub fn profile(lang: &str) -> &'static Profile {
    let primary = lang.split(['-', '_']).next().unwrap_or(lang);
    ALL.iter().find(|p| p.lang.eq_ignore_ascii_case(primary)).unwrap_or(&LATIN)
}

fn script_of(c: char) -> Option<Script> {
    Some(match c as u32 {
        0x41..=0x5A | 0x61..=0x7A | 0xC0..=0x24F | 0x1E00..=0x1EFF => Script::Latin,
        0x0E00..=0x0E7F => Script::Thai,
        0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Script::Hangul,
        0x3040..=0x30FF | 0x31F0..=0x31FF => Script::Kana,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::Han,
        _ => return None,
    })
}

/// The one script every letter of `text` is in. Digits and punctuation are in none.
pub fn script(text: impl IntoIterator<Item = char>) -> Option<Script> {
    let mut found = text.into_iter().filter(|c| c.is_alphabetic()).filter_map(script_of);
    found.next().filter(|first| found.all(|s| s == *first))
}

/// The one language among `spoken` that writes in the script of `text`. Latin letters name
/// none: they are an English word inside Thai or Chinese speech as often as they are English.
pub fn written_in(text: &str, spoken: &[String]) -> Option<&'static str> {
    let script = script(text.chars()).filter(|s| *s != Script::Latin)?;
    let mut writers = spoken.iter().map(|l| profile(l)).filter(|p| p.scripts.contains(&script));
    writers.next().filter(|_| writers.next().is_none()).map(|p| p.lang)
}

/// NIKHAHIT then SARA AA, a tone mark sometimes between them, written as SARA AM.
fn sara_am(mut s: String) -> String {
    for tone in ["", "\u{e48}", "\u{e49}", "\u{e4a}", "\u{e4b}"] {
        s = s.replace(&format!("\u{e4d}{tone}\u{e32}"), &format!("{tone}\u{e33}"));
    }
    s
}

impl Profile {
    /// The service's text set the way people write it.
    pub fn normalize(&self, s: &str) -> String {
        self.set(s, false)
    }

    /// A translation set for its reader. Translated from a language that spaces its words,
    /// a space between two characters is a seam between the service's chunks, not a pause.
    pub fn polish(&self, s: &str, from: &Profile) -> String {
        self.set(s, from.spaced)
    }

    fn set(&self, s: &str, seams: bool) -> String {
        self.norm.iter().fold(s.to_string(), |s, step| match step {
            Norm::Nfc => Normalizer::new_nfc().normalize(&s).into_owned(),
            Norm::SaraAm => sara_am(s),
            Norm::Punct(wide) => {
                let wide = |c: char| wide.iter().any(|r| r.contains(&(c as u32)));
                let cjk = |c: char| matches!(script_of(c), Some(Script::Han | Script::Kana));
                let (mut out, mut gap) = (String::new(), String::new());
                for c in s.chars() {
                    if matches!(c, ' ' | '\t' | '\u{a0}' | '\u{202f}') {
                        gap.push(c);
                        continue;
                    }
                    // A space before the first character, or beside punctuation that brings
                    // its own, is the service's separator. Between two characters it is a
                    // pause the service heard and did not punctuate, and it stays.
                    let noise = match out.chars().next_back() {
                        None => wide(c) || cjk(c),
                        Some(p) => wide(p) || wide(c) || seams && cjk(p) && cjk(c),
                    };
                    if !noise {
                        out.push_str(&gap);
                    }
                    gap.clear();
                    out.push(c);
                }
                out + &gap
            }
        })
    }

    /// What a search compares: case folded, full and half width alike, and SARA AM spelled
    /// one way however it was typed or stored.
    pub fn key(&self, s: &str) -> String {
        Normalizer::new_nfkc().normalize(&sara_am(self.normalize(s))).to_lowercase()
    }

    /// Where each display unit ends, in UTF-16 code units: what a web view slices by. The
    /// white space before a unit travels with it.
    pub fn units(&self, s: &str) -> Vec<u16> {
        let u: Vec<u16> = s.encode_utf16().collect();
        let space = |i: usize| char::from_u32(u[i] as u32).is_some_and(char::is_whitespace);
        let ends: Vec<usize> = match self.unit {
            Unit::Word => (1..=u.len()).filter(|&i| i == u.len() || space(i)).collect(),
            Unit::Char => Graphemes::new().segment_utf16(&u).collect(),
            Unit::Icu => Words::new_dictionary(Default::default()).segment_utf16(&u).collect(),
        };
        let shown = ends.into_iter().filter(|&i| i > 0 && !space(i - 1));
        shown.map_while(|i| u16::try_from(i).ok()).collect()
    }
}
