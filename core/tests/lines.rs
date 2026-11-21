use zutalk_core::lines::{Assembler, Boundary, Line, Token};

/// A settled word, as the service sends it.
fn said(text: &str, language: &str, speaker: &str, start_ms: u32, end_ms: u32) -> Token {
    Token {
        text: text.into(),
        start_ms,
        end_ms,
        is_final: true,
        speaker: speaker.into(),
        language: language.into(),
        into: None,
    }
}

/// A word the service is still guessing at.
fn guess(text: &str, language: &str, speaker: &str, start_ms: u32, end_ms: u32) -> Token {
    Token { is_final: false, ..said(text, language, speaker, start_ms, end_ms) }
}

/// A settled piece of a translation `into` a language, of speech in `from`.
fn translated(text: &str, into: &str, from: &str, speaker: &str) -> Token {
    Token { into: Some(into.into()), ..said(text, from, speaker, 0, 0) }
}

fn three() -> Assembler {
    Assembler::new(&["th", "zh", "en"])
}

fn pair() -> Assembler {
    Assembler::new(&["en", "zh"])
}

/// The lines one endpoint of a three-language session makes of these tokens.
fn rows(tokens: &[Token]) -> Vec<(String, String)> {
    three().push(tokens, Boundary::Provider).into_iter().map(|l| (l.language, l.text)).collect()
}

fn row(language: &str, text: &str) -> (String, String) {
    (language.into(), text.into())
}

