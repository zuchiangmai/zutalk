//! The 0.8 library, moved in. The fixture's audio and key file were written by the 0.8 code
//! itself (its recorder, its import path and its key store; see docs/migrate.md); its text is
//! made up. Nothing of anyone's is in it.

mod common;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Once};
use zu::{Pack, SecretKey};
use zutalk_core::capture::tape::{self, Pass};
use zutalk_core::id::keys;
use zutalk_core::record::migrate::{self, Handed, Progress};
use zutalk_core::record::shelf::{About, LINE, RECORDING, Shelf};

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const SOMEONE: &str = "did:zu:qq7hq3xbd2lvf5jy6tkmzbxe";
const LIVE: &str = "3b1f6c1e-8d7a-4c55-9a51-0c3f2f6b8a01"; // s16, 16 kHz, mono, two chunks
const IMPORTED: &str = "7e2d9a40-1b6c-4f3e-8e7d-5a9c1b2d3e02"; // f32, 48 kHz, stereo
const TEXT: &str = "c4a5b6d7-0e1f-4a2b-9c3d-4e5f6a7b8c03"; // its audio destroyed in 0.8
const NOW: i64 = 1_800_000_000_000;

/// The key store is the process's: every test here shares one, with the tape's secret set.
fn store() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a store"));
        keys::set("tape", Some(&[7; 32])).expect("the secret is kept");
    });
}

/// The 0.8 folder, copied where a test may change it, and an empty folder for this app.
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    store();
    (common::old_library(), tempfile::tempdir().unwrap())
}

fn shelf(data: &Path, me: &str) -> Shelf {
    let root = data.join("records").join(me.replace(':', "-"));
    Shelf::new(root, me, Arc::new(SecretKey::from_seed(&[5; 32])))
}

/// What a run said: what it could not move, or why it moved nothing.
type Said = Result<Vec<(String, String)>, &'static str>;

/// A way the 0.8 folder can be wrong, the recording it hurts, its title, and the code said.
type Case = (&'static str, fn(&Path), (&'static str, &'static str, &'static str));

fn run(old: &Path, data: &Path, me: &str) -> (Said, Vec<Progress>) {
    let mut told = Vec::new();
    let moved = migrate::run((old, data), &shelf(data, me), me, || NOW, |w| told.push(w));
    (moved, told)
}

fn handed(old: &Path, id: &str) -> Handed {
    let json = fs::read(old.join(format!("handover/recordings/{id}.json"))).unwrap();
    serde_json::from_slice(&json).unwrap()
}

/// Where a recording moved from 0.8 is here.
fn here(old: &Path, id: &str) -> String {
    handed(old, id).id()
}

/// The samples on a tape, and its length in ms.
fn tape(data: &Path, id: &str) -> (Vec<i16>, u32) {
    let (key, mut pcm) = (keys::derive("tape", id).unwrap(), Vec::new());
    let length =
        tape::read(&data.join("tape").join(id), &key, &Pass::Export, |block| pcm.extend(block.pcm));
    (pcm, length.expect("the tape reads"))
}

/// What the 0.8 recorder was given: a sawtooth that tells every sample from its neighbours.
fn saw(n: usize) -> i16 {
    ((n % 400) as i16 - 200) * 50
}

fn edit(path: &Path, change: impl FnOnce(String) -> String) {
    fs::write(path, change(fs::read_to_string(path).unwrap())).unwrap();
}

