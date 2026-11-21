use std::collections::BTreeMap;
use zutalk_core::live::lines::Line;
use zutalk_core::out::export::{Doc, EXPORTERS};

fn line(t0: u32, t1: u32, speaker: &str, language: &str, text: &str, tr: &[(&str, &str)]) -> Line {
    Line {
        t0,
        t1,
        speaker: speaker.into(),
        language: language.into(),
        text: text.into(),
        translations: tr.iter().map(|(l, t)| (l.to_string(), t.to_string())).collect(),
        incomplete: false,
    }
}

fn meeting() -> Vec<Line> {
    vec![
        line(
            81_250,
            86_900,
            "A",
            "zh",
            "这三个案例有什么区别？",
            &[("en", "How do these three cases differ?")],
        ),
        line(
            87_000,
            90_400,
            "B",
            "en",
            "The first one ships in March.",
            &[("zh", "第一个三月发货。")],
        ),
    ]
}

fn render(id: &str, lines: &[Line], langs: &[&str]) -> String {
    let speakers = BTreeMap::from([("A".to_string(), "王总".to_string())]);
    let doc = Doc { title: "供货谈判", lines, speakers: &speakers };
    let langs: Vec<String> = langs.iter().map(|l| l.to_string()).collect();
    let exporter = EXPORTERS.iter().find(|e| e.id == id).unwrap();
    (exporter.render)(&doc, &langs)
}

#[test]
fn markdown_names_speakers_and_quotes_the_translations_asked_for() {
    let expected = "# 供货谈判\n\
        \n**王总** 00:01:21\n这三个案例有什么区别？\n> How do these three cases differ?\n\
        \n**B** 00:01:27\nThe first one ships in March.\n";
    assert_eq!(render("markdown", &meeting(), &["en"]), expected);
}

#[test]
fn markdown_without_languages_is_the_talk_as_spoken() {
    let out = render("markdown", &meeting(), &[]);
    assert!(!out.contains('>'));
    assert!(out.contains("The first one ships in March."));
}

#[test]
fn subtitles_are_in_one_language_original_or_translated() {
    let expected = "1\n00:01:21,250 --> 00:01:26,900\nHow do these three cases differ?\n\n\
        2\n00:01:27,000 --> 00:01:30,400\nThe first one ships in March.\n\n";
    assert_eq!(render("srt", &meeting(), &["en"]), expected);
}

#[test]
fn subtitles_as_spoken_when_no_language_is_asked_for() {
    let out = render("srt", &meeting(), &[]);
    assert!(out.contains("这三个案例有什么区别？"));
    assert!(out.contains("The first one ships in March."));
}

#[test]
fn a_line_with_no_text_in_the_language_asked_for_is_left_out() {
    let lines = [line(0, 1_000, "", "th", "สวัสดี", &[])];
    assert_eq!(render("srt", &lines, &["en"]), "");
}

#[test]
fn vtt_has_its_header_and_dots_in_times() {
    let out = render("vtt", &meeting(), &["en"]);
    assert!(out.starts_with("WEBVTT\n\n1\n00:01:21.250 --> 00:01:26.900\n"));
}

#[test]
fn a_long_line_is_cut_into_cues_that_share_its_time() {
    // 44 Han characters: two cues of 22, the line's ten seconds split in half.
    let text = "一二三四五六七八九十".repeat(4) + "一二三四";
    let out = render("srt", &[line(0, 10_000, "", "zh", &text, &[])], &[]);
    let cues: Vec<&str> = out.trim_end().split("\n\n").collect();
    assert_eq!(cues.len(), 2);
    assert!(cues[0].starts_with("1\n00:00:00,000 --> 00:00:05,000\n"));
    assert!(cues[1].starts_with("2\n00:00:05,000 --> 00:00:10,000\n"));
    assert_eq!(cues[0].lines().nth(2).unwrap().chars().count(), 22);
}

#[test]
fn english_cues_break_between_words() {
    let text =
        "one two three four five six seven eight nine ten eleven twelve thirteen fourteen fifteen";
    let out = render("srt", &[line(0, 15_000, "", "en", text, &[])], &[]);
    let cues: Vec<&str> = out.trim_end().split("\n\n").collect();
    assert_eq!(cues.len(), 2);
    assert!(cues[0].ends_with("thirteen fourteen"));
    assert!(cues[1].ends_with("\nfifteen"));
}
