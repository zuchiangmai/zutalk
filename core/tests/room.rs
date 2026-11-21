//! The app's side of a room: frames sealed before they leave, opened only with the room's key,
//! written as the owner's signed records against the real server.

mod common;

use common::{Viewer, serve};
use std::sync::Arc;
use zu::SecretKey;
use zutalk_core::keys::{Me, Ours};
use zutalk_core::proto::{Open, Row};
use zutalk_core::room::{PAGE, Write, link, open, pages, seal, write};
use zutalk_core::shelf::Said;
use zutalk_core::supervise::End;

const KEY: [u8; 32] = [7; 32];

#[test]
fn a_frame_opens_with_its_key_only() {
    let frame = seal(&KEY, &[1; 12], b"said in the room").unwrap();
    assert_eq!(frame.len(), 12 + 16 + 16);
    assert_eq!(&frame[..12], &[1; 12]);
    assert_eq!(open(&KEY, &frame).unwrap(), b"said in the room");
    assert_ne!(seal(&KEY, &[2; 12], b"said in the room").unwrap(), frame);
    assert_eq!(open(&[8; 32], &frame), Err(End::Fatal("room.frame")));
    for at in [0, 12, frame.len() - 1] {
        let mut changed = frame.clone();
        changed[at] ^= 1;
        assert_eq!(open(&KEY, &changed), Err(End::Fatal("room.frame")), "byte {at}");
    }
    assert_eq!(open(&KEY, &frame[..11]), Err(End::Fatal("room.frame")));
    assert_eq!(open(&KEY, &seal(&KEY, &[3; 12], b"").unwrap()).unwrap(), b"");
}

#[test]
fn the_key_travels_only_after_the_hash() {
    let shared = link("https://talk.example", &"a".repeat(32), &KEY);
    let (url, fragment) = shared.split_once('#').unwrap();
    assert_eq!(url, format!("https://talk.example/r/{}", "a".repeat(32)));
    assert_eq!(fragment.len(), 43);
    assert_eq!(common::from_link(&shared).2, KEY);
}

fn recording(lines: usize, text: &str) -> Open {
    let said = |n: usize| {
        let mut said = Said::default();
        (said.line.t0, said.line.text) = (n as u32 * 1_000, format!("{n} {text}"));
        (format!("r.1.{n:05}"), said)
    };
    let row = Row {
        id: "r".into(),
        started: 0,
        length_ms: 0,
        title: "Budget".into(),
        preview: String::new(),
        spoken: vec!["en".into()],
        draft: false,
        audio: false,
    };
    Open {
        row,
        lines: (0..lines).map(said).collect(),
        speakers: Default::default(),
        note: String::new(),
        filed: None,
    }
}

#[test]
fn a_recording_goes_in_pages_that_each_say_what_it_is() {
    let long = recording(400, &"words ".repeat(40));
    let paged = pages(&long).unwrap();
    assert!(paged.len() > 2 && paged.iter().all(|page| page.len() <= PAGE), "{}", paged.len());
    let read: Vec<serde_json::Value> =
        paged.iter().map(|page| serde_json::from_slice(page).unwrap()).collect();
    assert!(read.iter().all(|page| page["row"]["title"] == "Budget"));
    // Put back together, the lines are all there, in order, once.
    let lines: Vec<&str> = read
        .iter()
        .flat_map(|page| page["lines"].as_array().unwrap())
        .map(|line| line[0].as_str().unwrap())
        .collect();
    assert_eq!(lines, long.lines.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>());
    // A recording with no text is one page; a line bigger than a page is a page of its own.
    assert_eq!(pages(&recording(0, "")).unwrap().len(), 1);
    assert_eq!(pages(&recording(2, &"x".repeat(PAGE))).unwrap().len(), 2);
}

fn ours(server: &str, n: u8) -> Ours {
    let me =
        Me { did: format!("did:zu:made-up-{n}"), key: Arc::new(SecretKey::from_seed(&[n; 32])) };
    Ours { server: server.into(), me, http: reqwest::Client::new() }
}

#[tokio::test(flavor = "multi_thread")]
async fn whoever_has_the_link_reads_what_the_owner_wrote_and_nobody_else_writes() {
    let (server, dir) = serve().await;
    let (owner, other, id) = (ours(&server, 1), ours(&server, 2), "c".repeat(32));
    write(&owner, &id, &KEY, Write::Make).await.unwrap();
    write(&owner, &id, &KEY, Write::Log(b"the first line")).await.unwrap();
    write(&owner, &id, &KEY, Write::Slot(Some(b"being said"))).await.unwrap();
    assert_eq!(
        write(&other, &id, &KEY, Write::Log(b"not theirs")).await,
        Err(End::Fatal("room.owner"))
    );
    let mut viewer = Viewer::open(&link(&server, &id, &KEY)).await;
    assert_eq!(viewer.next().await, Some(("log".into(), Ok(Some(b"the first line".to_vec())))));
    assert_eq!(viewer.next().await, Some(("slot".into(), Ok(Some(b"being said".to_vec())))));
    write(&owner, &id, &KEY, Write::Slot(None)).await.unwrap();
    assert_eq!(viewer.next().await, Some(("slot".into(), Ok(None))));
    // Without the key, nothing opens.
    let mut guess = Viewer::open(&link(&server, &id, &[0; 32])).await;
    assert_eq!(guess.next().await, Some(("log".into(), Err(()))));
    // No word of it is on the server's disk.
    let mut disk = Vec::new();
    for file in std::fs::read_dir(dir.path().join("rooms")).unwrap() {
        disk.extend(std::fs::read(file.unwrap().path()).unwrap());
    }
    assert!(!String::from_utf8_lossy(&disk).contains("line"));
    write(&owner, &id, &KEY, Write::Gone).await.unwrap();
    assert_eq!(viewer.next().await, Some(("gone".into(), Ok(None))));
    assert_eq!(write(&owner, &id, &KEY, Write::Log(b"after")).await, Err(End::Fatal("room.gone")));
    // A server that is not there is trouble that may pass.
    let away = ours("http://127.0.0.1:9", 1);
    assert_eq!(write(&away, &id, &KEY, Write::Make).await, Err(End::Retry("offline")));
}
