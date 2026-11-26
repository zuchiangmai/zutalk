use zutalk_core::live::lang::{ALL, LATIN, Profile, Script, Unit, profile, script, written_in};

fn spoken(languages: &[&str]) -> Vec<String> {
    languages.iter().map(|l| l.to_string()).collect()
}

#[test]
fn every_language_has_a_row_and_an_unknown_one_is_set_like_english() {
    for lang in ["en", "fr", "es", "de", "zh", "ja", "ko", "th"] {
        assert_eq!(profile(lang).lang, lang);
        assert_eq!(ALL.iter().filter(|p| p.lang == lang).count(), 1, "{lang}");
    }
    assert_eq!(profile("zh-Hans").lang, "zh");
    assert_eq!(profile("TH").lang, "th");
    for unknown in ["vi", "und", "", "x-private"] {
        assert_eq!(profile(unknown).lang, "en", "{unknown}");
    }
}

/// The table is the only place a language is named: a new one is one expression.
#[test]
fn adding_vietnamese_is_one_row() {
    let vi = Profile {
        lang: "vi",
        prompt: "Use tôi/bạn unless the source fixes a kinship term.",
        ..LATIN
    };
    // Tone marks typed after their vowel compose into the letters people search for.
    assert_eq!(vi.normalize("Tie\u{302}\u{301}ng Vie\u{323}\u{302}t"), "Tiếng Việt");
    assert_eq!(vi.units("Xin chào các bạn"), [3, 8, 12, 16]);
    assert_eq!(vi.key("TIẾNG VIỆT"), "tiếng việt");
    // Until the row exists, Vietnamese is already set by the base it would start from.
    assert_eq!(profile("vi").normalize("Tie\u{302}\u{301}ng"), "Tiếng");
}

#[test]
fn rows_differ_only_where_the_languages_do() {
    let (zh, ja, ko, th, de) =
        (profile("zh"), profile("ja"), profile("ko"), profile("th"), profile("de"));
    assert_eq!((zh.unit, zh.spaced), (Unit::Char, false));
    assert_eq!((ja.unit, ja.scripts), (Unit::Char, &[Script::Kana, Script::Han][..]));
    assert_eq!((ko.unit, ko.spaced, ko.stops), (Unit::Word, true, ".?!"));
    assert_eq!((th.unit, th.spaced, th.stops), (Unit::Icu, false, ""));
    assert!(de.cue_max < LATIN.cue_max, "German words are longer");
    assert!(ALL.iter().all(|p| p.reveal_per_s > 0.0 && p.cue_max > 0));
}

/// Lesson T2: nothing in Thai text says whether the speaker is a man or a woman.
#[test]
fn thai_translations_are_told_not_to_guess_the_speakers_gender() {
    let th = profile("th");
    for word in ["ครับ", "ค่ะ", "ผม", "ดิฉัน"] {
        assert!(th.forbid.contains(&word), "{word}");
        assert!(th.prompt.contains(word), "{word}");
    }
}

// ---- script evidence ----

#[test]
fn letters_of_one_language_name_it_and_latin_letters_name_none() {
    let three = spoken(&["th", "zh", "en"]);
    assert_eq!(written_in(" ภาษาไทย.", &three), Some("th"));
    assert_eq!(written_in("我们", &three), Some("zh"));
    assert_eq!(written_in(" Class", &three), None);
    assert_eq!(written_in("我们 ok", &three), None, "two scripts");
    assert_eq!(written_in("我们", &spoken(&["zh", "ja"])), None, "both write Han");
    assert_eq!(written_in("これ", &spoken(&["zh", "ja"])), Some("ja"));
    assert_eq!(written_in("안녕하세요", &spoken(&["ko", "en"])), Some("ko"));
    assert_eq!(written_in("123 ", &three), None);
    assert_eq!(written_in("ภาษาไทย", &spoken(&["zh", "en"])), None, "nobody here writes Thai");
}

#[test]
fn the_script_of_a_text_is_the_one_all_its_letters_share() {
    assert_eq!(script(" Class".chars()), Some(Script::Latin));
    assert_eq!(script("Chiang Mai, 2026!".chars()), Some(Script::Latin));
    assert_eq!(script(" ภาษา".chars()), Some(Script::Thai));
    assert_eq!(script("テスト".chars()), Some(Script::Kana));
    assert_eq!(script("한국어".chars()), Some(Script::Hangul));
    assert_eq!(script("我们 ok".chars()), None);
    // Digits, punctuation and signs are in no script.
    for text in [" 2026", "…。", "3 × 4 ÷ 2", ""] {
        assert_eq!(script(text.chars()), None, "{text}");
    }
    assert_eq!(script(Some('é')), Some(Script::Latin));
    assert_eq!(script(None::<char>), None);
}

