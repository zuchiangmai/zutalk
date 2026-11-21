use std::fs;
use zutalk_core::capture::clock::{BLOCK, Block};
use zutalk_core::capture::tape::{Pass, Writer, read, recover};

const KEY: [u8; 32] = [7; 32];

fn block(n: u32) -> Block {
    Block { t_ms: n * 100, pcm: vec![n as i16 + 1; BLOCK] }
}

fn write(dir: &std::path::Path, blocks: u32) {
    let mut tape = Writer::create(dir, &KEY).unwrap();
    for n in 0..blocks {
        tape.append(&block(n)).unwrap();
    }
    tape.sync().unwrap();
}

fn read_all(dir: &std::path::Path, key: &[u8; 32]) -> std::io::Result<(u32, Vec<Block>)> {
    let mut got = Vec::new();
    let ms = read(dir, key, &Pass::Write, |b| got.push(b))?;
    Ok((ms, got))
}

#[test]
fn what_was_written_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 3);
    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 300);
    assert_eq!(got, [block(0), block(1), block(2)]);
}

#[test]
fn each_minute_is_its_own_file() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 601);
    assert!(dir.path().join("00000.tape").exists());
    assert!(dir.path().join("00001.tape").exists());
    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 60_100);
    assert_eq!(got.last(), Some(&block(600)));
}

#[test]
fn nothing_on_disk_is_plain_audio() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2);
    let bytes = fs::read(dir.path().join("00000.tape")).unwrap();
    let plain = 1i16.to_le_bytes().repeat(8);
    assert!(!bytes.windows(plain.len()).any(|w| w == plain));
}

#[test]
fn a_crash_loses_only_the_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 5);
    let path = dir.path().join("00000.tape");
    let torn = fs::metadata(&path).unwrap().len() - 7;
    fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(torn).unwrap();

    assert_eq!(recover(dir.path(), &KEY).unwrap(), 400);
    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!((ms, got.len()), (400, 4));
    // Recovery is idempotent.
    assert_eq!(recover(dir.path(), &KEY).unwrap(), 400);
}

#[test]
fn the_wrong_key_is_an_error_not_an_empty_recording() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2);
    assert!(read_all(dir.path(), &[8; 32]).is_err());
}

#[test]
fn a_block_moved_to_another_position_does_not_open() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 2);
    let path = dir.path().join("00000.tape");
    let bytes = fs::read(&path).unwrap();
    let (first, second) = bytes.split_at(bytes.len() / 2);
    fs::write(&path, [second, first].concat()).unwrap();
    assert!(read_all(dir.path(), &KEY).is_err());
}

#[test]
fn a_short_block_inside_the_recording_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), 601);
    let path = dir.path().join("00000.tape");
    let torn = fs::metadata(&path).unwrap().len() - 7;
    fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(torn).unwrap();
    assert!(recover(dir.path(), &KEY).is_err());
}
