//! A panic is said, without its message. Alone in its binary: it installs the process's log.

use zutalk_log::{Level, flush, install};

#[test]
fn a_panic_is_said_with_its_thread_and_place_and_never_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = install(dir.path(), "zutalk", Level::Info, false).unwrap();
    let ended = std::thread::Builder::new()
        .name("t1".into())
        .spawn(|| panic!("secret words {}", 42))
        .unwrap()
        .join();
    assert!(ended.is_err());
    assert!(flush(5000));
    let text = std::fs::read_to_string(dir.path().join("zutalk.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> =
        text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let panics: Vec<&serde_json::Value> =
        lines.iter().filter(|line| line["event"] == "app.panic").collect();
    assert_eq!(panics.len(), 1, "{text}");
    assert_eq!(panics[0]["level"], "error");
    assert_eq!(panics[0]["thread"], "t1");
    assert!(panics[0]["file"].as_str().unwrap().ends_with("panic.rs"), "{text}");
    assert!(panics[0]["line"].as_u64().unwrap() > 0);
    assert!(!text.contains("secret") && !text.contains("42\""), "{text}");
}