// ---- normalize ----

/// The service spells SARA AM as NIKHAHIT + SARA AA; every keyboard types the single letter.
#[test]
fn sara_am_is_composed_in_both_tone_mark_orders() {
    let th = profile("th");
    // ทำงาน, and น้ำ with the tone before and after NIKHAHIT.
    assert_eq!(th.normalize("ท\u{0E4D}\u{0E32}งาน"), "ทำงาน");
    assert_eq!(th.normalize("น\u{0E49}\u{0E4D}\u{0E32}"), "น้ำ");
    assert_eq!(th.normalize("น\u{0E4D}\u{0E49}\u{0E32}"), "น้ำ");
    // A NIKHAHIT that is not followed by SARA AA is left alone.
    assert_eq!(th.normalize("สํ"), "สํ");
    assert_eq!(th.normalize("ทำงาน"), "ทำงาน");
    assert_eq!(
        th.normalize("เรามาท\u{0E4D}\u{0E32}งานและดื่มน\u{0E49}\u{0E4D}\u{0E32}"),
        "เรามาทำงานและดื่มน้ำ"
    );
}

/// Lesson X1. Thai vowel signs and tone marks are nonspacing marks: a rule written with a
/// general character class drops them, and distinct words collapse into one.
#[test]
fn thai_vowels_and_tone_marks_survive() {
    for text in ["ไม่สำคัญ", "น้ำ", "เต็นท์", "ผู้ใหญ่", "ก็", "จำได้"]
    {
        for row in ALL {
            assert_eq!(row.normalize(text), text, "{} {text}", row.lang);
            let key = row.key(text);
            let marks =
                |s: &str| s.chars().filter(|c| ('\u{0E31}'..='\u{0E4E}').contains(c)).count();
            // A composed ำ is one mark and is spelled as two in the key; none is lost.
            let sara_am = text.matches('ำ').count();
            assert_eq!(marks(&key), marks(text) + sara_am, "{} {text} {key}", row.lang);
        }
    }
}

/// Spellings from a real three-language recording.
#[test]
fn chinese_loses_the_spaces_beside_full_width_punctuation() {
    let zh = profile("zh");
    assert_eq!(
        zh.normalize("清迈的数字社区。 以及来自多个国家的人"),
        "清迈的数字社区。以及来自多个国家的人"
    );
    assert_eq!(
        zh.normalize("在我们结束之前， 请记住 下周的课程将重点关注"),
        "在我们结束之前，请记住 下周的课程将重点关注"
    );
    assert_eq!(zh.normalize("支持 已分配的记录。 Mei。"), "支持 已分配的记录。Mei。");
    assert_eq!(zh.normalize(" 以及"), "以及");
    assert_eq!(zh.normalize(" 「好」 的"), "「好」的");
    assert_eq!(profile("ja").normalize(" これは、 テスト"), "これは、テスト");
}

/// Lesson X2. A space between two characters is the only mark of a pause the service heard
/// and did not punctuate; without it two sentences run together.
#[test]
fn spaces_between_han_are_kept() {
    for text in [
        "一起合作工作 我认为最重要的是成员之间的信任",
        "これは テスト",
        "他说 ZuTalk 很好用",
        "ผมคิดว่า เราควรเริ่มใหม่",
        "안녕하세요 반갑습니다",
        "we should start over",
        " start over",
        "trailing ",
        "第 3 章",
    ] {
        for row in ALL {
            assert_eq!(row.normalize(text), text, "{} {text}", row.lang);
        }
    }
}

#[test]
fn letters_typed_in_pieces_compose() {
    assert_eq!(profile("fr").normalize("cafe\u{301}"), "café");
    assert_eq!(profile("ko").normalize("\u{1112}\u{1161}\u{11AB}"), "한");
    assert_eq!(profile("ja").normalize("か\u{3099}"), "が");
}

/// The service streams a translation in chunks and joins them with a space. Into Chinese
/// from a language that spaces its words, that space falls in the middle of a phrase.
#[test]
fn a_translation_from_a_spaced_language_loses_the_seams_between_chinese_characters() {
    let (zh, en) = (profile("zh"), profile("en"));
    assert_eq!(
        zh.polish("很长的泰语段落 往往会在屏幕上出现", en),
        "很长的泰语段落往往会在屏幕上出现"
    );
    assert_eq!(
        zh.polish("请记住 下周的会议将重点关注 治理问题", en),
        "请记住下周的会议将重点关注治理问题"
    );
    assert_eq!(zh.polish("他说 ZuTalk 很好用", en), "他说 ZuTalk 很好用", "mixed script");
    assert_eq!(profile("ja").polish("これは テスト", profile("ko")), "これはテスト");
}

