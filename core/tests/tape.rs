use std::fs::{self, OpenOptions};
use std::io::Write;
use zutalk_core::capture::clock::{BLOCK, Block};
use zutalk_core::capture::tape::{Pass, Place, Writer, place, read, recover};
use zutalk_log::Level;

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

#[test]
fn place_classifies_before_next_and_ahead() {
    let cases = [
        (0, 0, Place::Next),
        (0, 100, Place::Ahead { blocks: 1 }),
        (0, 500, Place::Ahead { blocks: 5 }),
        (100, 0, Place::Before),
        (100, 100, Place::Next),
        (100, 500, Place::Ahead { blocks: 4 }),
        (59_900, 60_200, Place::Ahead { blocks: 3 }),
        (60_000, 59_900, Place::Before),
        (60_000, 60_000, Place::Next),
    ];
    for (next_ms, block_ms, expected) in cases {
        assert_eq!(place(next_ms, block_ms), expected, "next_ms={next_ms}, block_ms={block_ms}");
    }
}

#[test]
fn a_hole_between_blocks_is_sealed_silence() {
    let seen = zutalk_log::capture(Level::Info);
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::create(dir.path(), &KEY).unwrap();
    writer.append(&block(0)).unwrap();
    writer.append(&block(5)).unwrap();
    writer.sync().unwrap();
    assert_eq!(writer.take_padded(), 4);

    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 600);
    assert_eq!(got.len(), 6);
    assert_eq!(got[0], block(0));
    for (i, b) in got[1..5].iter().enumerate() {
        assert_eq!(b.t_ms, (i as u32 + 1) * 100);
        assert!(b.pcm.iter().all(|&s| s == 0), "block {} must be silence", b.t_ms);
    }
    assert_eq!(got[5], block(5));

    let pads = seen.of("tape.pad");
    assert_eq!(pads.len(), 1, "{pads:?}");
    assert_eq!(pads[0].level, Level::Warn);
    assert_eq!(pads[0].u64("from_ms"), Some(100));
    assert_eq!(pads[0].u64("blocks"), Some(4));
    assert_eq!(seen.refused(), 0);
}

#[test]
fn a_hole_across_a_minute_is_silence_in_both_files() {
    let seen = zutalk_log::capture(Level::Info);
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::create(dir.path(), &KEY).unwrap();
    for n in 0..599u32 {
        writer.append(&block(n)).unwrap();
    }
    // Gap covers 59_900 (end of minute 0) and 60_000, 60_100 (start of minute 1).
    writer.append(&block(602)).unwrap();
    writer.sync().unwrap();
    assert_eq!(writer.take_padded(), 3);

    assert!(dir.path().join("00000.tape").exists());
    assert!(dir.path().join("00001.tape").exists());

    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 60_300);
    assert_eq!(got.len(), 603);
    assert_eq!(got[598], block(598));
    for idx in [599usize, 600, 601] {
        assert_eq!(got[idx].t_ms, idx as u32 * 100);
        assert!(got[idx].pcm.iter().all(|&s| s == 0), "block {idx} must be silence");
    }
    assert_eq!(got[602], block(602));

    let pads = seen.of("tape.pad");
    assert_eq!(pads.len(), 1, "{pads:?}");
    assert_eq!(pads[0].u64("from_ms"), Some(59_900));
    assert_eq!(pads[0].u64("blocks"), Some(3));
    assert_eq!(seen.refused(), 0);
}

#[test]
fn a_writer_opened_on_a_tape_goes_on_where_it_ends() {
    let seen = zutalk_log::capture(Level::Info);
    let dir = tempfile::tempdir().unwrap();
    {
        let mut writer = Writer::create(dir.path(), &KEY).unwrap();
        for n in 0..3u32 {
            writer.append(&block(n)).unwrap();
        }
    }
    // Append half a sealed block of garbage to simulate a mid-write crash.
    let path = dir.path().join("00000.tape");
    let garbage = vec![0xABu8; BLOCK];
    OpenOptions::new().append(true).open(&path).unwrap().write_all(&garbage).unwrap();

    // Reopen and write block at 1,000 ms (index 10): truncates tail, pads 7 silent blocks, writes block 10.
    let mut writer = Writer::create(dir.path(), &KEY).unwrap();
    writer.append(&block(10)).unwrap();
    writer.sync().unwrap();

    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 1_100);
    assert_eq!(got.len(), 11);
    assert_eq!(got[..3], [block(0), block(1), block(2)]);
    for (i, b) in got[3..10].iter().enumerate() {
        assert_eq!(b.t_ms, (i as u32 + 3) * 100);
        assert!(b.pcm.iter().all(|&s| s == 0));
    }
    assert_eq!(got[10], block(10));

    let pads = seen.of("tape.pad");
    assert_eq!(pads.len(), 1, "{pads:?}");
    assert_eq!(pads[0].u64("from_ms"), Some(300));
    assert_eq!(pads[0].u64("blocks"), Some(7));
    assert_eq!(seen.refused(), 0);
}

#[test]
fn a_block_from_before_the_end_is_skipped() {
    let seen = zutalk_log::capture(Level::Info);
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::create(dir.path(), &KEY).unwrap();
    for n in 0..4u32 {
        writer.append(&block(n)).unwrap();
    }
    // Re-send already-written blocks with different audio; they must be skipped cleanly.
    writer.append(&Block { t_ms: 100, pcm: vec![99; BLOCK] }).unwrap();
    writer.append(&Block { t_ms: 300, pcm: vec![99; BLOCK] }).unwrap();
    writer.append(&block(4)).unwrap();
    writer.sync().unwrap();

    let (ms, got) = read_all(dir.path(), &KEY).unwrap();
    assert_eq!(ms, 500);
    assert_eq!(got, [block(0), block(1), block(2), block(3), block(4)]);
    assert!(seen.of("tape.pad").is_empty());
    assert_eq!(seen.refused(), 0);
}
