mod hub;

use futures_util::{SinkExt, StreamExt};
use hub::{App, Asked, STORE, config, signed_in, soniox};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use zu::SecretKey;
use zutalk_core::app::hub::Config;
use zutalk_core::app::proto::{Command, Setup as Next};
use zutalk_core::id::keys;
use zutalk_core::knowledge::glossary::{self, Term};
use zutalk_core::live::lane::{self, Setup, Stream};
use zutalk_core::record::shelf::Shelf;

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const NOW: i64 = 1_800_000_000_000;

fn owned(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn terms() -> Vec<Term> {
    vec![
        Term {
            said: "Zuzalu".into(), written: owned(&[("th", "ซูซาลู"), ("zh", "祖扎卢")])
        },
        Term { said: "ZuTalk".into(), written: BTreeMap::new() },
    ]
}

/// What the realtime service is told first on a connection for `stream`.
fn told(terms: &[Term], stream: Stream) -> Value {
    let setup = Setup { model: "stt-rt-v6".into(), ..Setup::default() };
    lane::config("a-key", &stream, &glossary::live(terms, &stream, setup))
}

fn pairs(pairs: &[(&str, &str)]) -> Value {
    json!(
        pairs
            .iter()
            .map(|(source, target)| json!({ "source": source, "target": target }))
            .collect::<Vec<_>>()
    )
}

#[test]
fn every_form_of_a_term_is_listened_for_and_each_column_gets_its_own_form() {
    let heard = json!(["ZuTalk", "Zuzalu", "ซูซาลู", "祖扎卢"]);
    // Both ways between English and Thai: English has no form of its own, so it is as said.
    let both = told(&terms(), Stream::TwoWay("en".into(), "th".into()));
    assert_eq!(both["context"]["terms"], heard);
    let expected =
        [("Zuzalu", "ซูซาลู"), ("ซูซาลู", "Zuzalu"), ("祖扎卢", "Zuzalu"), ("祖扎卢", "ซูซาลู")];
    assert_eq!(both["context"]["translation_terms"], pairs(&expected));
    let one = told(&terms(), Stream::OneWay("zh".into()));
    assert_eq!(
        one["context"]["translation_terms"],
        pairs(&[("Zuzalu", "祖扎卢"), ("ซูซาลู", "祖扎卢")])
    );
    // Transcribing only, the terms are listened for and nothing is translated.
    let only = told(&terms(), Stream::Transcribe);
    assert_eq!(only["context"], json!({ "terms": heard }));
    // Without a glossary the service is told nothing about one.
    assert!(told(&[], Stream::TwoWay("en".into(), "th".into()))["context"].is_null());
}

#[test]
fn a_term_is_one_record_however_it_is_typed_and_goes_when_it_is_taken_out() {
    let dir = tempfile::tempdir().expect("a folder");
    let shelf = Shelf::new(dir.path(), ME, Arc::new(SecretKey::from_seed(&[5; 32])));
    let written = owned(&[("th", " ซูซาลู "), ("zh", " ")]);
    glossary::put(&shelf, "  Zuzalu ", Some(written), NOW).expect("it is kept");
    glossary::put(&shelf, "ZuTalk", Some(BTreeMap::new()), NOW).expect("it is kept");
    let read = glossary::read(&shelf).expect("it reads");
    let zuzalu = Term { said: "Zuzalu".into(), written: owned(&[("th", "ซูซาลู")]) };
    assert_eq!(read, [Term { said: "ZuTalk".into(), written: BTreeMap::new() }, zuzalu]);
    // Typed again in other letters, it is the same term, now as typed.
    glossary::put(&shelf, "ＺＵＺＡＬＵ", Some(owned(&[("zh", "祖扎卢")])), NOW + 1).expect("kept");
    let read = glossary::read(&shelf).expect("it reads");
    assert_eq!(read[1], Term { said: "ＺＵＺＡＬＵ".into(), written: owned(&[("zh", "祖扎卢")]) });
    glossary::put(&shelf, "zuzalu", None, NOW + 2).expect("taken out");
    assert_eq!(glossary::read(&shelf).expect("it reads").len(), 1);
    assert_eq!(glossary::put(&shelf, " ", Some(BTreeMap::new()), NOW), Err("term.empty"));
    // It is the person's, kept apart: no recording.
    assert!(shelf.list().expect("a list").is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_glossary_reaches_the_live_captions_and_the_whole_session_transcript() {
    let _store = STORE.lock().await;
    signed_in();
    keys::set("openrouter", Some(b"or-own-test-key")).expect("the key is kept");
    let (dir, asked) = (tempfile::tempdir().expect("a folder"), Asked::default());
    let stt = soniox(&asked).await;
    let realtime = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let config = Config {
        soniox: format!("{}/v1", stt.uri()),
        realtime: format!("ws://{}", realtime.local_addr().expect("an address")),
        ..config(dir.path())
    };
    let mut app = App::with(config);
    app.until(|_| true).await;
    let written = Some(owned(&[("th", "ซูซาลู")]));
    app.send(Command::Term { said: "Zuzalu".into(), written });
    let terms = app.wait_glossary(|terms| !terms.is_empty()).await;
    assert_eq!(terms, [Term { said: "Zuzalu".into(), written: owned(&[("th", "ซูซาลู")]) }]);
    let next = Next {
        spoken: vec!["en".into(), "th".into()],
        shown: vec!["en".into(), "th".into()],
        ..Next::default()
    };
    app.send(Command::Setup { setup: next });
    app.send(Command::Start);
    // The live captions: one connection, both ways between English and Thai.
    let (stream, _) = realtime.accept().await.expect("a connection");
    let mut ws = accept_async(stream).await.expect("a websocket");
    let Some(Ok(Message::Text(first))) = ws.next().await else { panic!("no configuration") };
    let first: Value = serde_json::from_str(&first).expect("JSON");
    assert_eq!(first["context"]["terms"], json!(["Zuzalu", "ซูซาลู"]));
    assert_eq!(
        first["context"]["translation_terms"],
        pairs(&[("Zuzalu", "ซูซาลู"), ("ซูซาลู", "Zuzalu")])
    );
    tokio::spawn(async move {
        while let Some(Ok(message)) = ws.next().await {
            if matches!(&message, Message::Text(text) if text.is_empty()) {
                let finished = json!({ "tokens": [], "finished": true }).to_string();
                _ = ws.send(Message::text(finished)).await;
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(600)).await;
    app.send(Command::Stop);
    // The whole session: the transcription service is told every form too.
    let wait = async {
        while asked.lock().expect("one at a time").is_empty() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(20), wait).await.expect("it was asked in time");
    let body = asked.lock().expect("one at a time")[0].clone();
    assert_eq!(body["context"]["terms"], json!(["Zuzalu", "ซูซาลู"]));
    // The glossary is the person's: signed out, there is none on screen.
    app.send(Command::SignOut);
    app.until(|state| state.me.is_none()).await;
    app.wait_glossary(|terms| terms.is_empty()).await;
}
