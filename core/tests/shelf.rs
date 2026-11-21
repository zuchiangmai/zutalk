use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use zu::{Pack, SecretKey};
use zutalk_core::lines::Line;
use zutalk_core::shelf::{About, LINE, NOTE, RECORDING, SHARE, SPEAKER, Said, Share, Shelf, Words};

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const ID: &str = "kx7q2m5v4tbn3hpw";
const NOW: i64 = 1_800_000_000_000;

fn shelf(dir: &Path) -> Shelf {
    Shelf::new(dir, ME, Arc::new(SecretKey::from_seed(&[5; 32])))
}

fn owned(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn said(t0: u32, language: &str, text: &str, translations: &[(&str, &str)]) -> Said {
    let line = Line {
        t0,
        t1: t0 + 2_000,
        speaker: "1".into(),
        language: language.into(),
        text: text.into(),
        translations: owned(translations),
        incomplete: false,
    };
    let mut via = owned(&[("text", "stt-async")]);
    via.extend(translations.iter().map(|(lang, _)| (lang.to_string(), "a model".to_string())));
    Said { line, via }
}

fn about(started: i64, take: u32) -> About {
    About {
        started,
        length_ms: 90_000,
        spoken: vec!["zh".into(), "en".into()],
        take,
        ..About::default()
    }
}

fn meeting() -> Vec<Said> {
    vec![
        said(1_000, "zh", "这三个案例有什么区别？", &[("en", "How do these three cases differ?")]),
        said(4_000, "en", "The first one ships in March.", &[("zh", "第一个三月发货。")]),
    ]
}

/// A recording's lines under one take, then the record that says which take is its text.
fn write(shelf: &Shelf, id: &str, started: i64, take: u32, lines: &[Said]) {
    let key = |n: usize| format!("{id}.{take}.{n:05}");
    let keyed: Vec<_> = lines.iter().enumerate().map(|(n, s)| (key(n), Some(s.clone()))).collect();
    shelf.put(id, LINE, &keyed, NOW).unwrap();
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about(started, take)))], NOW).unwrap();
}

fn texts(shelf: &Shelf, id: &str) -> Vec<String> {
    let recording = shelf.read(id).unwrap().unwrap();
    recording.lines.into_iter().map(|(_, said)| said.line.text).collect()
}

#[test]
fn what_was_written_reads_back_and_any_tool_can_read_the_files() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());

    let recording = shelf.read(ID).unwrap().unwrap();
    assert_eq!(recording.about, about(NOW, 1));
    let lines: Vec<Said> = recording.lines.into_iter().map(|(_, said)| said).collect();
    assert_eq!(lines, meeting());

    let on_disk = fs::read_to_string(dir.path().join(ID).join("lines.jsonl")).unwrap();
    assert!(on_disk.contains("The first one ships in March."));
}

#[test]
fn every_record_is_signed_by_the_person() {
    let dir = tempfile::tempdir().unwrap();
    write(&shelf(dir.path()), ID, NOW, 1, &meeting());
    let key = SecretKey::from_seed(&[5; 32]).public();
    for pack in ["lines.jsonl", "about.jsonl"] {
        let held = Pack::read(dir.path().join(ID).join(pack)).unwrap();
        assert!(!held.records.is_empty());
        for record in &held.records {
            assert_eq!(record.verify(|by, signer, _| by == ME && *signer == key, |_| None), Ok(()));
        }
    }
}

#[test]
fn a_correction_is_one_more_record_and_what_it_replaced_stays() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());

    let mut fixed = meeting()[1].clone();
    fixed.line.text = "The first one ships in May.".into();
    fixed.via.insert("text".into(), "hand".into());
    shelf.put(ID, LINE, &[(format!("{ID}.1.00001"), Some(fixed.clone()))], NOW + 60_000).unwrap();

    let recording = shelf.read(ID).unwrap().unwrap();
    assert_eq!(recording.lines[1].1, fixed);
    assert_eq!(recording.lines.len(), 2);
    let on_disk = fs::read_to_string(dir.path().join(ID).join("lines.jsonl")).unwrap();
    assert!(on_disk.contains("ships in March") && on_disk.contains("ships in May"));
}

#[test]
fn a_change_made_by_a_slow_clock_still_follows_what_it_changes() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());
    let titled = About { title: "供货谈判".into(), ..about(NOW, 1) };
    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(titled.clone()))], NOW - 3_600_000).unwrap();
    assert_eq!(shelf.read(ID).unwrap().unwrap().about, titled);
}

#[test]
fn a_speaker_gets_a_name_and_the_recording_a_note() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());
    let words = |text: &str| Some(Words { text: text.into() });
    shelf.put(ID, SPEAKER, &[(format!("{ID}.1"), words("王总"))], NOW).unwrap();
    shelf.put(ID, NOTE, &[(ID.to_string(), words("- 三月发货\n- 账期 60 天"))], NOW).unwrap();

    let recording = shelf.read(ID).unwrap().unwrap();
    assert_eq!(recording.speakers.get("1").map(String::as_str), Some("王总"));
    assert_eq!(recording.note, "- 三月发货\n- 账期 60 天");
}

#[test]
fn removing_hides_a_recording_and_putting_it_back_undoes_that() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());

    shelf.put::<About>(ID, RECORDING, &[(ID.to_string(), None)], NOW + 1_000).unwrap();
    assert_eq!(shelf.read(ID).unwrap(), None);
    assert!(shelf.list().unwrap().is_empty());

    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(about(NOW, 1)))], NOW + 2_000).unwrap();
    assert_eq!(texts(&shelf, ID).len(), 2);
}