#[test]
fn the_old_library_comes_over_whole_signed_by_the_person() {
    let (old, data) = fixture();
    let (moved, told) = run(old.path(), data.path(), ME);
    assert_eq!(moved, Ok(vec![]));
    assert_eq!(told.iter().map(|w| (w.done, w.of)).collect::<Vec<_>>(), [(0, 3), (1, 3), (2, 3)]);
    assert!(told.iter().all(|w| w.step == "moving"));
    let shelf = shelf(data.path(), ME);
    let listed: Vec<String> = shelf.list().unwrap().into_iter().map(|r| r.about.title).collect();
    assert_eq!(listed, ["interview", "Weekly sync", ""], "the newest first, as ever");

    // A live recording: its live text is its text, as in 0.8, corrections and all.
    let id = here(old.path(), LIVE);
    assert_eq!(id.len(), 19);
    assert!(id.starts_with(&format!("{:011x}", 1_731_900_000_000_i64)), "shaped as ids here");
    let live = shelf.read(&id).unwrap().expect("it is here");
    let About { take, moved, length_ms, spoken, shown, ref preview, .. } = live.about;
    assert_eq!((take, moved, length_ms), (0, true, 2_500));
    assert_eq!((spoken, shown), (vec!["en".into(), "zh".into()], vec!["en".into(), "zh".into()]));
    assert!(preview.starts_with("Good morning, everyone. 预算是五千。"));
    let texts: Vec<&str> = live.lines.iter().map(|(_, s)| s.line.text.as_str()).collect();
    assert_eq!(texts, ["Good morning, everyone.", "预算是五千。", "Thank"]);
    let (key, corrected) = &live.lines[1];
    assert_eq!(key, &format!("{id}.0.00001"));
    assert_eq!(corrected.via["text"], "hand");
    assert_eq!(corrected.line.translations["en"], "The budget is 5,000.");
    assert!(live.lines[2].1.line.incomplete);
    assert_eq!(live.speakers["1"], "Host");
    assert_eq!(live.speakers["2"], "Guest");
    assert_eq!(live.note, "Agenda: the budget.");
    // Its sound, sealed again: the recorder's 1.5 s, the half second 0.8 no longer had as
    // silence, then the second chunk; the text still meets its sound.
    let (pcm, length) = tape(data.path(), &id);
    assert_eq!((length, pcm.len()), (2_500, 40_000));
    assert!(pcm[..24_000].iter().enumerate().all(|(n, s)| *s == saw(n)));
    assert!(pcm[24_000..32_000].iter().all(|s| *s == 0));
    assert!(pcm[32_000..].iter().enumerate().all(|(n, s)| *s == saw(n)));

    // An imported file: two channels at 48 kHz become one at 16 kHz. Its channels were each
    // other's opposite, so mixed they cancel.
    let id = here(old.path(), IMPORTED);
    let imported = shelf.read(&id).unwrap().expect("it is here");
    assert_eq!(imported.about.take, 1, "its whole-session transcript is its text");
    assert_eq!(imported.lines.len(), 1);
    assert_eq!(imported.lines[0].0, format!("{id}.1.00000"));
    let (pcm, length) = tape(data.path(), &id);
    assert!((300..=400).contains(&length), "{length} ms of 400");
    assert!(pcm.iter().all(|s| s.abs() <= 1), "the opposite channels cancel");
    assert_eq!(imported.about.length_ms, length, "a recording is as long as its tape");

    // Its audio destroyed in 0.8: its text comes, and there is no tape.
    let id = here(old.path(), TEXT);
    let text = shelf.read(&id).unwrap().expect("it is here");
    assert_eq!((text.about.take, text.about.length_ms), (1, 65_000));
    assert_eq!(text.lines[1].1.via["text"], "hand");
    assert!(!data.path().join("tape").join(&id).exists());
    let pack = data.path().join("records").join(ME.replace(':', "-")).join(&id);
    let lines = Pack::read(pack.join("lines.jsonl")).unwrap();
    assert_eq!(lines.records.len(), 3, "the live take is kept beside the one shown");

    // Every record is the person's, signed with their key.
    let public = SecretKey::from_seed(&[5; 32]).public();
    for record in lines.records.iter().chain(&Pack::read(pack.join("about.jsonl")).unwrap().records)
    {
        assert_eq!(record.path.by, ME);
        record.verify(|by, key, _| by == ME && *key == public, |_| None).expect("signed");
    }
}

