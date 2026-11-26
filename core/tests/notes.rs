mod hub;

use hub::{App, ME, STORE, local_shelf, nobody};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use zu::SecretKey;
use zutalk_core::app::proto::{Command, NoteOp};
use zutalk_core::id::keys::Me;
use zutalk_core::out::city::{self, Filing};
use zutalk_core::record::notes::{self, Note};
use zutalk_core::record::shelf::{About, NOTE, RECORDING, Shelf, Words};

const ID: &str = "kx7q2m5v4tbn3hpw";
const OTHER_ID: &str = "kx7q2m5v4tbn3hpx";
const NOW: i64 = 1_800_000_000_000;

fn make_shelf(dir: &Path) -> Shelf {
    Shelf::new(dir, ME, Arc::new(SecretKey::from_seed(&[5; 32])))
}

fn seed_recording(shelf: &Shelf, id: &str) {
    let about =
        About { started: NOW, length_ms: 60_000, title: "Planning".into(), ..About::default() };
    shelf.put(id, RECORDING, &[(id.to_string(), Some(about))], NOW).expect("recording written");
}

#[test]
fn timestamped_notes_add_edit_remove_and_bare_mark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = make_shelf(dir.path());
    seed_recording(&shelf, ID);
    seed_recording(&shelf, OTHER_ID);

    let anchor_a = Some(format!("{ID}.0.00003"));
    let anchor_mark = Some(format!("{ID}.0.00001"));

    // Add out of time order plus a bare mark (empty text, DECISIONS.md #2).
    let later = notes::add(
        &shelf,
        ID,
        45_000,
        "Follow up on pricing".into(),
        anchor_a.clone(),
        0x00ab_cdef,
        NOW + 1,
    )
    .expect("add later note");
    let mark =
        notes::add(&shelf, ID, 12_000, String::new(), anchor_mark.clone(), 0x0000_0002, NOW + 2)
            .expect("add bare mark");
    let same_time_earlier =
        notes::add(&shelf, ID, 12_000, "First at 12s".into(), None, 0x0000_0001, NOW + 3)
            .expect("add tie-breaker note");
    let other = notes::add(&shelf, OTHER_ID, 5_000, "Other recording".into(), None, 0x1, NOW + 4)
        .expect("add other recording note");

    assert_eq!(later.key, notes::make_key(ID, 45_000, 0x00ab_cdef));
    assert!(notes::belongs_to(ID, &later.key));
    assert!(!notes::belongs_to(ID, &other.key));

    // Sorted by (at_ms, key); bare mark is preserved.
    let list = notes::read(&shelf, ID).expect("read notes");
    assert_eq!(list, vec![same_time_earlier.clone(), mark.clone(), later.clone()]);

    // Edit preserves original at_ms and anchor while updating text.
    let edited = notes::edit(&shelf, ID, &later.key, "Follow up on Q3 pricing".into(), NOW + 10)
        .expect("edit note");
    assert_eq!(
        edited,
        Note {
            key: later.key.clone(),
            at_ms: 45_000,
            text: "Follow up on Q3 pricing".into(),
            anchor: anchor_a,
        }
    );
    let after_edit = notes::read(&shelf, ID).expect("read after edit");
    assert_eq!(after_edit[2], edited);

    // Bad keys and cross-recording keys fail with "note.gone".
    assert_eq!(
        notes::edit(&shelf, ID, &format!("{ID}.0000000000.deadbeef"), "x".into(), NOW + 11),
        Err("note.gone")
    );
    assert_eq!(notes::edit(&shelf, ID, &other.key, "x".into(), NOW + 12), Err("note.gone"));
    assert_eq!(notes::remove(&shelf, ID, &other.key, NOW + 13), Err("note.gone"));
    assert_eq!(notes::remove(&shelf, ID, "not-a-key", NOW + 14), Err("note.gone"));

    // Remove writes a tombstone and hides the note.
    notes::remove(&shelf, ID, &mark.key, NOW + 20).expect("remove mark");
    let after_remove = notes::read(&shelf, ID).expect("read after remove");
    assert_eq!(after_remove, vec![same_time_earlier, edited]);
    assert_eq!(notes::remove(&shelf, ID, &mark.key, NOW + 21), Err("note.gone"));
}

