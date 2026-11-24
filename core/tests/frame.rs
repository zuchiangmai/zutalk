//! Pure frame composition, byte-budget fitting, link-to-health mapping, and status rules
//! (`docs/build/phases/P1.md` §3.4, `P1-10-frame.md`).

use std::collections::BTreeMap;
use zutalk_core::live::frame::{
    Caption, Column, Composer, Fit, Frame, KEEP_LINES, MAX_BYTES, MAX_COLUMNS, Status, fit, status,
};
use zutalk_core::live::lane::{Event, Link, Stream, plan};
use zutalk_core::live::lang::profile;
use zutalk_core::live::lines::Line;
use zutalk_core::rt::water::Health;

fn strings(of: &[&str]) -> Vec<String> {
    of.iter().map(|s| (*s).to_owned()).collect()
}

fn line(lang: &str, text: &str, t1: u32, trans: &[(&str, &str)]) -> Line {
    let translations: BTreeMap<String, String> =
        trans.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    Line {
        t0: t1.saturating_sub(500),
        t1,
        speaker: "1".into(),
        language: lang.into(),
        text: text.into(),
        translations,
        incomplete: false,
    }
}

#[test]
fn a_line_shows_in_a_column_once_it_is_in_that_columns_language() {
    let shown = strings(&["en", "zh", "th"]);
    let streams = plan(&shown);
    assert_eq!(streams.len(), 2);
    let mut c = Composer::new(&shown, streams);

    // 1. Two-way stream: finished English line carrying Chinese translation shows original in en,
    //    translation in zh.
    c.take(Event::Lines {
        stream: 0,
        lines: vec![line("en", "Good morning", 1_000, &[("zh", "早上好")])],
    });
    let (frame, _) = c.frame().expect("frame after finished line");
    assert_eq!(frame.t_audio, 1_000);
    assert_eq!(frame.columns[0].lines[0].text, "Good morning");
    assert_eq!(frame.columns[1].lines[0].text, "早上好");
    assert!(frame.columns[2].lines.is_empty());

    // 2. Translation has not arrived yet and line is still open: shown in its spoken column (en),
    //    absent in zh and th.
    c.take(Event::Tail { stream: 0, tail: vec![line("en", "Still speaking", 1_600, &[])] });
    c.take(Event::Tail { stream: 1, tail: vec![line("en", "Still speaking", 1_600, &[])] });
    let (frame, _) = c.frame().expect("frame after untranslated open tail");
    assert_eq!(frame.columns[0].tail.as_ref().map(|t| t.text.as_str()), Some("Still speaking"));
    assert!(frame.columns[1].tail.is_none(), "zh must wait while translation is still on the way");
    assert!(frame.columns[2].tail.is_none(), "th must wait while translation is still on the way");

    // 3. Once the untranslated line finishes, every column on its stream shows it as spoken.
    c.take(Event::Lines { stream: 0, lines: vec![line("en", "Untranslated", 2_000, &[])] });
    c.take(Event::Tail { stream: 0, tail: Vec::new() });
    let (frame, _) = c.frame().expect("frame after untranslated finished line");
    assert_eq!(frame.columns[0].lines.last().unwrap().text, "Untranslated");
    assert_eq!(frame.columns[1].lines.last().unwrap().text, "Untranslated");

    // 4. A transcribe-only stream shows open lines as spoken immediately, regardless of column tag.
    let single = strings(&["zh"]);
    let mut transcribe = Composer::new(&single, vec![Stream::Transcribe]);
    transcribe.take(Event::Tail { stream: 0, tail: vec![line("en", "Direct words", 500, &[])] });
    let (frame, _) = transcribe.frame().expect("transcribe frame");
    assert_eq!(frame.columns[0].tail.as_ref().map(|t| t.text.as_str()), Some("Direct words"));
}

#[test]
fn finished_lines_carry_no_units_and_the_tail_does() {
    let shown = strings(&["th"]);
    let mut c = Composer::new(&shown, plan(&shown));
    let thai = "ผมคิดว่าเราควรเริ่มใหม่";
    let expected_units = profile("th").units(thai);
    assert!(!expected_units.is_empty());

    c.take(Event::Tail { stream: 0, tail: vec![line("th", thai, 800, &[])] });
    let (frame, _) = c.frame().expect("tail frame");
    let tail = frame.columns[0].tail.as_ref().expect("tail present");
    assert_eq!(tail.text, thai);
    assert_eq!(tail.units, expected_units);

    c.take(Event::Lines { stream: 0, lines: vec![line("th", thai, 800, &[])] });
    c.take(Event::Tail { stream: 0, tail: Vec::new() });
    let (frame, _) = c.frame().expect("finished frame");
    assert!(frame.columns[0].tail.is_none());
    assert_eq!(frame.columns[0].lines.len(), 1);
    assert_eq!(frame.columns[0].lines[0].text, thai);
    assert!(frame.columns[0].lines[0].units.is_empty(), "finished lines must carry no units");
}

