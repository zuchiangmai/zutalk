//! The process's one log, installed once. Alone in its binary: it is the process's.

use zutalk_log::{Level, flush, info, install};

#[test]
fn the_log_is_installed_once_and_its_guard_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs");
    assert!(flush(10), "nothing installed: nothing to wait for");
    let guard = install(&logs, "zutalk", Level::Info, false).unwrap();
    info!("test.one", n = 1u8);
    info!("test.two", n = 2u8);
    assert!(flush(5000));
    let read = || std::fs::read_to_string(logs.join("zutalk.jsonl")).unwrap();
    let text = read();
    assert!(text.contains(r#""event":"test.one""#) && text.contains(r#""event":"test.two""#));
    assert_eq!(text.lines().count(), 2);
    assert!(install(&logs, "zutalk", Level::Info, false).is_err());
    assert!(install(&dir.path().join("elsewhere"), "zutalk", Level::Info, false).is_err());
    assert_eq!((zutalk_log::lost(), zutalk_log::refused()), (0, 0));
    drop(guard);
    info!("test.after", n = 3u8);
    assert!(flush(100));
    assert_eq!(read().lines().count(), 2);
}
