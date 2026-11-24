mod hub;

use hub::{App, Asked, STORE, config, shelf, signed_in, soniox, tape, wav};
use std::fs;
use std::path::Path;
use zutalk_core::ai::write::Journal;
use zutalk_core::app::proto::{Command, Row, Setup, State};
use zutalk_core::capture::clock::{BLOCK, Block, wall_ms};
use zutalk_core::capture::tape::{self as tapes, Pass, Writer};
use zutalk_core::live::lines::Line;
use zutalk_core::record::retain::goes;
use zutalk_core::record::shelf::{About, LINE, RECORDING, Said};

const DAY: i64 = 86_400_000;
const NOW: i64 = 1_800_000_000_000;

#[test]
fn audio_stays_until_the_record_is_written_then_as_long_as_the_person_chose() {
    #[rustfmt::skip]
    let table = [
        // keep,   written, age,           goes
        (None,     true,    4_000 * DAY,   false), // until the person destroys it
        (None,     false,   4_000 * DAY,   false),
        (Some(0),  true,    0,             true),  // once its record is written
        (Some(0),  false,   4_000 * DAY,   false), // never before
        (Some(7),  true,    7 * DAY - 1,   false),
        (Some(7),  true,    7 * DAY,       true),
        (Some(7),  false,   8 * DAY,       false),
        (Some(30), true,    29 * DAY,      false),
        (Some(30), true,    31 * DAY,      true),
        (Some(0),  true,    -DAY,          true),  // a clock set back: once written is still now
        (Some(7),  true,    -DAY,          false), // and a start in the future is no age at all
    ];
    for (keep, written, age, expected) in table {
        assert_eq!(goes(keep, written, NOW - age, NOW), expected, "{keep:?} {written} {age}");
    }
    // Whatever the clock says, nothing overflows.
    assert!(goes(Some(u32::MAX), true, i64::MIN, i64::MAX));
    assert!(!goes(Some(u32::MAX), true, i64::MAX, i64::MIN));
}

#[test]
fn destroying_a_tape_takes_every_minute_and_leaves_what_the_service_is_still_owed() {
    let (dir, key) = (tempfile::tempdir().expect("a folder"), [7; 32]);
    let tape = dir.path().join("kx7q2m5v4tbn3hpw");
    let fill = |tape: &Path| {
        let mut writer = Writer::create(tape, &key).expect("a tape");
        for t_ms in (0..61_000).step_by(100) {
            writer.append(&Block { t_ms, pcm: vec![300; BLOCK] }).expect("a block");
        }
    };
    fill(&tape);
    assert!(tapes::kept(&tape));
    tapes::destroy(&tape).expect("it is destroyed");
    assert!(!tapes::kept(&tape) && !tape.exists(), "two minutes, and the folder, gone");
    tapes::destroy(&tape).expect("gone is gone");
    // A write that left something at the service keeps its journal: the start-up sweep reads it.
    fill(&tape);
    let owed = vec!["files/file-1".to_string()];
    let journal = Journal { name: Some("zutalk-kx7q2m5v4tbn3hpw.wav".into()), owed, take: 0 };
    journal.save(&tape).expect("the journal is kept");
    tapes::destroy(&tape).expect("it is destroyed");
    assert!(!tapes::kept(&tape));
    assert_eq!(Journal::load(&tape).expect("it reads"), journal);
    assert_eq!(tapes::read(&tape, &key, &Pass::Write, |_| {}).expect("nothing to read"), 0);
}

/// A recording as the hub would have kept it: its record, a line of `take`, and its audio.
fn kept(data: &Path, id: &str, started: i64, take: u32) {
    let shelf = shelf(data);
    let line = Line { language: "en".into(), text: format!("Said in {id}."), ..Line::default() };
    let said = Said { line, via: [("text".to_string(), "hand".to_string())].into() };
    shelf.put(id, LINE, &[(format!("{id}.{take}.00000"), Some(said))], started).expect("a line");
    let about =
        About { started, length_ms: 1_000, spoken: vec!["en".into()], take, ..About::default() };
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], started).expect("its record");
    tape(data, id, 1);
}

fn audio(state: &State, id: &str) -> Option<bool> {
    state.library.iter().find(|row| row.id == id).map(|row| row.audio)
}