#[test]
fn legacy_single_string_note_reads_alongside_timestamped_notes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shelf = make_shelf(dir.path());
    seed_recording(&shelf, ID);

    // Legacy single-string note at key `<id>` with body `Words { text }`.
    shelf
        .put(ID, NOTE, &[(ID.to_string(), Some(Words { text: "Legacy summary".into() }))], NOW + 1)
        .expect("write legacy note");
    let added = notes::add(&shelf, ID, 8_000, "Point at 8s".into(), None, 0x42, NOW + 2)
        .expect("add timestamped note");

    let list = notes::read(&shelf, ID).expect("read combined notes");
    assert_eq!(
        list,
        vec![
            Note { key: ID.to_string(), at_ms: 0, text: "Legacy summary".into(), anchor: None },
            added.clone(),
        ]
    );

    let about = shelf.about(ID).expect("about").expect("recording present");
    assert_eq!(about.note, "Legacy summary");
    assert_eq!(about.notes, list);

    // Clearing the legacy single-string note to "" omits it from `notes` while keeping timestamped marks.
    shelf
        .put(ID, NOTE, &[(ID.to_string(), Some(Words { text: String::new() }))], NOW + 3)
        .expect("clear legacy note");
    let bare_mark =
        notes::add(&shelf, ID, 15_000, String::new(), None, 0x99, NOW + 4).expect("add bare mark");
    let after_clear = notes::read(&shelf, ID).expect("read after clear");
    assert_eq!(after_clear, vec![added, bare_mark]);
}

#[tokio::test(flavor = "multi_thread")]
async fn notes_work_during_live_recording_and_never_leave_to_city() {
    let _store = STORE.lock().await;
    nobody();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut app = App::start(dir.path());
    app.until(|_| true).await;

    app.send(Command::Start);
    let session = app.session(|s| s.is_some()).await.expect("recording session started");
    let rec_id = session.id.clone();

    // Wait until the recording has finished Opening (about.jsonl synced and listed) while still live.
    let shelf = local_shelf(dir.path());
    app.wait_library(|rows| rows.iter().any(|r| r.id == rec_id)).await;
    assert!((*app.outlet.session.now().1).as_ref().is_some_and(|s| s.id == rec_id));

    // Add both a timestamped note and a bare mark while recording is live (no sign-in required).
    app.send(Command::Note {
        id: rec_id.clone(),
        op: NoteOp::Add {
            at_ms: 1_500,
            text: "Private live note".into(),
            anchor: Some(format!("{rec_id}.0.00000")),
        },
    });
    app.send(Command::Note {
        id: rec_id.clone(),
        op: NoteOp::Add { at_ms: 2_500, text: String::new(), anchor: None },
    });

    // Wait for both notes to land on disk while still recording.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let current = notes::read(&shelf, &rec_id).expect("read live notes");
        if current.len() == 2 {
            assert_eq!(current[0].at_ms, 1_500);
            assert_eq!(current[0].text, "Private live note");
            assert_eq!(current[0].anchor.as_deref(), Some(format!("{rec_id}.0.00000").as_str()));
            assert_eq!(current[1].at_ms, 2_500);
            assert_eq!(current[1].text, "");
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live notes did not land; current={current:?}; events={:?}",
            app.seen.all()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Stop recording and wait for seal.
    app.send(Command::Stop);
    app.session(|s| s.is_none()).await;
    app.wait_library(|rows| rows.iter().any(|r| r.id == rec_id)).await;
    app.wait_progress(|p| p.refine.is_none()).await;

    let recorded = shelf.read(&rec_id).expect("read recorded").expect("recording exists");
    assert_eq!(recorded.notes.len(), 2);

    // City payload must never include any `talk/note` record or note text.
    let me = Me { did: ME.into(), key: Arc::new(SecretKey::from_seed(&[5; 32])) };
    let filing = Filing {
        city: "city.example".into(),
        did: "did:zu:4seascity4seascity4seas".into(),
        event: "zu://did:zu:organizer/ev/event/e1".into(),
        title: "Town hall".into(),
        audio: false,
        key: "a".repeat(32),
        state: String::new(),
    };
    let held = city::records(&me, &filing, &recorded, &[], NOW).expect("city records built");
    assert!(held.records.iter().all(|r| r.path.ty != NOTE));
    for body in held.bodies.values() {
        let json = body.to_json().expect("body json");
        assert!(!json.contains("Private live note"), "note text leaked into city payload: {json}");
    }
    let _ = BTreeMap::<String, String>::new();
}
