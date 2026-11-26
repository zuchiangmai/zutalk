//! pack: the append-only file of records, one per line as JSON text, and what a crash leaves.

mod common;

use common::{AT, cid, header, map, record, seal, secret, text};
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use zu::{Error, Pack, PublicKey, Record, Value};

/// The records of a pack; what else it holds has tests of its own below.
fn read(path: impl AsRef<std::path::Path>) -> Result<Vec<Record>, Error> {
    Pack::read(path).map(|held| held.records)
}

fn open(path: impl AsRef<std::path::Path>) -> Result<(Pack, Vec<Record>), Error> {
    Pack::open(path).map(|(pack, held)| (pack, held.records))
}

fn fresh() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.jsonl");
    (dir, path)
}

fn chain(len: usize) -> Vec<Record> {
    let mut records: Vec<Record> = Vec::new();
    for n in 0..len {
        let prev: Vec<&Record> = records.last().into_iter().collect();
        records.push(record("k", AT + n as i64, &prev));
    }
    records
}

/// A pack with `records` in it, closed.
fn written(records: &[Record]) -> (tempfile::TempDir, PathBuf) {
    let (dir, path) = fresh();
    let (mut pack, held) = open(&path).unwrap();
    assert!(held.is_empty());
    records.iter().for_each(|record| pack.append(record).unwrap());
    pack.sync().unwrap();
    (dir, path)
}