#[test]
fn a_column_keeps_its_last_six_lines() {
    let shown = strings(&["en"]);
    let mut c = Composer::new(&shown, plan(&shown));
    for i in 0..1_000u32 {
        c.take(Event::Lines {
            stream: 0,
            lines: vec![line("en", &format!("line-{i}"), (i + 1) * 100, &[])],
        });
    }
    let (frame, _) = c.frame().expect("frame after 1000 lines");
    let kept: Vec<&str> = frame.columns[0].lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(kept.len(), KEEP_LINES);
    assert_eq!(kept, ["line-994", "line-995", "line-996", "line-997", "line-998", "line-999"]);
}

#[test]
fn nothing_changed_no_frame() {
    let shown = strings(&["en"]);
    let mut c = Composer::new(&shown, plan(&shown));
    assert!(c.frame().is_some(), "initial state emits one frame");
    assert!(c.frame().is_none(), "second read with no new events must be None");

    c.take(Event::Tail { stream: 0, tail: vec![line("en", "hello", 400, &[])] });
    assert!(c.frame().is_some(), "incoming Tail emits a frame");
    assert!(c.frame().is_none(), "no further events means None");

    c.pause(true);
    assert!(c.frame().is_some(), "pausing emits a frame");
    assert!(c.frame().is_none(), "repeating without change returns None");
}

#[test]
fn a_frame_never_outgrows_its_budget() {
    let shown = strings(&["zh", "th", "en", "ja"]);
    let streams = plan(&shown);
    assert_eq!(streams.len(), 3);
    let mut c = Composer::new(&shown, streams);
    let _ = c.frame();

    let zh_150 = "汉".repeat(150);
    let th_150 = "ก".repeat(150);
    let en_150 = "word ".repeat(150);
    let ja_150 = "語".repeat(150);

    let zh_tail = "尾".repeat(600);
    let th_tail = "ท".repeat(600);
    let en_tail = "tail ".repeat(600);
    let ja_tail = "末".repeat(600);

    c.take(Event::Tail { stream: 0, tail: vec![line("zh", &zh_tail, 1_000, &[("th", &th_tail)])] });
    c.take(Event::Tail { stream: 1, tail: vec![line("zh", &zh_tail, 1_000, &[("en", &en_tail)])] });
    c.take(Event::Tail { stream: 2, tail: vec![line("zh", &zh_tail, 1_000, &[("ja", &ja_tail)])] });

    for i in 0..200u32 {
        let t1 = 2_000 + i * 500;
        c.take(Event::Lines {
            stream: 0,
            lines: vec![line("zh", &zh_150, t1, &[("th", &th_150)])],
        });
        c.take(Event::Lines {
            stream: 1,
            lines: vec![line("zh", &zh_150, t1, &[("en", &en_150)])],
        });
        c.take(Event::Lines {
            stream: 2,
            lines: vec![line("zh", &zh_150, t1, &[("ja", &ja_150)])],
        });

        let (frame, fit_kind) = c.frame().expect("frame after heavy batch");
        let encoded = serde_json::to_vec(&frame).expect("serialize frame");
        assert!(
            encoded.len() <= MAX_BYTES,
            "frame {i} grew to {} bytes > {MAX_BYTES} bytes",
            encoded.len()
        );
        assert_eq!(frame.columns.len(), 4);
        assert!(frame.columns.iter().all(|col| col.tail.is_some()), "every column keeps its tail");
        assert_ne!(fit_kind, Fit::Whole, "frame {i} must report trimming");
        assert!(c.bound_bytes() > MAX_BYTES);
    }
}