fn texts(lines: &[Line]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

fn only(mut lines: Vec<Line>) -> Line {
    assert_eq!(lines.len(), 1, "{lines:#?}");
    lines.remove(0)
}

// ---- 1. guesses are a whole revision ----

#[test]
fn a_guess_is_replaced_by_the_next_response_not_appended_to() {
    let mut lines = pair();
    assert!(lines.push(&[guess("hel", "en", "", 0, 100)], Boundary::None).is_empty());
    assert_eq!(texts(&lines.tail()), ["hel"]);
    assert!(lines.push(&[guess("hello", "en", "", 0, 200)], Boundary::None).is_empty());
    assert_eq!(texts(&lines.tail()), ["hello"]);
    let settled = only(lines.push(&[said("hello", "en", "", 0, 200)], Boundary::Provider));
    assert_eq!((settled.text.as_str(), settled.incomplete), ("hello", false));
}

#[test]
fn a_guess_the_next_response_leaves_out_is_gone() {
    let mut lines = pair();
    lines.push(
        &[said("hello", "en", "", 220, 520), guess(" world", "en", "", 600, 800)],
        Boundary::None,
    );
    assert_eq!(texts(&lines.tail()), ["hello world"]);
    lines.push(&[], Boundary::None);
    let tail = only(lines.tail());
    assert_eq!((tail.text.as_str(), tail.incomplete), ("hello", false));
}

#[test]
fn a_guess_brings_its_own_times_and_settled_times_stay() {
    let mut lines = pair();
    lines.push(&[guess("hel", "en", "", 100, 900)], Boundary::None);
    lines.push(&[said("hello", "en", "", 220, 520)], Boundary::None);
    lines.push(&[guess(" world", "en", "", 600, 800)], Boundary::None);
    lines.push(&[guess(" world!", "en", "", 650, 750)], Boundary::None);
    let tail = only(lines.tail());
    assert_eq!((tail.text.as_str(), tail.t0, tail.t1), ("hello world!", 220, 750));
    assert!(tail.incomplete);
    let settled = only(lines.push(&[], Boundary::Provider));
    assert_eq!((settled.text.as_str(), settled.t0, settled.t1), ("hello", 220, 520));
}

// ---- 2. a line starts only where settled words change voice or language ----

/// The service's speaker labels flip inside a word now and then, on the same voice, with
/// no pause at all. A Thai word is several tokens, so it was cut in two: "เต็น" / "ท์ไว้".
#[test]
fn a_speaker_label_that_flips_without_a_pause_does_not_cut_the_line() {
    let rows = rows(&[
        said("เราเตรียมเต็น", "th", "1", 0, 600),
        said("ท์ไว้", "th", "2", 600, 900),
        said(" fin", "en", "2", 2_000, 2_200),
        said("e.", "en", "3", 2_300, 2_400),
    ]);
    assert_eq!(rows, [row("th", "เราเตรียมเต็นท์ไว้"), row("en", "fine.")]);
}

#[test]
fn a_new_speaker_after_a_pause_starts_a_line() {
    let lines = three().push(
        &[said("สวัสดี", "th", "1", 0, 600), said("ครับ", "th", "2", 1_000, 1_300)],
        Boundary::Provider,
    );
    assert_eq!(texts(&lines), ["สวัสดี", "ครับ"]);
    assert_eq!((lines[0].speaker.as_str(), lines[1].speaker.as_str()), ("1", "2"));
    assert_eq!((lines[0].t0, lines[0].t1, lines[1].t0, lines[1].t1), (0, 600, 1_000, 1_300));
}

#[test]
fn another_speaker_in_another_language_starts_a_line_without_a_pause() {
    let rows = rows(&[said("ตกลงค่ะ", "th", "1", 0, 600), said(" Great", "en", "2", 660, 900)]);
    assert_eq!(rows, [row("th", "ตกลงค่ะ"), row("en", "Great")]);
}

#[test]
fn one_speaker_changing_language_starts_a_line_each_time() {
    let lines = pair().push(
        &[
            said("你好", "zh", "0", 0, 500),
            said(" hello there everyone", "en", "0", 1_400, 2_400),
            said("再见", "zh", "0", 3_300, 3_800),
        ],
        Boundary::Provider,
    );
    assert_eq!(texts(&lines), ["你好", "hello there everyone", "再见"]);
    assert!(lines.iter().all(|l| l.speaker == "0" && !l.incomplete));
}

/// A guess never draws a line: its label and its speaker are revised until it settles.
#[test]
fn a_guessed_language_and_speaker_are_revised_until_the_words_settle() {
    let mut lines = Assembler::new(&["th", "zh"]);
    lines.push(&[guess("สวัส", "zh", "0", 0, 300)], Boundary::None);
    lines.push(&[guess("สวัสดี", "th", "1", 0, 600)], Boundary::None);
    let line = only(lines.push(&[said("สวัสดี", "th", "1", 0, 600)], Boundary::Provider));
    assert_eq!((line.language.as_str(), line.speaker.as_str()), ("th", "1"));
    assert_eq!(line.text, "สวัสดี");
}

#[test]
fn guesses_in_two_languages_are_two_lines_only_once_they_settle_that_way() {
    let mut lines = pair();
    lines.push(
        &[guess("你好", "zh", "0", 0, 500), guess(" hello everyone here", "en", "0", 1_500, 2_500)],
        Boundary::None,
    );
    // Shown as heard so far, but nothing is finished by a guess.
    assert_eq!(texts(&lines.tail()), ["你好", "hello everyone here"]);
    let settled = lines.push(&[said("你好大家", "zh", "0", 0, 900)], Boundary::Provider);
    assert_eq!(texts(&settled), ["你好大家"]);
}

// ---- 3. a line's language: `und`, then what its letters say ----

/// Lesson S7. A whole session may come without language labels.
#[test]
fn und_is_promoted_on_script_evidence() {
    let mut lines = three();
    lines.push(&[said(" 10", "", "1", 0, 300)], Boundary::None);
    assert_eq!(only(lines.tail()).language, "und");
    lines.push(&[said(" หลัง น่าจะพอ", "", "1", 300, 1_500)], Boundary::None);
    assert_eq!(only(lines.tail()).language, "th");
    let line = only(
        lines.push(&[said(" 我们走吧", "", "2", 2_500, 3_300)], Boundary::Provider).split_off(1),
    );
    assert_eq!((line.language.as_str(), line.text.as_str()), ("zh", "我们走吧"));
}

#[test]
fn a_line_with_no_letters_and_no_label_stays_und() {
    let line = only(three().push(&[said(" 2026.", "", "1", 0, 600)], Boundary::Provider));
    assert_eq!((line.language.as_str(), line.text.as_str()), ("und", "2026."));
    // Latin letters name no language by themselves.
    let line = only(three().push(&[said(" Okay.", "", "1", 0, 600)], Boundary::Provider));
    assert_eq!(line.language, "und");
}

/// A number has no language. The service labelled "10" in a Thai sentence English, and the
/// sentence was cut in three.
#[test]
fn digits_take_their_rows_language() {
    let rows = rows(&[
        said("เราเตรียมเต็นท์ไว้", "th", "1", 0, 900),
        said(" 10", "en", "1", 1_000, 1_300),
        said(" หลัง น่าจะพอ", "th", "1", 1_300, 2_500),
    ]);
    assert_eq!(rows, [row("th", "เราเตรียมเต็นท์ไว้ 10 หลัง น่าจะพอ")]);
}

/// Thai letters are Thai whatever the service labels them. It labelled the end of a Thai
/// sentence English, and the line went to the English column.
#[test]
fn letters_overrule_the_services_label() {
    let rows = rows(&[
        said("ส่วนคุณซาร่าจะเล่าเรื่องการทำ", "th", "1", 0, 2_000),
        said(" Power", "th", "1", 2_000, 2_400),
        said(" Class", "en", "1", 2_900, 3_300),
        said(" ภาษาไทย.", "en", "1", 3_400, 4_000),
    ]);
    assert_eq!(rows, [row("th", "ส่วนคุณซาร่าจะเล่าเรื่องการทำ Power Class ภาษาไทย.")]);
}

#[test]
fn a_language_nobody_named_keeps_the_services_label() {
    let lines = pair().push(
        &[
            said("Usually we have this saying in Arabic.", "en", "1", 0, 2_000),
            said(" الحاجة أم الاختراع", "ar", "1", 2_100, 4_000),
        ],
        Boundary::Provider,
    );
    assert_eq!(lines.iter().map(|l| l.language.as_str()).collect::<Vec<_>>(), ["en", "ar"]);
    assert_eq!(lines[1].text, "الحاجة أم الاختراع");
}

// ---- 4. Latin words inside a sentence in another script ----

/// An English sentence right after a Thai one, under the same speaker label: its first two
/// words were taken for words inside the Thai sentence and stayed on the Thai line when the
/// third showed the language had changed: "…4 ทุ่ม If you" / "are joining…".
#[test]
fn an_english_sentence_after_a_thai_one_keeps_its_first_words() {
    let lines = three().push(
        &[
            said("เราเลยต้องปิดแอร์หลัง 4 ทุ่ม", "th", "1", 103_420, 109_700),
            said(" I", "en", "1", 110_020, 110_080),
            said("f", "en", "1", 110_080, 110_140),
            said(" y", "en", "1", 110_140, 110_200),
            said("ou", "en", "1", 110_200, 110_260),
            said(" are", "en", "1", 110_320, 110_380),
            said(" joining from Chiang Rai.", "en", "1", 110_500, 112_000),
        ],
        Boundary::Provider,
    );
    assert_eq!(texts(&lines), ["เราเลยต้องปิดแอร์หลัง 4 ทุ่ม", "If you are joining from Chiang Rai."]);
    assert_eq!((lines[0].language.as_str(), lines[1].language.as_str()), ("th", "en"));
    // The words went back with their times.
    assert_eq!((lines[0].t0, lines[0].t1), (103_420, 109_700));
    assert_eq!((lines[1].t0, lines[1].t1), (110_020, 112_000));
}

/// An English word or two inside a Thai sentence is part of it; a sentence of English is
/// not, even when the service hears one speaker.
#[test]
fn an_english_sentence_after_thai_does_not_stay_on_the_thai_line() {
    let rows = rows(&[
        said("ตกลงค่ะ", "th", "1", 0, 600),
        said(" Do", "en", "1", 1_300, 1_500),
        said(" we", "en", "1", 1_500, 1_600),
        said(" already", "en", "1", 1_600, 1_900),
        said(" have", "en", "1", 1_900, 2_100),
        said(" a", "en", "1", 2_100, 2_200),
        said(" list?", "en", "1", 2_200, 2_500),
    ]);
    assert_eq!(rows, [row("th", "ตกลงค่ะ"), row("en", "Do we already have a list?")]);
}

#[test]
fn two_english_words_stay_and_thai_resumes_on_the_same_line() {
    let rows = rows(&[
        said("เราใช้", "th", "1", 0, 600),
        said(" Power", "en", "1", 700, 1_000),
        said(" Class", "en", "1", 1_000, 1_300),
        said(" ทุกวัน", "th", "1", 1_400, 2_000),
        said(" Zoom", "en", "1", 2_100, 2_400),
        said(" Meeting", "en", "1", 2_400, 2_800),
        said(" ด้วย", "th", "1", 2_900, 3_300),
    ]);
    // The count starts again each time Thai resumes: four words, never three in a row.
    assert_eq!(rows, [row("th", "เราใช้ Power Class ทุกวัน Zoom Meeting ด้วย")]);
}

#[test]
fn an_english_line_does_not_take_in_words_of_another_latin_language() {
    let lines = Assembler::new(&["en", "fr"]).push(
        &[
            said("See you tomorrow.", "en", "1", 0, 1_200),
            said(" Bonjour", "fr", "1", 1_300, 1_800),
        ],
        Boundary::Provider,
    );
    assert_eq!(texts(&lines), ["See you tomorrow.", "Bonjour"]);
}

/// The service heard 上次 as "Shots." and ran straight on into the rest of the Chinese
/// sentence: its full stop does not make it a sentence.
#[test]
fn a_misheard_word_with_a_full_stop_still_opens_the_sentence_after_it() {
    let rows = rows(&[
        said(" Shots.", "en", "2", 4_620, 4_920),
        said("有人说太挤了。", "zh", "2", 4_920, 6_000),
    ]);
    assert_eq!(rows, [row("zh", "Shots.有人说太挤了。")]);
}

/// "Of course." is a sentence of its own. A Chinese sentence after it under the same
/// speaker label is not opened by it.
#[test]
fn an_english_reply_before_a_chinese_sentence_keeps_its_own_line() {
    let rows = rows(&[
        said(" Of", "en", "2", 401_720, 401_900),
        said(" course.", "en", "2", 401_900, 402_140),
        said(" 还有一个问题", "zh", "2", 402_440, 403_160),
    ]);
    assert_eq!(rows, [row("en", "Of course."), row("zh", "还有一个问题")]);
}

#[test]
fn a_thai_sentence_that_opens_with_an_english_term_is_one_thai_line() {
    let rows = rows(&[
        said(" Co-working space", "en", "1", 0, 1_200),
        said(" ชั้น 3 ค่ะ", "th", "1", 1_500, 2_400),
    ]);
    assert_eq!(rows, [row("th", "Co-working space ชั้น 3 ค่ะ")]);
}

// ---- 5. a word in several tokens ----

/// The service sends one English word as several tokens. Counting tokens, "Hotsp" + "ot"
/// was a third embedded word, and the word was cut across two lines.
#[test]
fn an_english_word_in_several_tokens_is_one_word() {
    let rows = rows(&[
        said("就4个。", "zh", "2", 59_440, 59_980),
        said(" Hot", "en", "2", 60_160, 60_400),
        said("sp", "en", "2", 60_460, 60_520),
        said("ot", "en", "2", 60_520, 60_700),
        said("和移动", "en", "2", 60_700, 61_000),
        said(" 有12个团队报名。", "zh", "2", 61_060, 62_400),
    ]);
    assert_eq!(rows, [row("zh", "就4个。Hotspot和移动 有12个团队报名。")]);
}

/// The same cut in a Thai line, with the speaker label flipping inside the word:
/// "Of cours" ended one line and "e." opened the next.
#[test]
fn a_line_never_starts_inside_a_word() {
    let rows = rows(&[
        said("อย่าลืมใส่ 3 ภาษานะคะ", "th", "1", 28_900, 33_700),
        said(" Of", "en", "1", 34_140, 34_300),
        said(" cours", "en", "1", 34_300, 34_600),
        said("e.", "en", "2", 34_600, 34_800),
        said(" เราเตรียมเต็นท์ไว้", "th", "1", 35_230, 36_500),
    ]);
    assert_eq!(rows, [row("th", "อย่าลืมใส่ 3 ภาษานะคะ Of course."), row("th", "เราเตรียมเต็นท์ไว้")]);
}

#[test]
fn the_rest_of_a_word_under_another_label_after_a_pause_is_another_speaker() {
    let rows = rows(&[said(" Yes", "en", "1", 0, 400), said("okay", "en", "2", 900, 1_300)]);
    assert_eq!(rows, [row("en", "Yes"), row("en", "okay")]);
}

// ---- 6. translations carry no times ----

#[test]
fn a_translation_joins_its_line_in_response_order() {
    let line = only(pair().push(
        &[
            said("good morning", "en", "", 100, 900),
            translated("早上", "zh", "en", ""),
            translated("好", "zh", "en", ""),
        ],
        Boundary::Provider,
    ));
    assert_eq!((line.text.as_str(), line.t0, line.t1), ("good morning", 100, 900));
    assert_eq!(line.translations, [("zh".to_string(), "早上好".to_string())].into());
}

#[test]
fn a_translation_sent_after_each_line_reaches_that_line() {
    let lines = Assembler::new(&["th", "zh"]).push(
        &[
            said("你好", "zh", "0", 0, 500),
            translated("สวัสดี", "th", "zh", "0"),
            said("ขอบคุณ", "th", "0", 600, 1_200),
            translated("谢谢", "zh", "th", "0"),
        ],
        Boundary::Provider,
    );
    assert_eq!(texts(&lines), ["你好", "ขอบคุณ"]);
    assert_eq!(lines[0].translations["th"], "สวัสดี");
    assert_eq!(lines[1].translations["zh"], "谢谢");
}

/// The service may send every original of a response before any translation. "The line of
/// the word before it" would give both translations to the last line.
#[test]
fn translations_sent_after_all_the_originals_reach_their_own_lines() {
    let lines = Assembler::new(&["th", "zh"]).push(
        &[
            said("你好", "zh", "a", 0, 500),
            said("ขอบคุณ", "th", "a", 600, 1_200),
            translated("สวัสดี", "th", "zh", "a"),
            translated("谢谢", "zh", "th", "a"),
        ],
        Boundary::Provider,
    );
    assert_eq!(lines[0].translations, [("th".to_string(), "สวัสดี".to_string())].into());
    assert_eq!(lines[1].translations, [("zh".to_string(), "谢谢".to_string())].into());
}

#[test]
fn a_speaker_switching_back_and_forth_gets_every_translation_in_order() {
    let lines = pair().push(
        &[
            said("你好", "zh", "0", 0, 500),
            said(" hello there everyone", "en", "0", 1_400, 2_400),
            said("再见", "zh", "0", 3_300, 3_800),
            translated("hello-en", "en", "zh", "0"),
            translated("hello-zh", "zh", "en", "0"),
            translated("goodbye-en", "en", "zh", "0"),
        ],
        Boundary::Provider,
    );
    let translations: Vec<_> = lines.iter().flat_map(|l| l.translations.values()).collect();
    assert_eq!(translations, ["hello-en", "hello-zh", "goodbye-en"]);
}

/// A speaker went from English into Arabic while the Chinese translation of the English was
/// still arriving. The rest of it came after the English line had closed.
#[test]
fn a_translation_still_arriving_after_its_line_closed_reaches_it() {
    let mut lines = pair();
    lines.push(
        &[
            said("Usually we have this saying in Arabic.", "en", "1", 0, 2_000),
            translated("通常我们有", "zh", "en", "1"),
        ],
        Boundary::None,
    );
    lines.push(&[said(" الحاجة أم الاختراع", "ar", "1", 2_100, 4_000)], Boundary::None);
    assert_eq!(lines.tail()[0].translations["zh"], "通常我们有");
    let done =
        lines.push(&[translated("这种阿拉伯语的说法。", "zh", "en", "1")], Boundary::Provider);
    assert_eq!(done[0].translations["zh"], "通常我们有这种阿拉伯语的说法。");
    assert!(done[1].translations.is_empty());
}

#[test]
fn two_speakers_in_one_language_each_get_their_own_translation() {
    let lines = pair().push(
        &[
            said("first", "en", "a", 0, 500),
            said(" second", "en", "b", 900, 1_400),
            translated("乙", "zh", "en", "b"),
            translated("甲", "zh", "en", "a"),
        ],
        Boundary::Provider,
    );
    assert_eq!(lines[0].translations["zh"], "甲");
    assert_eq!(lines[1].translations["zh"], "乙");
}

/// The service heard "Shots" in English and translated it into Chinese; the same speaker
/// went on in Chinese 60 ms later and the line became that Chinese sentence's.
#[test]
fn a_line_that_turns_into_its_translations_language_has_no_translation_in_it() {
    let mut lines = pair();
    lines.push(
        &[said(" Shots", "en", "2", 4_620, 4_800), translated("开枪", "zh", "en", "2")],
        Boundary::None,
    );
    assert_eq!(only(lines.tail()).translations["zh"], "开枪");
    lines.push(&[said(" 有人说太挤了。", "zh", "2", 4_860, 5_940)], Boundary::None);
    let line = only(lines.push(
        &[translated(" Someone said it was too crowded.", "en", "zh", "2")],
        Boundary::Provider,
    ));
    assert_eq!((line.language.as_str(), line.text.as_str()), ("zh", "Shots 有人说太挤了。"));
    let en = [("en".to_string(), "Someone said it was too crowded.".to_string())];
    assert_eq!(line.translations, en.into());
}

/// A Chinese translation from English loses the seams between the service's chunks.
#[test]
fn a_translation_is_set_for_its_reader() {
    let line = only(pair().push(
        &[
            said("Long Thai passages tend to arrive late on the screen.", "en", "", 100, 2_000),
            translated("很长的泰语段落", "zh", "en", ""),
            translated(" 往往会在屏幕上出现得比较晚。", "zh", "en", ""),
        ],
        Boundary::Provider,
    ));
    assert_eq!(line.translations["zh"], "很长的泰语段落往往会在屏幕上出现得比较晚。");
}

#[test]
fn a_translation_with_no_line_to_belong_to_is_dropped() {
    let mut lines = pair();
    assert!(lines.push(&[translated("你好", "zh", "en", "")], Boundary::Provider).is_empty());
    // Nor does it attach to a line in its own language, or to another speaker's.
    let line = only(lines.push(
        &[
            said("你好", "zh", "a", 0, 500),
            translated("你好", "zh", "en", "a"),
            translated("hello", "en", "zh", "b"),
            translated(" ", "en", "zh", "a"),
        ],
        Boundary::Provider,
    ));
    assert!(line.translations.is_empty(), "{line:?}");
}

#[test]
fn a_guessed_translation_is_shown_and_replaced_like_any_guess() {
    let mut lines = pair();
    lines.push(&[said("你好", "zh", "2", 0, 500)], Boundary::None);
    lines.push(&[Token { is_final: false, ..translated(" Hel", "en", "zh", "2") }], Boundary::None);
    assert_eq!(only(lines.tail()).translations["en"], "Hel");
    lines.push(
        &[Token { is_final: false, ..translated(" Hello", "en", "zh", "2") }],
        Boundary::None,
    );
    let tail = only(lines.tail());
    assert_eq!(tail.translations["en"], "Hello");
    // Its words are all settled: only the translation is still being guessed.
    assert!(!tail.incomplete);
    assert!(only(lines.push(&[], Boundary::Provider)).translations.is_empty());
}

// ---- 7. boundaries ----

/// Lesson S5. The truncated line directly above its own completion: 172 of the 222
/// speculative rows in the old library were this pair.
#[test]
fn speculation_past_a_provider_boundary_is_dropped() {
    let mut lines = pair();
    lines.push(&[guess(" 什么？这三个案例", "zh", "", 1_125_720, 1_126_560)], Boundary::None);
    // On screen while it is being spoken.
    assert_eq!(texts(&lines.tail()), ["什么？这三个案例"]);

    let frozen = lines
        .push(&[guess(" 什么？这三个案例", "zh", "", 1_125_720, 1_126_560)], Boundary::Provider);
    assert!(frozen.is_empty(), "these words belong to the line after the boundary: {frozen:#?}");

    // Which the service then sends, whole.
    let line = only(lines.push(
        &[said(" 什么？这三个案例有什么区别？它是三个。", "zh", "", 1_125_720, 1_127_880)],
        Boundary::Provider,
    ));
    assert_eq!(
        (line.text.as_str(), line.t0),
        ("什么？这三个案例有什么区别？它是三个。", 1_125_720)
    );
    assert!(!line.incomplete);
}

/// The other half of the rule. Nothing follows a seam, so the guess is the only record
/// that this speech happened.
#[test]
fn tail_is_frozen_incomplete_at_seam_and_stop() {
    let mut lines = pair();
    lines.push(&[guess(" 什么？这三个案例", "zh", "", 1_125_720, 1_126_560)], Boundary::None);
    assert!(only(lines.tail()).incomplete);

    let kept = only(
        lines.push(&[guess(" 什么？这三个案例", "zh", "", 1_125_720, 1_126_560)], Boundary::Seam),
    );
    assert_eq!((kept.text.as_str(), kept.language.as_str()), ("什么？这三个案例", "zh"));
    assert_eq!((kept.t0, kept.t1, kept.incomplete), (1_125_720, 1_126_560, true));
    // It was handed over once.
    assert!(lines.tail().is_empty());
    assert!(lines.push(&[], Boundary::Seam).is_empty());
}

/// A boundary takes back only the speculation sitting past it, never settled words.
#[test]
fn a_provider_boundary_keeps_the_settled_words_and_drops_only_the_guess() {
    let tokens = [
        said(" 它给你讲了三个案例。", "zh", "", 9_360, 12_000),
        guess("你那个", "zh", "", 12_000, 12_600),
    ];
    let closed = only(pair().push(&tokens, Boundary::Provider));
    assert_eq!(
        (closed.text.as_str(), closed.t1, closed.incomplete),
        ("它给你讲了三个案例。", 12_000, false)
    );

    let frozen = only(pair().push(&tokens, Boundary::Seam));
    assert_eq!(
        (frozen.text.as_str(), frozen.t1, frozen.incomplete),
        ("它给你讲了三个案例。你那个", 12_600, true)
    );
}

/// The service marked an endpoint while "Of cours", under another speaker label than the
/// Thai line it trailed, was still a guess, then sent "Of course." settled. The guess was
/// frozen as a line of its own: "Of cours" above "Of course.".
#[test]
fn an_endpoint_during_speculation_leaves_one_line() {
    let mut lines = three();
    let mut finished = Vec::new();
    finished.extend(lines.push(
        &[said("อย่าลืมใส่ 3 ภาษานะคะ ไทย จีน และอังกฤษ", "th", "4", 518_360, 522_860)],
        Boundary::None,
    ));
    finished.extend(lines.push(&[guess(" Of", "en", "4", 523_580, 523_640)], Boundary::None));
    let guessed =
        [guess(" Of", "en", "2", 523_520, 523_580), guess(" cours", "en", "2", 523_700, 523_760)];
    finished.extend(lines.push(&guessed, Boundary::Provider));
    finished.extend(lines.push(
        &[guessed[0].clone(), guessed[1].clone(), guess("e.", "en", "2", 523_940, 524_000)],
        Boundary::None,
    ));
    finished.extend(lines.push(
        &[
            said(" Of", "en", "2", 523_520, 523_580),
            said(" cours", "en", "2", 523_700, 523_760),
            said("e.", "en", "2", 523_760, 523_820),
        ],
        Boundary::Provider,
    ));
    assert_eq!(texts(&finished), ["อย่าลืมใส่ 3 ภาษานะคะ ไทย จีน และอังกฤษ", "Of course."]);
}

#[test]
fn a_guess_under_another_voice_is_frozen_as_its_own_line_at_a_seam() {
    let lines = pair().push(
        &[said("hello", "en", "a", 0, 500), guess(" goodbye", "en", "b", 1_000, 1_500)],
        Boundary::Seam,
    );
    assert_eq!(texts(&lines), ["hello", "goodbye"]);
    assert_eq!((lines[0].incomplete, lines[1].incomplete), (false, true));
    assert_eq!((lines[0].speaker.as_str(), lines[1].speaker.as_str()), ("a", "b"));
}

#[test]
fn nothing_is_finished_before_a_boundary_and_each_line_is_finished_once() {
    let mut lines = pair();
    assert!(lines.push(&[said("first.", "en", "a", 0, 500)], Boundary::None).is_empty());
    assert!(lines.push(&[said(" second.", "en", "b", 900, 1_400)], Boundary::None).is_empty());
    assert_eq!(texts(&lines.tail()), ["first.", "second."]);
    assert_eq!(texts(&lines.push(&[], Boundary::Provider)), ["first.", "second."]);
    assert!(lines.tail().is_empty());
    assert!(lines.push(&[], Boundary::Provider).is_empty());
    // The next line starts clean: the voice before the boundary does not hold it back.
    let next = only(lines.push(&[said(" third.", "en", "b", 1_450, 1_900)], Boundary::Seam));
    assert_eq!((next.text.as_str(), next.t0, next.incomplete), ("third.", 1_450, false));
}

// ---- 8. blank segments ----

/// Lesson S6. The service sends the word separator as a token of its own: 264 rows of the
/// old library held nothing but one space, ten of them with a translation of it.
#[test]
fn blank_segment_is_not_a_line() {
    let mut lines = pair();
    let blank = [said(" ", "zh", "", 9_300, 9_360), translated(" ", "en", "zh", "")];
    assert!(lines.push(&blank, Boundary::None).is_empty());
    assert!(lines.tail().is_empty());
    assert!(lines.push(&[], Boundary::Seam).is_empty());
    assert!(lines.push(&[guess(" ", "zh", "", 9_300, 9_360)], Boundary::Seam).is_empty());

    // The same separator followed by speech is the ordinary case. It opens a Chinese line,
    // where the language has none, and is dropped.
    let line = only(lines.push(
        &[said(" ", "zh", "", 9_300, 9_360), said("它给你讲了三个案例。", "zh", "", 9_360, 12_000)],
        Boundary::Provider,
    ));
    assert_eq!(line.text, "它给你讲了三个案例。");
}

// ---- 9. text is set after it is joined ----

/// The service spells SARA AM as NIKHAHIT + SARA AA, sometimes across two tokens.
#[test]
fn thai_sara_am_reaches_lines_composed_even_across_tokens() {
    let mut lines = pair();
    lines.push(
        &[
            said("เรามาท\u{0E4D}", "th", "", 1_000, 1_400),
            said("\u{0E32}งานและดื่มน\u{0E49}\u{0E4D}\u{0E32}", "th", "", 1_400, 2_200),
        ],
        Boundary::None,
    );
    assert_eq!(texts(&lines.tail()), ["เรามาทำงานและดื่มน้ำ"]);
    assert_eq!(texts(&lines.push(&[], Boundary::Provider)), ["เรามาทำงานและดื่มน้ำ"]);
}

/// Chinese the way it is written: the service's spaces beside `。` and `，` and before the
/// first character do not reach the line; a space between two characters, its mark of an
/// unpunctuated pause, does. Spellings from a real recording.
#[test]
fn chinese_lines_do_not_keep_the_services_spaces_beside_punctuation() {
    let line = only(pair().push(
        &[
            said(" 清迈的数字社区。", "zh", "", 1_000, 2_000),
            said(" 以及来自多个国家的人，请记住", "zh", "", 2_000, 3_000),
            said(" 下周的课程用 ZuTalk 记录", "zh", "", 3_000, 4_000),
        ],
        Boundary::Provider,
    ));
    assert_eq!(line.text, "清迈的数字社区。以及来自多个国家的人，请记住 下周的课程用 ZuTalk 记录");
}

// ---- whatever the input ----

/// A small deterministic generator: no dependency, the same sequence every run.
fn pick(seed: &mut u64, n: usize) -> usize {
    *seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    ((*seed >> 33) as usize) % n
}

/// Over any sequence of responses: nothing panics, no line is blank, and every settled word
/// reaches exactly one finished line, in order. Guesses never do, except at a seam.
#[test]
fn settled_words_reach_exactly_one_line_whatever_the_order() {
    let words = [
        " Of",
        " cours",
        "e.",
        " Hot",
        "sp",
        "ot",
        " ",
        "",
        " 10",
        "就4个。",
        "和移动",
        " 有人说",
        "เราเตรียม",
        " ทุ่ม",
        " ภาษาไทย.",
        " If",
        " you",
        " are",
        "?",
        "안녕",
    ];
    let languages = ["th", "zh", "en", "", "ko", "ar"];
    let speakers = ["1", "2", ""];
    let boundaries =
        [Boundary::None, Boundary::None, Boundary::None, Boundary::Provider, Boundary::Seam];
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    for case in 0..2_000_u64 {
        let (mut seed, mut clock) = (case, 0_u32);
        let (mut lines, mut sent, mut got) = (three(), String::new(), String::new());
        for _ in 0..pick(&mut seed, 40) {
            let mut response = Vec::new();
            for _ in 0..pick(&mut seed, 5) {
                clock += [0, 60, 250, 800][pick(&mut seed, 4)];
                let text = words[pick(&mut seed, words.len())];
                let (language, speaker) =
                    (languages[pick(&mut seed, 6)], speakers[pick(&mut seed, 3)]);
                let mut token = said(text, language, speaker, clock, clock + 50);
                match pick(&mut seed, 7) {
                    0 => token.is_final = false,
                    1 | 2 => token.into = Some(languages[pick(&mut seed, 6)].into()),
                    _ => sent += text,
                }
                // A translation may be a guess too.
                token.is_final &= token.into.is_none() || pick(&mut seed, 2) == 0;
                // Sometimes the clock runs backwards: a reconnect numbers its audio anew.
                clock = clock.saturating_sub([0, 0, 0, 5_000][pick(&mut seed, 4)]);
                response.push(token);
            }
            // A guess frozen at a seam is the one thing that is not a settled word.
            let boundary = boundaries[pick(&mut seed, boundaries.len())];
            let guesses = response.iter().filter(|t| !t.is_final && t.into.is_none());
            if boundary == Boundary::Seam {
                guesses.for_each(|t| sent += &t.text);
            }
            let tail = lines.tail();
            let finished = lines.push(&response, boundary);
            assert!(boundary != Boundary::None || finished.is_empty(), "case {case}");
            for line in finished.iter().chain(&tail) {
                assert!(!line.text.trim().is_empty(), "case {case}: {line:?}");
                assert_eq!(line.text, line.text.trim(), "case {case}");
                assert!(line.t0 <= line.t1, "case {case}: {line:?}");
                assert!(!line.language.is_empty(), "case {case}");
                assert!(!line.translations.contains_key(&line.language), "case {case}: {line:?}");
                assert!(line.translations.values().all(|t| !t.trim().is_empty()), "case {case}");
            }
            finished.iter().for_each(|line| got += &line.text);
        }
        lines.push(&[], Boundary::Seam).iter().for_each(|line| got += &line.text);
        assert_eq!(squeeze(&got), squeeze(&sent), "case {case}");
    }
}

#[test]
fn odd_tokens_do_not_panic() {
    let mut lines = Assembler::new(&[]);
    let odd = [
        said("", "", "", u32::MAX, 0),
        said("\u{0E4D}", "th", "", 0, u32::MAX),
        said("\u{0E32}", "zz-Unknown", "?", 5, 1),
        translated("", "", "", ""),
        guess("e\u{301}", "en", "", 0, 0),
        said("𠮷", "zh", "", 0, 0),
    ];
    for boundary in [Boundary::None, Boundary::Provider, Boundary::Seam] {
        lines.push(&odd, boundary);
        lines.tail();
    }
    assert!(Assembler::new(&["en"]).push(&[], Boundary::Seam).is_empty());
}