#[test]
fn a_recording_moves_once_however_often_this_runs_even_once_it_is_removed() {
    let (old, data) = fixture();
    assert_eq!(run(old.path(), data.path(), ME).0, Ok(vec![]));
    let (shelf, id) = (shelf(data.path(), ME), here(old.path(), LIVE));
    shelf.put::<About>(&id, RECORDING, &[(id.clone(), None)], NOW).unwrap();
    let pack = data.path().join("records").join(ME.replace(':', "-")).join(&id);
    let before = fs::read(pack.join("lines.jsonl")).unwrap();
    let (moved, told) = run(old.path(), data.path(), ME);
    assert_eq!((moved, told), (Ok(vec![]), vec![]));
    assert!(shelf.about(&id).unwrap().is_none(), "removed, it stays removed");
    assert_eq!(fs::read(pack.join("lines.jsonl")).unwrap(), before);
    assert_eq!(shelf.list().unwrap().len(), 2);
}

#[test]
fn a_move_cut_short_is_done_again_from_the_start() {
    let (old, data) = fixture();
    let (shelf, id) = (shelf(data.path(), ME), here(old.path(), LIVE));
    // What a crash left: half a tape, and a line, but not the record that makes it a recording.
    let dir = data.path().join("tape").join(&id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("00000.tape"), [9; 5_000]).unwrap();
    let partial = handed(old.path(), LIVE).lines(&id).into_iter().take(1).collect::<Vec<_>>();
    shelf.put(&id, LINE, &partial, NOW).unwrap();
    assert!(shelf.about(&id).unwrap().is_none() && !shelf.known(&id).unwrap());

    assert_eq!(run(old.path(), data.path(), ME).0, Ok(vec![]));
    assert_eq!(tape(data.path(), &id).1, 2_500, "the tape is sealed anew, not added to");
    assert_eq!(shelf.read(&id).unwrap().expect("it is here").lines.len(), 3);
}

#[test]
fn what_cannot_move_is_said_with_a_code_and_the_rest_moves() {
    let cases: [Case; 6] = [
        (
            "a chunk is gone",
            |old| fs::remove_file(old.join(format!("audio/{LIVE}/chunk.00001.enc"))).unwrap(),
            (LIVE, "Weekly sync", "migrate.audio"),
        ),
        (
            "a chunk was altered",
            |old| {
                let path = old.join(format!("audio/{LIVE}/chunk.00000.enc"));
                let mut bytes = fs::read(&path).unwrap();
                bytes[40] ^= 1;
                fs::write(path, bytes).unwrap();
            },
            (LIVE, "Weekly sync", "migrate.audio"),
        ),
        (
            "a chunk was cut short",
            |old| {
                let path = old.join(format!("audio/{LIVE}/chunk.00001.enc"));
                let bytes = fs::read(&path).unwrap();
                fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
            },
            (LIVE, "Weekly sync", "migrate.audio"),
        ),
        (
            "a format this app does not know",
            |old| {
                edit(&old.join(format!("handover/recordings/{IMPORTED}.json")), |json| {
                    json.replace("\"f32\"", "\"f64\"")
                })
            },
            (IMPORTED, "interview", "migrate.audio"),
        ),
        (
            "its key is not in the key file",
            |old| {
                edit(&old.join("Secrets/content-keys.json"), |json| {
                    json.replace(&format!("zutalk.audio.{IMPORTED}"), "zutalk.audio.elsewhere")
                })
            },
            (IMPORTED, "interview", "migrate.key"),
        ),
        (
            "a speaker this app cannot name",
            |old| {
                edit(&old.join(format!("handover/recordings/{LIVE}.json")), |json| {
                    json.replace("\"2\": \"Guest\"", "\"a b\": \"Guest\"")
                })
            },
            (LIVE, "Weekly sync", "migrate.handover"),
        ),
    ];
    for (what, change, (failed, title, code)) in cases {
        let (old, data) = fixture();
        change(old.path());
        let (moved, _) = run(old.path(), data.path(), ME);
        assert_eq!(moved, Ok(vec![(title.to_string(), code.to_string())]), "{what}");
        let shelf = shelf(data.path(), ME);
        assert!(shelf.about(&here(old.path(), failed)).unwrap().is_none(), "{what}: not half");
        assert_eq!(shelf.list().unwrap().len(), 2, "{what}: the rest moved");
    }

    // A file that is no handover is said without a title; fixed, it moves at the next run.
    let (old, data) = fixture();
    let file = old.path().join(format!("handover/recordings/{TEXT}.json"));
    let kept = fs::read(&file).unwrap();
    fs::write(&file, b"{ \"version\": 2 }").unwrap();
    let (moved, _) = run(old.path(), data.path(), ME);
    assert_eq!(moved, Ok(vec![(String::new(), "migrate.handover".into())]));
    fs::write(&file, kept).unwrap();
    let (moved, told) = run(old.path(), data.path(), ME);
    assert_eq!((moved, told.len()), (Ok(vec![]), 1));
    assert_eq!(shelf(data.path(), ME).list().unwrap().len(), 3);

    // An index this version cannot read moves nothing.
    let index = old.path().join("handover/index.json");
    edit(&index, |json| json.replace("\"version\": 1", "\"version\": 2"));
    assert_eq!(run(old.path(), data.path(), ME).0, Err("migrate.handover"));
}