#[test]
fn what_gives_way_first_is_the_oldest_line_of_the_fullest_column() {
    assert_eq!(
        [Fit::Whole.code(), Fit::Lines.code(), Fit::Units.code(), Fit::Tail.code()],
        ["Whole", "Lines", "Units", "Tail"]
    );

    let cap =
        |text: &str, units: Vec<u16>| Caption { speaker: "1".into(), text: text.into(), units };
    let col = |lang: &str, lines: Vec<Caption>, tail: Option<Caption>| Column {
        language: lang.into(),
        rate: 4.0,
        lines,
        tail,
        health: Health::Live,
    };

    // Row 1: short frame returns unchanged with Fit::Whole.
    let small = Frame {
        t_audio: 500,
        columns: vec![col("en", vec![cap("hello", vec![])], Some(cap("world", vec![5])))],
    };
    let (out, state) = fit(small.clone());
    assert_eq!(state, Fit::Whole);
    assert_eq!(out, small);

    // Row 2: Fit::Lines drops the oldest line of the fullest column first, leaving lighter
    // columns and tail units intact.
    let heavy_lines: Vec<Caption> =
        (0..6).map(|i| cap(&format!("col1-{i}-{}", "x".repeat(1_500)), Vec::new())).collect();
    let lines_case = Frame {
        t_audio: 1_200,
        columns: vec![
            col("en", vec![cap("keep-col0", vec![])], Some(cap("tail0", vec![5]))),
            col("zh", heavy_lines, Some(cap("tail1", vec![5]))),
        ],
    };
    let (out, state) = fit(lines_case);
    assert_eq!(state, Fit::Lines);
    assert!(serde_json::to_vec(&out).unwrap().len() <= MAX_BYTES);
    assert_eq!(out.columns[0].lines.len(), 1);
    assert_eq!(out.columns[0].lines[0].text, "keep-col0");
    assert!(out.columns[1].lines.len() < 6);
    assert!(!out.columns[1].lines.iter().any(|l| l.text.starts_with("col1-0-")));
    assert!(out.columns[1].lines.last().unwrap().text.starts_with("col1-5-"));
    assert_eq!(out.columns[0].tail.as_ref().unwrap().units, vec![5]);
    assert_eq!(out.columns[1].tail.as_ref().unwrap().units, vec![5]);

    // Row 3: Fit::Units strips tail units when text alone fits within MAX_BYTES.
    let text_2000 = "字".repeat(2_000); // 6,000 UTF-8 bytes < 8,192
    let units_2000: Vec<u16> = (1..=2_000).collect(); // ~9,800 JSON bytes
    let units_case = Frame {
        t_audio: 2_000,
        columns: vec![col("zh", vec![cap("old-line", vec![])], Some(cap(&text_2000, units_2000)))],
    };
    let (out, state) = fit(units_case);
    assert_eq!(state, Fit::Units);
    assert!(serde_json::to_vec(&out).unwrap().len() <= MAX_BYTES);
    assert!(out.columns[0].lines.is_empty());
    let out_tail = out.columns[0].tail.as_ref().expect("tail kept");
    assert!(out_tail.units.is_empty());
    assert_eq!(out_tail.text, text_2000);

    // Row 4: Fit::Tail truncates the front of tail.text and prepends `…` while keeping latest words.
    let text_huge = format!("{}末尾保留", "长".repeat(4_000)); // > 12,000 UTF-8 bytes
    let tail_case = Frame {
        t_audio: 3_000,
        columns: vec![col("zh", vec![], Some(cap(&text_huge, vec![1, 2, 3])))],
    };
    let (out, state) = fit(tail_case);
    assert_eq!(state, Fit::Tail);
    assert!(serde_json::to_vec(&out).unwrap().len() <= MAX_BYTES);
    let out_tail = out.columns[0].tail.as_ref().expect("tail kept");
    assert!(out_tail.units.is_empty());
    assert!(out_tail.text.starts_with('…'), "truncated tail must open with `…`");
    assert!(out_tail.text.ends_with("末尾保留"), "truncated tail must keep latest words");
}

#[test]
fn a_columns_health_is_its_links() {
    let shown = strings(&["en"]);
    let mut c = Composer::new(&shown, plan(&shown));
    let _ = c.frame();

    let table = [
        (Link::Live, Health::Live),
        (Link::Behind, Health::Slow),
        (Link::Connecting, Health::Slow),
        (Link::Stopped("done"), Health::Live),
        (Link::Stopped("key"), Health::Dead { why: "key".into() }),
        (Link::Stopped("quota"), Health::Dead { why: "quota".into() }),
    ];
    for (link, expected) in table {
        c.take(Event::Link { stream: 0, state: link });
        let (frame, _) = c.frame().expect("frame after Link event");
        assert_eq!(frame.columns[0].health, expected, "link={link:?}");
    }
}

#[test]
fn the_status_line_says_what_most_needs_saying() {
    use Link::{Behind, Connecting, Live, Stopped};
    let only = |why: &str| Status::AudioOnly { why: why.into() };
    assert_eq!(
        [
            Status::Recording.code(),
            Status::Paused.code(),
            only("captions.off").code(),
            Status::Connecting.code(),
            Status::Behind.code(),
        ],
        ["Recording", "Paused", "AudioOnly", "Connecting", "Behind"]
    );
    assert_eq!(Status::Recording.why(), "");
    assert_eq!(only("captions.no_key").why(), "captions.no_key");
    assert_eq!(status(true, &[Connecting], "captions.off"), Status::Paused);
    assert_eq!(status(false, &[], "captions.off"), only("captions.off"));
    assert_eq!(status(false, &[], "captions.no_key"), only("captions.no_key"));
    assert_eq!(status(false, &[Stopped("keys.none"), Stopped("done")], ""), only("keys.none"));
    assert_eq!(status(false, &[Live, Connecting], ""), Status::Connecting);
    assert_eq!(status(false, &[Behind, Live], ""), Status::Behind);
    assert_eq!(status(false, &[Live, Stopped("key")], ""), Status::Recording);
}

#[test]
fn no_more_than_four_columns() {
    let six = strings(&["en", "zh", "th", "ja", "ko", "fr"]);
    let streams = plan(&six);
    assert_eq!(
        streams,
        [
            Stream::TwoWay("en".into(), "zh".into()),
            Stream::OneWay("th".into()),
            Stream::OneWay("ja".into()),
        ]
    );
    let mut c = Composer::new(&six, streams);
    let (frame, _) = c.frame().expect("initial frame");
    assert_eq!(frame.columns.len(), MAX_COLUMNS);
    let langs: Vec<&str> = frame.columns.iter().map(|col| col.language.as_str()).collect();
    assert_eq!(langs, ["en", "zh", "th", "ja"]);
}