#[test]
fn a_new_pack_is_an_empty_file() {
    let (_dir, path) = fresh();
    assert_eq!(read(&path), Err(Error::Io(ErrorKind::NotFound)));
    let (pack, held) = open(&path).unwrap();
    assert!(held.is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert_eq!(read(&path), Ok(vec![]));
    drop(pack);
    // An empty file that was already there is a pack with no records.
    let (_pack, held) = open(&path).unwrap();
    assert!(held.is_empty());
    // Where no file can be made, the error says so.
    assert_eq!(open(path.join("below-a-file")).unwrap_err(), Error::Io(ErrorKind::NotADirectory));
}

#[test]
fn records_are_lines_of_json_that_read_back_the_same() {
    let records = chain(5);
    let (_dir, path) = written(&records);
    assert_eq!(read(&path).unwrap(), records);
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 5);
    assert!(text.ends_with('\n'));
    for (line, record) in text.lines().zip(&records) {
        // Searchable with any tool, and identical in DRISL once read back.
        assert!(line.contains(r#""$type":"record""#) && line.contains(r#""key":"k""#), "{line}");
        assert!(line.contains(&format!(r#""pub":"{}""#, secret(1).public())));
        assert_eq!(Value::from_json(line).unwrap().encode(), record.header().encode());
    }
    // Opened again, it gives the same records and goes on where it stopped.
    let (mut pack, held) = open(&path).unwrap();
    assert_eq!(held, records);
    let next = record("k", AT + 5, &[&records[4]]);
    pack.append(&next).unwrap();
    drop(pack);
    assert_eq!(read(&path).unwrap(), [records, vec![next]].concat());
    // What is read was checked for its structure; signatures are the caller's to check.
    let anyone = |_: &str, _: &PublicKey, _: i64| true;
    assert!(read(&path).unwrap().iter().all(|r| r.verify(anyone, |_| None).is_ok()));
}

/// A crash in the middle of a write leaves the beginning of a line. It is dropped, the file
/// is cut back, and the next record does not land behind it.
#[test]
fn a_torn_last_line_is_cut_off_before_appending() {
    let records = chain(3);
    let (_dir, path) = written(&records);
    let whole = fs::read(&path).unwrap();
    let two = whole.len() - (records[2].header().to_json().unwrap().len() + 1);
    // Every way the third line can be cut short, down to nothing and up to its missing newline.
    for torn in two..whole.len() {
        fs::write(&path, &whole[..torn]).unwrap();
        assert_eq!(read(&path).unwrap(), records[..2], "read leaves out {} torn bytes", torn - two);
        assert_eq!(fs::read(&path).unwrap().len(), torn, "read changes nothing");
        let (mut pack, held) = open(&path).unwrap();
        assert_eq!(held, records[..2]);
        assert_eq!(fs::read(&path).unwrap(), whole[..two], "open cuts {} torn bytes", torn - two);
        pack.append(&records[2]).unwrap();
        drop(pack);
        assert_eq!(fs::read(&path).unwrap(), whole);
    }
}

/// The tail may be cut inside a character of several bytes, or be bytes that were never
/// written by anyone.
#[test]
fn a_tail_that_is_not_text_is_cut_off() {
    let mut with_text = header("k");
    with_text.insert("note".to_owned(), text("活動 événement"));
    let wide = Record::try_from(seal(&secret(1), Value::Map(with_text))).unwrap();
    let records = [chain(2), vec![wide.clone()]].concat();
    let (_dir, path) = written(&records);
    let whole = fs::read(&path).unwrap();
    let two = whole.len() - (wide.header().to_json().unwrap().len() + 1);
    let character = whole.windows(3).position(|bytes| bytes == "活".as_bytes()).unwrap();
    let tails: [&[u8]; 8] = [
        &whole[two..character + 1],
        &whole[two..character + 2],
        &[0; 4096],
        &[0xff, 0xfe, 0x00, 0x80],
        b"{\"$type\":\"record\"}\n",
        b"\n",
        b"not json\n",
        b"\r\n",
    ];
    for tail in tails {
        fs::write(&path, [&whole[..two], tail].concat()).unwrap();
        assert_eq!(read(&path).unwrap(), records[..2]);
        let (mut pack, held) = open(&path).unwrap();
        assert_eq!(held, records[..2]);
        pack.append(&wide).unwrap();
        drop(pack);
        assert_eq!(fs::read(&path).unwrap(), whole);
    }
    // A file that is nothing but a torn line is an empty pack.
    fs::write(&path, &whole[..10]).unwrap();
    let (_pack, held) = open(&path).unwrap();
    assert!(held.is_empty());
    assert_eq!(fs::read(&path).unwrap(), b"");
}

/// A line that is not a record, with lines after it, is damage and not a crash. It is
/// reported with its number, and the file is left as it is.
#[test]
fn a_damaged_line_in_the_middle_is_an_error() {
    let records = chain(4);
    let (_dir, path) = written(&records);
    let whole = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = whole.lines().collect();
    let damaged: [(String, usize); 7] = [
        (lines[1].replacen("ev/event", "ev event", 1), 2),
        (lines[1][..40].to_owned(), 2),
        (String::new(), 2),
        ("null".to_owned(), 2),
        (lines[1].replacen("\"v\":1", "\"v\":2", 1), 2),
        (lines[1].replacen('{', "", 1), 2),
        (format!("{}\u{0}", lines[1]), 2),
    ];
    for (line, number) in damaged {
        let text = [lines[0], &line, lines[2], lines[3]].join("\n") + "\n";
        fs::write(&path, &text).unwrap();
        assert_eq!(read(&path), Err(Error::Corrupt(number)), "{line:.50}");
        assert_eq!(open(&path).unwrap_err(), Error::Corrupt(number));
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "left as it is");
    }
    // The first line, and the last but one before a torn tail.
    fs::write(&path, format!("x\n{whole}")).unwrap();
    assert_eq!(read(&path), Err(Error::Corrupt(1)));
    fs::write(&path, format!("{}\n{}\nx\n{}", lines[0], lines[1], &lines[2][..9])).unwrap();
    assert_eq!(read(&path), Err(Error::Corrupt(3)));
    // A line whose signature is wrong is still a line: the pack checks structure only.
    let forged = lines[1].replacen(&(AT + 1).to_string(), &(AT + 77).to_string(), 1);
    assert_ne!(forged, lines[1]);
    fs::write(&path, [lines[0], &forged, lines[2]].join("\n") + "\n").unwrap();
    let held = read(&path).unwrap();
    assert_eq!(held.len(), 3);
    assert_eq!(held[1].verify(|_, _, _| true, |_| None), Err(Error::Signature));
    // JSON text is free in its spacing and in the order of its keys.
    let spaced = format!(" {}\t\r", lines[1].replace("\",\"", "\" ,\t\"").replace("\":", "\" : "));
    assert!(spaced.len() > lines[1].len() + 20);
    fs::write(&path, [lines[0], &spaced, lines[2]].join("\n") + "\n").unwrap();
    assert_eq!(read(&path).unwrap(), records[..3]);
}

#[test]
fn one_writer_at_a_time() {
    let (_dir, path) = written(&chain(1));
    let (pack, _) = open(&path).unwrap();
    assert_eq!(open(&path).unwrap_err(), Error::Io(ErrorKind::WouldBlock));
    assert_eq!(read(&path).unwrap().len(), 1, "reading needs no lock");
    drop(pack);
    assert!(open(&path).is_ok());
}

/// A record that JSON text cannot hold is refused before the file is touched.
#[test]
fn a_record_that_would_not_read_back_is_not_written() {
    let records = chain(2);
    let (_dir, path) = written(&records);
    let before = fs::read(&path).unwrap();
    let mut reserved = header("k");
    reserved.insert("x".to_owned(), map([("$link", Value::Int(5))]));
    let unwritable = Record::try_from(seal(&secret(1), Value::Map(reserved))).unwrap();
    assert_eq!(unwritable.verify(|_, _, _| true, |_| None), Ok(()), "a valid record all the same");
    let (mut pack, _) = open(&path).unwrap();
    assert_eq!(pack.append(&unwritable), Err(Error::Json));
    assert_eq!(fs::read(&path).unwrap(), before);
    // The pack is still good for the next record.
    let next = Record::sign(
        &secret(1),
        common::path("k"),
        Some(cid("next")),
        [records[1].cid()],
        AT + 9,
        None,
    )
    .unwrap();
    pack.append(&next).unwrap();
    pack.sync().unwrap();
    assert_eq!(read(&path).unwrap(), [records, vec![next]].concat());
}

/// After a failed write the pack answers every call with the same error, and the torn line
/// the failure left is cut off when the pack is opened again. To make a write fail for real,
/// this test runs itself once more under a file size limit (`ulimit -f 8`: 4096 or 8192 bytes).
#[cfg(unix)]
#[test]
fn a_failed_write_stops_the_pack() {
    const INNER: &str = "ZU_TEST_UNDER_FILE_LIMIT";
    let Some(path) = std::env::var_os(INNER) else {
        let (_dir, path) = fresh();
        let limited = "trap '' XFSZ; ulimit -f 8; exec \"$0\" \"$@\"";
        // Its output goes to a pipe: the limit would strike a log file this run writes to.
        let run = std::process::Command::new("sh")
            .args(["-c", limited])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "a_failed_write_stops_the_pack", "--nocapture"])
            .env(INNER, &path)
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&run.stdout) + String::from_utf8_lossy(&run.stderr);
        assert!(run.status.success(), "the run under the file size limit failed: {said}");
        // Out of the limit again: the pack holds what was appended before the failure.
        let (mut pack, held) = open(&path).unwrap();
        assert!(!held.is_empty() && fs::read(&path).unwrap().len() < 8192);
        pack.append(&record("k", AT + 100, &[])).unwrap();
        assert_eq!(read(&path).unwrap().len(), held.len() + 1);
        return;
    };
    let too_large = Err(Error::Io(ErrorKind::FileTooLarge));
    let (mut pack, _) = open(&path).unwrap();
    let mut appended = Vec::new();
    let failed = loop {
        let record = record("k", AT + appended.len() as i64, &[]);
        match pack.append(&record) {
            Ok(()) => appended.push(record),
            failed => break failed,
        }
        assert!(appended.len() < 100, "the limit never struck");
    };
    assert_eq!(failed, too_large);
    assert!(appended.len() > 3);
    // Every later call is answered with the same error, and nothing more is written.
    let torn = fs::read(&path).unwrap();
    assert!([4096, 8192].contains(&torn.len()) && !torn.ends_with(b"\n"), "{} bytes", torn.len());
    assert_eq!(pack.append(&appended[0]), too_large);
    assert_eq!(pack.sync(), too_large);
    assert_eq!(fs::read(&path).unwrap(), torn);
    // Reading leaves out the torn line; opening again cuts it off.
    assert_eq!(read(&path).unwrap(), appended);
    drop(pack);
    let (_pack, held) = open(&path).unwrap();
    assert_eq!(held, appended);
    assert!(fs::read(&path).unwrap().ends_with(b"}\n"));
}

#[test]
fn a_body_is_kept_beside_the_record_that_links_to_it() {
    let (_dir, path) = fresh();
    let body = map([("text", text("the first shipment leaves in March"))]);
    let (mut pack, _) = Pack::open(&path).unwrap();
    let cid = pack.keep(&body).unwrap();
    pack.append(&record("k", AT, &[])).unwrap();
    drop(pack);

    let held = Pack::read(&path).unwrap();
    assert_eq!(held.records.len(), 1);
    assert_eq!(held.bodies.get(&cid), Some(&body));
    // The line is the body as JSON, for any tool to read.
    assert!(fs::read_to_string(&path).unwrap().contains("the first shipment leaves in March"));
}

#[test]
fn a_body_cannot_pass_for_a_header() {
    let (_dir, path) = fresh();
    let (mut pack, _) = Pack::open(&path).unwrap();
    let fake = map([("$type", text("record"))]);
    assert_eq!(pack.keep(&fake), Err(Error::Field("body")));
    // record.md § Body: a body is a map.
    assert_eq!(pack.keep(&text("not a map")), Err(Error::Field("body")));
}

#[test]
fn a_torn_body_is_cut_off_like_a_torn_record() {
    let (_dir, path) = fresh();
    let (mut pack, _) = Pack::open(&path).unwrap();
    pack.append(&record("k", AT, &[])).unwrap();
    pack.keep(&map([("text", text("never finished"))])).unwrap();
    drop(pack);
    let whole = fs::read(&path).unwrap();
    fs::write(&path, &whole[..whole.len() - 5]).unwrap();

    let (_pack, held) = Pack::open(&path).unwrap();
    assert_eq!((held.records.len(), held.bodies.len()), (1, 0));
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
}