#[test]
fn a_library_not_handed_over_is_said_and_it_goes_to_one_person() {
    let (old, data) = fixture();
    fs::remove_dir_all(old.path().join("handover")).unwrap();
    assert_eq!(run(old.path(), data.path(), ME).0, Ok(vec![]), "no 0.8 library: nothing to say");
    fs::write(old.path().join("zutalk.db"), b"").unwrap();
    assert_eq!(run(old.path(), data.path(), ME).0, Err("migrate.missing"));

    let (old, data) = fixture();
    assert_eq!(run(old.path(), data.path(), ME).0, Ok(vec![]));
    let (moved, told) = run(old.path(), data.path(), SOMEONE);
    assert_eq!((moved, told), (Ok(vec![]), vec![]));
    assert!(shelf(data.path(), SOMEONE).list().unwrap().is_empty());
}

#[test]
fn safe_paths_and_unmoved_cap_rules() {
    let name_cases = [
        ("3b1f6c1e-8d7a-4c55-9a51-0c3f2f6b8a01", true),
        ("rec_001.v1", true),
        ("", false),
        (".", false),
        ("..", false),
        ("../escape", false),
        ("nested/name", false),
        ("nested\\name", false),
        ("/abs", false),
        ("name/", false),
    ];
    for (name, ok) in name_cases {
        assert_eq!(migrate::safe_recording_name(name), ok, "safe_recording_name({name:?})");
    }

    let path_cases = [
        ("audio/rec1/chunk.00000.enc", true),
        ("chunk.00000.enc", true),
        ("", false),
        (".", false),
        ("..", false),
        ("../secret.enc", false),
        ("audio/../secret.enc", false),
        ("audio/./chunk.00000.enc", false),
        ("/etc/passwd", false),
        ("audio\\rec1\\chunk.00000.enc", false),
        ("audio/rec1/", false),
        ("audio//chunk.00000.enc", false),
    ];
    for (path, ok) in path_cases {
        assert_eq!(migrate::safe_chunk_path(path), ok, "safe_chunk_path({path:?})");
    }

    assert!(migrate::cap_unmoved(vec![]).is_empty());
    let small: Vec<(String, String)> =
        (0..5).map(|i| (format!("t{i}"), "migrate.audio".into())).collect();
    assert_eq!(migrate::cap_unmoved(small.clone()), small);

    let exact: Vec<(String, String)> = (0..migrate::STATE_UNMOVED_MAX_ROWS)
        .map(|i| (format!("t{i}"), "migrate.handover".into()))
        .collect();
    assert_eq!(migrate::cap_unmoved(exact.clone()).len(), migrate::STATE_UNMOVED_MAX_ROWS);

    let huge: Vec<(String, String)> = (0..migrate::STATE_UNMOVED_MAX_ROWS + 18)
        .map(|i| (format!("t{i}"), "migrate.handover".into()))
        .collect();
    let capped = migrate::cap_unmoved(huge.clone());
    assert_eq!(capped.len(), migrate::STATE_UNMOVED_MAX_ROWS);
    assert_eq!(capped, huge[..migrate::STATE_UNMOVED_MAX_ROWS]);
}

