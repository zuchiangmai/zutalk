//! What a line is: one flat JSON object of numbers, booleans, codes and ids; text that is none
//! of these is turned away and counted; nothing another library says gets in.

use serde_json::Value as Json;
use std::path::Path;
use zutalk_log::{Id, Level, Limits, Value, capture, files, info, is_code, is_plain};

/// The lines of a file, each parsed.
fn read(path: &Path) -> Vec<serde_json::Map<String, Json>> {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines()
        .map(|line| serde_json::from_str::<Json>(line).unwrap())
        .map(|json| json.as_object().unwrap().clone())
        .collect()
}

#[test]
fn a_line_is_one_flat_object_of_numbers_booleans_codes_and_ids() {
    let dir = tempfile::tempdir().unwrap();
    let (scope, guard) = files(dir.path(), "zutalk", Level::Debug, Limits::default()).unwrap();
    let entered = scope.enter();
    info!(
        "test.every_kind",
        a = 1u8,
        b = 2u16,
        c = 3u32,
        d = 4u64,
        e = 5usize,
        f = -6i32,
        g = -7i64,
        h = 0.5f64,
        i = true,
        j = "a.code",
        k = Id("19a3f0c2b7e00d4c1f2"),
    );
    drop(entered);
    drop(guard);
    let lines = read(&dir.path().join("zutalk.jsonl"));
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    assert!(line["t_ms"].as_u64().unwrap() > 1_700_000_000_000);
    assert_eq!(line["seq"], 1);
    assert_eq!(line["level"], "info");
    assert_eq!(line["component"], "lines");
    assert_eq!(line["event"], "test.every_kind");
    let fields: Vec<(&str, &Json)> = line.iter().map(|(k, v)| (k.as_str(), v)).collect();
    let want = serde_json::json!({
        "a": 1, "b": 2, "c": 3, "d": 4, "e": 5, "f": -6, "g": -7, "h": 0.5, "i": true,
        "j": "a.code", "k": "19a3f0c2b7e00d4c1f2",
    });
    for (name, value) in want.as_object().unwrap() {
        assert_eq!(&line[name], value, "{name}");
    }
    assert!(
        line["d"].is_u64() && line["g"].is_i64() && line["h"].is_f64() && line["i"].is_boolean()
    );
    // Flat: no value is an object or an array.
    assert!(fields.iter().all(|(_, v)| !v.is_object() && !v.is_array()), "{fields:?}");
    assert_eq!(fields.len(), 5 + 11);
}

#[test]
fn text_that_is_no_code_and_no_id_is_turned_away_and_counted() {
    let long = "x".repeat(65);
    let say = || {
        info!(
            "test.refused",
            spaced = Id("two words"),
            wide = Id("字幕"),
            long = Id(&long),
            event = 1u8,
            fine = Id("19a3f0c2b7e00d4c1f2"),
        )
    };
    let seen = capture(Level::Info);
    say();
    assert_eq!(seen.refused(), 4);
    let line = &seen.of("test.refused")[0];
    for name in ["spaced", "wide", "long", "event"] {
        assert_eq!(line.text(name), Some("!"), "{name}");
    }
    assert_eq!(line.text("fine"), Some("19a3f0c2b7e00d4c1f2"));
    drop(seen);

    // The file says the same, and its own `event` is the code, not the field.
    let dir = tempfile::tempdir().unwrap();
    let (scope, guard) = files(dir.path(), "zutalk", Level::Info, Limits::default()).unwrap();
    let entered = scope.enter();
    say();
    drop((entered, guard));
    let text = std::fs::read_to_string(dir.path().join("zutalk.jsonl")).unwrap();
    assert!(!text.contains("two words") && !text.contains("字幕") && !text.contains(&long));
    assert_eq!(text.matches(r#""!""#).count(), 4, "{text}");
    assert!(text.contains(r#""event":"test.refused""#), "{text}");
}

#[test]
fn the_grammar_of_a_code_and_of_plain_text() {
    let codes = [
        ("lane.link", true),
        ("stall.snapshot.end", true),
        ("a.b", true),
        ("lane.open2", true),
        ("shell.main_thread", true),
        ("lane", false),      // one part
        ("Lane.link", false), // upper case
        ("lane.Link", false),
        ("lane reopen", false), // a space
        ("lane..link", false),  // an empty part
        ("lane.", false),
        (".lane", false),
        ("lane.2nd", false), // a part starts with a letter
        ("lane._x", false),
        ("lane-link.x", false),
        ("", false),
    ];
    for (code, ok) in codes {
        assert_eq!(is_code(code), ok, "{code:?}");
    }
    // At most 64 bytes, a code's included.
    let at = |n: usize| format!("a.{}", "b".repeat(n - 2));
    assert!(is_code(&at(64)) && !is_code(&at(65)));

    let texts = [
        ("", true),
        ("19a3f0c2b7e00d4c1f2", true),
        ("Recording", true),
        ("zutalk_core::rt::supervise", true),
        ("core/src/app/hub.rs", true),
        ("a-b.c", true),
        ("two words", false),
        ("字幕", false),
        ("a\"b", false),
        ("a\nb", false),
        ("did:key:z6Mk?x", false),
        ("a,b", false),
    ];
    for (text, ok) in texts {
        assert_eq!(is_plain(text), ok, "{text:?}");
    }
    assert!(is_plain(&"x".repeat(64)) && !is_plain(&"x".repeat(65)));

    // The level a setting asks for: `debug`, else `info`.
    let settings = [(Some("debug"), Level::Debug), (Some("info"), Level::Info)];
    let settings = settings.into_iter().chain([(Some("DEBUG"), Level::Info), (None, Level::Info)]);
    for (setting, level) in settings {
        assert_eq!(Level::from_setting(setting), level, "{setting:?}");
    }
}

#[test]
fn only_our_own_events_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (scope, guard) = files(dir.path(), "zutalk", Level::Debug, Limits::default()).unwrap();
    let entered = scope.enter();
    info!("test.ours", n = 1u8);
    tracing::info!(target: "hyper::client", peer = "10.0.0.1:443", "connected to {}", "a host");
    // Even under one of our names: only what our macros made has fields the compiler checked.
    tracing::error!(target: "zutalk_core::app::hub", "a sentence through tracing itself");
    let span = tracing::info_span!("a span", id = 7);
    let inside = span.enter();
    tracing::warn!("inside a span");
    drop(inside);
    drop(span);
    drop((entered, guard));
    let lines = read(&dir.path().join("zutalk.jsonl"));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["event"], "test.ours");
}

#[test]
fn a_capture_keeps_the_level_it_was_asked_for() {
    let seen = capture(Level::Info);
    zutalk_log::debug!("test.quiet", n = 1u8);
    zutalk_log::warn!("test.loud", n = 2u8);
    zutalk_log::error!("test.louder");
    assert_eq!(
        seen.all().iter().map(|line| line.code).collect::<Vec<_>>(),
        ["test.loud", "test.louder"]
    );
    let loud = &seen.of("test.loud")[0];
    assert_eq!((loud.level, loud.component), (Level::Warn, "lines"));
    assert_eq!(loud.get("n"), Some(&Value::U64(2)));
    assert_eq!(seen.count("test.louder"), 1);
    assert_eq!((seen.refused(), seen.lost()), (0, 0));
}
