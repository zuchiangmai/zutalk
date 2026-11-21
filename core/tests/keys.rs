//! Where secrets live, what the 0.8 app left behind, and an identity made on this device.

mod common;

use std::fs;
use std::path::Path;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use zutalk_core::keys::{self, Secret};
use zutalk_core::supervise::End;

/// Where secrets live is the process's: one test at a time has it.
static STORE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Secrets kept under `dir`, as the app keeps them on macOS; elsewhere a store in memory.
fn store(dir: &Path) {
    if cfg!(target_os = "macos") {
        keys::open_store(dir).expect("the store opens");
    } else {
        keyring_core::set_default_store(keyring_core::mock::Store::new().expect("a mock store"));
    }
}

#[test]
fn a_secret_is_kept_replaced_and_forgotten_by_its_name() {
    let _one = STORE.blocking_lock();
    let dir = tempfile::tempdir().expect("a folder");
    store(dir.path());
    assert!(keys::get("soniox").expect("it reads").is_none());
    keys::set("soniox", Some(b"sk-first")).expect("it is kept");
    keys::set("openrouter", Some(b"or-key")).expect("it is kept");
    keys::set("soniox", Some(b"sk-second")).expect("it is replaced");
    assert_eq!(
        keys::get("soniox").expect("it reads").as_deref().map(Vec::as_slice),
        Some(&b"sk-second"[..])
    );
    keys::set("soniox", None).expect("it is forgotten");
    keys::set("never-kept", None).expect("forgetting nothing is no error");
    assert!(keys::get("soniox").expect("it reads").is_none());
    assert_eq!(
        keys::get("openrouter").expect("it reads").as_deref().map(Vec::as_slice),
        Some(&b"or-key"[..])
    );
    // A secret does not print.
    assert_eq!(format!("{:?}", Secret::new("sk-second")), "Secret(…)");
}

/// On macOS, while releases are ad-hoc signed, secrets live in one file only this account can
/// read, in a folder only it can enter; written whole, so nothing is left beside it.
#[cfg(target_os = "macos")]
#[test]
fn on_macos_secrets_live_in_one_file_only_this_account_can_read() {
    use std::os::unix::fs::PermissionsExt;
    let _one = STORE.blocking_lock();
    let dir = tempfile::tempdir().expect("a folder");
    store(dir.path());
    let threads: Vec<_> = (0..8)
        .map(|n| std::thread::spawn(move || keys::set(&format!("k{n}"), Some(&[n; 32])).unwrap()))
        .collect();
    threads.into_iter().for_each(|thread| thread.join().unwrap());
    for n in 0..8 {
        let kept = keys::get(&format!("k{n}")).expect("it reads").expect("no change was lost");
        assert_eq!(kept.as_slice(), &[n; 32]);
    }
    let secrets = dir.path().join("Secrets");
    let names: Vec<_> = fs::read_dir(&secrets).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["secrets.json"]);
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode(&secrets), mode(&secrets.join("secrets.json"))), (0o700, 0o600));
    // A file that does not read is said, not taken for an empty one.
    fs::write(secrets.join("secrets.json"), b"not json").unwrap();
    assert_eq!(keys::get("k1").err(), Some(End::Fatal("keys.store")));
    assert_eq!(keys::set("k1", None), Err(End::Fatal("keys.store")));
}

#[test]
fn the_old_apps_invite_is_read_from_its_one_file_left_as_it_is_and_carried_once() {
    let _one = STORE.blocking_lock();
    let dir = tempfile::tempdir().expect("a folder");
    store(dir.path());
    let file = dir.path().join("community-invite.json");
    assert!(keys::invite(&file).expect("none is no error").is_none());
    let json = br#"{"version":1,"accessToken":" zl-token \n"}"#;
    fs::write(&file, json).unwrap();
    let token = keys::invite(&file).expect("it reads").expect("there is one");
    assert_eq!(token.expose(), "zl-token");
    keys::carried(&token).expect("it is remembered");
    assert!(keys::invite(&file).expect("it reads").is_none(), "carried over, it is not sent again");
    assert_eq!(fs::read(&file).unwrap(), json, "the old file is left as it is");
    fs::write(&file, br#"{"version":1,"accessToken":"another"}"#).unwrap();
    assert_eq!(
        keys::invite(&file).expect("it reads").map(|t| t.expose().to_owned()).as_deref(),
        Some("another")
    );
    for damaged in
        [&br#"{"version":2,"accessToken":"t"}"#[..], br#"{"version":1,"accessToken":" "}"#, b"{"]
    {
        fs::write(&file, damaged).unwrap();
        assert_eq!(keys::invite(&file).err(), Some(End::Fatal("invite.file")));
    }
}

/// did-zu.md § DID Creation: the genesis operation goes to the directory, and the identity is
/// kept only once the directory took it. Its rotation key is kept too, and reminded of.
#[tokio::test]
async fn an_identity_is_made_here_once_the_directory_takes_it() {
    let _one = STORE.lock().await;
    let dir = tempfile::tempdir().expect("a folder");
    store(dir.path());
    let directory = MockServer::start().await;
    _ = rustls::crypto::ring::default_provider().install_default();
    let http = reqwest::Client::new();
    for (status, end) in [(400, End::Fatal("id.directory")), (503, End::Retry("server"))] {
        let refusing = Mock::given(method("POST")).respond_with(ResponseTemplate::new(status));
        let refusing = refusing.mount_as_scoped(&directory).await;
        assert_eq!(keys::create(&http, &directory.uri()).await.err(), Some(end));
        drop(refusing);
    }
    let away = keys::create(&http, "http://127.0.0.1:9").await.err();
    assert_eq!(away, Some(End::Retry("offline")));
    assert!(keys::kept().expect("it reads").is_none() && !keys::unsaved(), "nothing was kept");
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&directory).await;
    let me = keys::create(&http, &directory.uri()).await.expect("it is made");
    let posted = directory.received_requests().await.unwrap();
    let posted = posted.last().unwrap();
    assert_eq!(posted.url.path(), format!("/{}", me.did));
    // What was posted is the operation the kernel makes: it resolves to the key kept here.
    let op: serde_json::Value = serde_json::from_slice(&posted.body).unwrap();
    let doc = zu::did::resolve(&me.did, &common::audit(&me.did, &op)).expect("it resolves");
    assert!(doc.key_now(&me.key.public()));
    let kept = keys::kept().expect("it reads").expect("it is kept");
    assert_eq!((kept.did, kept.key.public()), (me.did.clone(), me.key.public()));
    // The person is reminded of the rotation key until they say they saved it; it is kept on.
    assert!(keys::unsaved());
    keys::saved().expect("it is noted");
    assert!(
        !keys::unsaved() && keys::get("rotation").expect("it reads").is_some_and(|k| k.len() == 32)
    );
}