#[test]
fn corrupt_audio_edge_cases_are_rejected_without_oom() {
    type AudioCase = (&'static str, fn(&Path));
    let cases: [AudioCase; 3] = [
        ("empty chunks list", |old| {
            let path = old.join(format!("handover/recordings/{LIVE}.json"));
            let mut v: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            v["audio"]["chunks"] = serde_json::json!([]);
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        }),
        ("chunk path traversal outside old dir", |old| {
            let path = old.join(format!("handover/recordings/{LIVE}.json"));
            let mut v: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            v["audio"]["chunks"][0]["path"] = serde_json::json!("../secret.enc");
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        }),
        ("t_ms exceeding MIGRATE_MAX_AUDIO_MS", |old| {
            let path = old.join(format!("handover/recordings/{LIVE}.json"));
            let mut v: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let over = (migrate::MIGRATE_MAX_AUDIO_MS as u32) + 1_000;
            v["audio"]["chunks"][1]["t_ms"] = serde_json::json!(over);
            fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        }),
    ];

    for (what, mutate) in cases {
        let (old, data) = fixture();
        mutate(old.path());
        let id = here(old.path(), LIVE);
        let (moved, _) = run(old.path(), data.path(), ME);
        assert_eq!(moved, Ok(vec![("Weekly sync".into(), "migrate.audio".into())]), "{what}");
        let s = shelf(data.path(), ME);
        assert!(s.about(&id).unwrap().is_none(), "{what}: recording must not be shelved");
        assert!(!data.path().join("tape").join(&id).exists(), "{what}: no residual tape dir");
        assert_eq!(s.list().unwrap().len(), 2, "{what}: other recordings still moved");
    }
}

#[test]
fn run_report_counts_moved_already_and_skipped_accurately() {
    let (old, data) = fixture();
    fs::remove_file(old.path().join(format!("audio/{LIVE}/chunk.00001.enc"))).unwrap();

    let s = shelf(data.path(), ME);
    let first = migrate::run_report((old.path(), data.path()), &s, ME, || NOW, |_| {})
        .expect("first run succeeds with partial skip");
    assert_eq!(
        first,
        migrate::MoveReport {
            moved: 2,
            already: 0,
            skipped: vec![("Weekly sync".into(), "migrate.audio".into())],
        }
    );

    // moved.json written atomically with no leftover temp sibling.
    assert!(data.path().join("moved.json").exists());
    assert!(!data.path().join("moved.json.tmp").exists());
    let owner: serde_json::Value =
        serde_json::from_slice(&fs::read(data.path().join("moved.json")).unwrap()).unwrap();
    assert_eq!(owner["to"], ME);

    let second = migrate::run_report((old.path(), data.path()), &s, ME, || NOW, |_| {})
        .expect("second run is idempotent");
    assert_eq!(
        second,
        migrate::MoveReport {
            moved: 0,
            already: 2,
            skipped: vec![("Weekly sync".into(), "migrate.audio".into())],
        }
    );

    // Removing a migrated recording leaves a tombstone that still counts as already known.
    let imported_id = here(old.path(), IMPORTED);
    s.put::<About>(&imported_id, RECORDING, &[(imported_id.clone(), None)], NOW).unwrap();
    let third = migrate::run_report((old.path(), data.path()), &s, ME, || NOW, |_| {})
        .expect("third run does not resurrect removed recording");
    assert_eq!(third.moved, 0);
    assert_eq!(third.already, 2);
    assert_eq!(third.skipped, vec![("Weekly sync".into(), "migrate.audio".into())]);
    assert!(s.about(&imported_id).unwrap().is_none());
}