#[tokio::test(flavor = "multi_thread")]
async fn at_start_up_audio_the_person_let_go_is_destroyed_and_audio_not_yet_written_from_stays() {
    let _store = STORE.lock().await;
    signed_in();
    let dir = tempfile::tempdir().expect("a folder");
    let (data, now) = (dir.path(), wall_ms() as i64);
    kept(data, "oldwritten", now - 40 * DAY, 1);
    kept(data, "newwritten", now - 2 * DAY, 1);
    kept(data, "olddraft", now - 400 * DAY, 0);
    // Kept until destroyed, as the 0.8 app kept it: nothing goes.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 3).await;
    assert!(state.library.iter().all(|row| row.audio), "{:?}", state.library);
    assert_eq!(state.setup.keep, None);
    app.send(Command::Setup { setup: Setup { keep: Some(30), ..Setup::default() } });
    app.until(|state| state.setup.keep == Some(30)).await;
    drop(app);
    // Thirty days: at the next start the old written one's audio goes, and its text stays.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 3).await;
    assert_eq!(audio(&state, "oldwritten"), Some(false));
    assert_eq!(audio(&state, "newwritten"), Some(true));
    assert_eq!(audio(&state, "olddraft"), Some(true), "its record is not written yet");
    assert!(!data.join("tape/oldwritten").exists() && data.join("tape/olddraft").exists());
    app.send(Command::Open { id: "oldwritten".into() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Said in oldwritten.");
    assert!(!open.row.audio && !open.row.draft);
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_destroyed_at_the_persons_word_is_gone_for_good_and_its_text_stays() {
    let _store = STORE.lock().await;
    signed_in();
    let dir = tempfile::tempdir().expect("a folder");
    let (data, now) = (dir.path(), wall_ms() as i64);
    kept(data, "written", now, 1);
    kept(data, "draft", now, 0);
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.len() == 2).await;
    assert_eq!((audio(&state, "written"), audio(&state, "draft")), (Some(true), Some(true)));
    app.send(Command::DestroyAudio { id: "written".into() });
    app.until(|state| audio(state, "written") == Some(false)).await;
    assert!(!data.join("tape/written").exists());
    // A draft whose write left something at the service: the journal stays for the sweep.
    let owed = vec!["files/file-1".to_string()];
    let journal = Journal { name: Some("zutalk-draft.wav".into()), owed, take: 0 };
    journal.save(&data.join("tape/draft")).expect("the journal is kept");
    app.send(Command::DestroyAudio { id: "draft".into() });
    let state = app.until(|state| audio(state, "draft") == Some(false)).await;
    assert!(state.library.iter().all(|row| !row.audio));
    app.send(Command::DestroyAudio { id: "nowhere".into() });
    app.notice("recording.gone").await;
    // Not while it is being recorded.
    app.send(Command::Start);
    let id = app.session(|s| s.is_some()).await.expect("one").id;
    app.send(Command::DestroyAudio { id });
    app.notice("recording.under_way").await;
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    drop(app);
    // Started again, the draft without its audio is not written again: its text stays.
    let mut app = App::with(config(data));
    let state = app.until(|state| state.library.iter().any(|row| row.id == "draft")).await;
    let draft = state.library.iter().find(|row| row.id == "draft").expect("still there");
    assert!(draft.draft && !draft.audio);
    assert!(state.writing.iter().all(|w| w.id != "draft"), "{:?}", state.writing);
    app.send(Command::Open { id: "draft".into() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Said in draft.");
}

#[tokio::test(flavor = "multi_thread")]
async fn once_the_record_is_written_audio_kept_only_until_then_goes() {
    let _store = STORE.lock().await;
    signed_in();
    let (dir, asked) = (tempfile::tempdir().expect("a folder"), Asked::default());
    let stt = soniox(&asked).await;
    let data = dir.path();
    let mut app = App::with(zutalk_core::app::hub::Config {
        soniox: format!("{}/v1", stt.uri()),
        ..config(data)
    });
    app.until(|_| true).await;
    let setup = Setup { spoken: vec!["en".into()], keep: Some(0), ..Setup::default() };
    app.send(Command::Setup { setup });
    let file = data.join("Planning.wav");
    wav(&file, 1_500);
    app.send(Command::Import { path: file.to_string_lossy().into() });
    // Once its tape is sealed the audio stays until it is written; then it goes, and the
    // transcript is the text.
    let gone = |state: &State| {
        let kept = |row: &Row| row.audio || !row.draft || row.length_ms == 0;
        assert!(state.library.iter().all(kept), "{:?}", state.library);
        state.library.iter().any(|row| !row.draft && !row.audio)
    };
    let row = app.until(gone).await.library[0].clone();
    assert!(!data.join("tape").join(&row.id).exists());
    app.send(Command::Open { id: row.id.clone() });
    let open = app.until(|state| state.open.is_some()).await.open.expect("it opens");
    assert_eq!(open.lines[0].1.line.text, "Zuzalu is in Chiang Mai.");
    assert_eq!(asked.lock().expect("one at a time").len(), 1);
    assert!(fs::read_dir(data.join("tape")).expect("the folder").next().is_none());
}
