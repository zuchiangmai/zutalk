//! The file: it turns at its size and keeps so many; a writer that stands still loses lines,
//! never the one who says them, and says how many.

use serde_json::Value as Json;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use zutalk_log::{Level, Limits, Open, files, files_with, info, must_turn};

fn read(path: &Path) -> Vec<Json> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn files_turn_at_their_size_and_the_oldest_goes() {
    let dir = tempfile::tempdir().unwrap();
    let limits = Limits { file_bytes: 1024, files_kept: 3, ..Limits::default() };
    let (scope, guard) = files(dir.path(), "zutalk", Level::Info, limits).unwrap();
    let entered = scope.enter();
    for n in 0..100u32 {
        info!("test.line", n = n, pad = "abcdefghij");
    }
    drop((entered, guard));

    let mut names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, ["zutalk.1.jsonl", "zutalk.2.jsonl", "zutalk.jsonl"]);
    let mut seqs = Vec::new();
    for name in ["zutalk.2.jsonl", "zutalk.1.jsonl", "zutalk.jsonl"] {
        let path = dir.path().join(name);
        let lines = read(&path);
        let longest = lines.iter().map(|line| line.to_string().len() as u64 + 1).max().unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() <= 1024 + longest, "{name}");
        // Every file here began with a turn; the writer's own line takes no number.
        assert_eq!(lines[0]["event"], "log.turned", "{name}");
        assert_eq!(lines[0]["seq"], 0);
        assert!(lines[1..].iter().all(|line| line["event"] == "test.line"), "{name}");
        seqs.extend(lines[1..].iter().map(|line| line["seq"].as_u64().unwrap()));
    }
    // What is kept runs on without a gap, to the newest at the end of the newest file.
    assert!(seqs.windows(2).all(|pair| pair[1] == pair[0] + 1), "{seqs:?}");
    assert_eq!(seqs.last(), Some(&100));
    assert!(seqs.len() > 10 && seqs.len() < 100, "{}", seqs.len());
}

#[test]
fn a_file_turns_before_the_line_that_would_take_it_past_its_size() {
    let rows = [
        // written, line, most: turns
        (0, 100, 1024, false),   // an empty file takes any line
        (0, 5000, 1024, false),  // even one bigger than a file
        (924, 100, 1024, false), // exactly full
        (925, 100, 1024, true),
        (1024, 1, 1024, true),
        (1, 1023, 1024, false),
        (1, 1024, 1024, true),
        (u64::MAX, 1, 1024, true),
    ];
    for (written, line, most, turns) in rows {
        assert_eq!(must_turn(written, line, most), turns, "{written} {line} {most}");
    }
}

/// A door the writer cannot open a file through until the test lets it.
#[derive(Default)]
struct Gate(Mutex<bool>, Condvar);

impl Gate {
    fn pass(&self) {
        let mut open = self.0.lock().unwrap();
        while !*open {
            open = self.1.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }
}

#[test]
fn a_writer_that_stands_still_loses_lines_and_says_how_many() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::default());
    let held = gate.clone();
    let open: Open = Arc::new(move |path: &Path| -> io::Result<Box<dyn Write + Send>> {
        held.pass();
        Ok(Box::new(std::fs::OpenOptions::new().create(true).append(true).open(path)?))
    });
    let limits = Limits { queue_lines: 4, ..Limits::default() };
    let (scope, guard) = files_with(dir.path(), "zutalk", Level::Info, limits, open).unwrap();
    let entered = scope.enter();
    // The writer holds at most one line at the gate and the queue four: the rest are lost,
    // and saying them never waited.
    for n in 0..20u32 {
        info!("test.line", n = n);
    }
    gate.open();
    drop((entered, guard));

    let lines = read(&dir.path().join("zutalk.jsonl"));
    let lost: Vec<&Json> = lines.iter().filter(|line| line["event"] == "log.lost").collect();
    // One before the line the writer held at the gate, if it saw losses by then, and one after.
    assert!((1..=2).contains(&lost.len()), "{lines:?}");
    assert!(lost.iter().all(|line| line["level"] == "warn" && line["seq"] == 0));
    let n: u64 = lost.iter().map(|line| line["n"].as_u64().unwrap()).sum();
    let kept: Vec<u64> = lines
        .iter()
        .filter(|line| line["event"] == "test.line")
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert!(n >= 15, "{n}");
    assert_eq!(kept.len() as u64 + n, 20);
    // The numbers missing are exactly as many as were lost: a gap is a line lost.
    let missing = (1..=20).filter(|seq| !kept.contains(seq)).count() as u64;
    assert_eq!(missing, n);
    assert!(kept.windows(2).all(|pair| pair[0] < pair[1]), "{kept:?}");
}

#[test]
fn a_file_is_written_on_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    for round in 0..2u8 {
        let (scope, guard) = files(dir.path(), "zutalk", Level::Info, Limits::default()).unwrap();
        let entered = scope.enter();
        info!("test.round", round = round);
        drop((entered, guard));
    }
    let lines = read(&dir.path().join("zutalk.jsonl"));
    let rounds: Vec<&Json> = lines.iter().map(|line| &line["round"]).collect();
    assert_eq!(rounds, [&Json::from(0), &Json::from(1)]);
}