/// From Thai the space is the one Thai writes between clauses, and it stays.
#[test]
fn a_translation_from_thai_keeps_the_space_that_stood_for_a_sentence_break() {
    let text = "重要的是成员之间的信任 因为如果没有信任";
    assert_eq!(profile("zh").polish(text, profile("th")), text);
    assert_eq!(profile("zh").polish(text, profile("ja")), text);
}

// ---- units ----

/// Lesson X2. Thai runs its words together: captions reveal and wrap by dictionary words.
#[test]
fn thai_units_are_words() {
    let th = profile("th");
    // ผม|คิด|ว่า|เรา|ควร|เริ่ม|ใหม่
    assert_eq!(th.units("ผมคิดว่าเราควรเริ่มใหม่"), [2, 5, 8, 11, 14, 19, 23]);
    // The space between two clauses travels with the word after it.
    assert_eq!(th.units("ผมคิดว่า เราควร"), [2, 5, 8, 12, 15]);
    // An English term inside Thai speech is one unit, not one per letter.
    assert_eq!(th.units("ใช้ ZuTalk ได้"), [3, 10, 14]);
    let text = "อย่าลืมใส่ 3 ภาษานะคะ ไทย จีน และอังกฤษ";
    let units = th.units(text);
    assert!(units.len() < text.chars().count() / 2, "{units:?}");
    assert_eq!(units.last().copied(), Some(text.encode_utf16().count() as u16));
}

#[test]
fn spaced_languages_count_words_and_chinese_counts_characters() {
    assert_eq!(profile("en").units("we should start"), [2, 9, 15]);
    assert_eq!(profile("en").units(" Of course."), [3, 11]);
    assert_eq!(profile("ko").units("안녕하세요 반갑습니다"), [5, 11]);
    assert_eq!(profile("zh").units("他说 好。"), [1, 2, 4, 5]);
    assert_eq!(profile("ja").units("これは"), [1, 2, 3]);
}

/// The offsets are what a web view slices a string by: UTF-16 code units.
#[test]
fn offsets_count_utf16_code_units_and_never_split_a_character() {
    // 𠮷 is outside the basic plane: two code units, one character.
    assert_eq!(profile("zh").units("𠮷野家"), [2, 3, 4]);
    assert_eq!(profile("en").units("go 🎉 now"), [2, 5, 9]);
    assert_eq!(profile("th").units("ดี 🎉"), [2, 5]);
}

#[test]
fn text_with_nothing_to_show_has_no_units() {
    for row in ALL {
        assert_eq!(row.units(""), [] as [u16; 0], "{}", row.lang);
        assert_eq!(row.units(" \t "), [] as [u16; 0], "{}", row.lang);
        // Trailing white space belongs to no unit.
        let units = row.units("ok ");
        assert_eq!(units.last().copied(), Some(2), "{}", row.lang);
    }
}

#[test]
fn a_text_longer_than_the_offsets_can_count_stops_instead_of_wrapping() {
    let long = "字".repeat(70_000);
    let units = profile("zh").units(&long);
    assert_eq!(units.len(), u16::MAX as usize);
    assert!(units.windows(2).all(|w| w[0] < w[1]));
}

// ---- key ----

#[test]
fn a_search_key_folds_case_and_width() {
    let en = profile("en");
    assert_eq!(en.key("ＺｕＴａｌｋ ２０２６"), "zutalk 2026");
    assert_eq!(en.key("Straße ÉCOLE"), "straße école");
    assert_eq!(profile("ja").key("ｶﾀｶﾅ"), profile("ja").key("カタカナ"));
    assert_eq!(profile("zh").key("ＡＩ　时代"), "ai 时代");
}

/// Text stored before normalisation spells ำ in two letters, in either tone order; a
/// search typed on a keyboard spells it in one. All three must meet.
#[test]
fn a_search_key_spells_sara_am_one_way() {
    for row in ALL {
        let typed = row.key("น้ำ");
        assert_eq!(row.key("น\u{0E49}\u{0E4D}\u{0E32}"), typed, "{}", row.lang);
        assert_eq!(row.key("น\u{0E4D}\u{0E49}\u{0E32}"), typed, "{}", row.lang);
        assert_eq!(typed, "น\u{0E49}\u{0E4D}\u{0E32}", "{}", row.lang);
        assert_eq!(row.key("ทำงาน"), row.key("ท\u{0E4D}\u{0E32}งาน"), "{}", row.lang);
    }
}