#[test]
fn a_later_take_becomes_the_text_and_the_draft_stays_in_the_pack() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    let draft = [said(1_000, "zh", "这三个案例", &[]), said(3_000, "zh", "有什么区别", &[])];
    write(&shelf, ID, NOW, 0, &draft);
    assert_eq!(texts(&shelf, ID), ["这三个案例", "有什么区别"]);

    write(&shelf, ID, NOW, 1, &meeting()[..1]);
    assert_eq!(texts(&shelf, ID), ["这三个案例有什么区别？"]);
    let on_disk = fs::read_to_string(dir.path().join(ID).join("lines.jsonl")).unwrap();
    assert!(on_disk.contains("有什么区别\""));
}

#[test]
fn the_list_is_newest_first_and_made_of_recordings_only() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, "older", NOW - 86_400_000, 1, &meeting());
    write(&shelf, "newer", NOW, 1, &meeting());
    fs::write(dir.path().join(".DS_Store"), b"not a recording").unwrap();
    fs::create_dir(dir.path().join("empty")).unwrap();

    let ids: Vec<String> = shelf.list().unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["newer", "older"]);
}

#[test]
fn search_reads_what_was_said_and_every_translation() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    let thai = said(7_000, "th", "ราคานี้รวมภาษีแล้ว น\u{e49}\u{e33}หนัก 5 กิโล", &[]);
    write(&shelf, ID, NOW, 1, &[meeting(), vec![thai]].concat());

    let keys = |query: &str| -> Vec<String> {
        shelf.search(query).unwrap().into_iter().map(|(_, key)| key).collect()
    };
    assert_eq!(keys("SHIPS in march"), [format!("{ID}.1.00001")]);
    assert_eq!(keys("三月发货"), [format!("{ID}.1.00001")]);
    // Full-width typing finds half-width text, and SARA AM matches however it was typed.
    assert_eq!(keys("Ｍａｒｃｈ"), [format!("{ID}.1.00001")]);
    assert_eq!(keys("น\u{e4d}\u{e49}\u{e32}หนัก"), [format!("{ID}.1.00002")]);
    assert!(keys("nothing like this").is_empty());
}

#[test]
fn restoring_brings_back_what_the_recording_said_last() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());
    let titled = About { title: "供货谈判".into(), ..about(NOW, 1) };
    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(titled.clone()))], NOW + 1_000).unwrap();
    shelf.put::<About>(ID, RECORDING, &[(ID.to_string(), None)], NOW + 2_000).unwrap();
    assert_eq!(shelf.read(ID).unwrap(), None);

    shelf.restore(ID, NOW + 3_000).unwrap();
    assert_eq!(shelf.read(ID).unwrap().unwrap().about, titled);
}

#[test]
fn the_live_draft_is_kept_line_by_line_and_reads_back_as_take_0() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(about(NOW, 0)))], NOW).unwrap();
    let mut draft = shelf.open(ID, LINE).unwrap();
    let lines = meeting();
    draft.put(&format!("{ID}.0.00000"), Some(&lines[0]), NOW).unwrap();
    // Each line is in the pack as it is said, before the draft is closed: a crash keeps it.
    assert_eq!(texts(&shelf, ID), ["这三个案例有什么区别？"]);
    draft.put(&format!("{ID}.0.00001"), Some(&lines[1]), NOW + 1).unwrap();
    draft.sync().unwrap();
    drop(draft);
    assert_eq!(texts(&shelf, ID), ["这三个案例有什么区别？", "The first one ships in March."]);
    // Nothing was read back to add a line: every record is new, and none follows another.
    let held = Pack::read(dir.path().join(ID).join("lines.jsonl")).unwrap();
    assert!(held.records.iter().all(|record| record.prev.is_empty()));
}

#[test]
fn a_writer_follows_what_a_path_says_like_one_change_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    let mut writer = shelf.open(ID, NOTE).unwrap();
    writer.put(ID, Some(&Words { text: "first".into() }), NOW).unwrap();
    // Later than the record it follows even when the clock says otherwise.
    writer.put(ID, Some(&Words { text: "second".into() }), NOW - 5_000).unwrap();
    writer.sync().unwrap();
    drop(writer);
    let held = Pack::read(dir.path().join(ID).join("about.jsonl")).unwrap();
    assert_eq!(held.records[1].prev, [held.records[0].cid()]);
    assert!(held.records[1].at > held.records[0].at);
    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(about(NOW, 0)))], NOW).unwrap();
    assert_eq!(shelf.about(ID).unwrap().unwrap().note, "second");
}

#[test]
fn the_rooms_a_person_has_are_records_beside_the_recordings() {
    let dir = tempfile::tempdir().unwrap();
    let shelf = shelf(dir.path());
    write(&shelf, ID, NOW, 1, &meeting());
    let (live, one) = (Share { recording: None }, Share { recording: Some(ID.into()) });
    shelf.put("", SHARE, &[("a".repeat(32), Some(live.clone()))], NOW).unwrap();
    shelf.put("", SHARE, &[("b".repeat(32), Some(one))], NOW).unwrap();
    shelf.put::<Share>("", SHARE, &[("b".repeat(32), None)], NOW + 1).unwrap();
    assert_eq!(shelf.kept::<Share>(SHARE).unwrap(), [("a".repeat(32), live)]);
    // They are not a recording.
    assert_eq!(shelf.list().unwrap().len(), 1);
}
