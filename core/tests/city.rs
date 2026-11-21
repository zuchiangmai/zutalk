//! Filing to a city's archive, end to end: the app, signed in, files a recording to a stand-in
//! city on a port of this machine, a Zu host as record.md, query.md and blob.md describe it.

mod hub;

use hub::{App, ME, STORE, config, shelf, signed_in};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate, matchers::any};
use zu::{Blob, Cid, Codec, Held, PublicKey, Query, Record, SecretKey, Value};
use zutalk_core::app::proto::{Command, State};
use zutalk_core::capture::clock::Block;
use zutalk_core::capture::tape;
use zutalk_core::id::keys::{self, Me};
use zutalk_core::live::lines::Line;
use zutalk_core::out::city::{self, Filing};
use zutalk_core::record::shelf::{About, FILING, LINE, NOTE, RECORDING, SPEAKER, Said, Words};

const CITY: &str = "did:zu:4seascity4seascity4seas";
const ORGANIZER: &str = "did:zu:organizer";
const NOW: i64 = 1_800_000_000_000;

fn key(by: &str) -> SecretKey {
    SecretKey::from_seed(&[if by == ME { 5 } else { 7 }; 32])
}

/// A recording kept here as the app keeps one: its text in two languages, a speaker's name, a
/// note, and `ms` of audio whose every sample says when it was taken, in tenths of a second.
fn recording(data: &Path, ms: u32) -> String {
    let (id, shelf) = (format!("{NOW:011x}00000007"), shelf(data));
    let (spoken, shown) = (vec!["en".into()], vec!["en".into(), "th".into()]);
    let about = About {
        started: NOW,
        length_ms: ms,
        title: "Budget".into(),
        spoken,
        shown,
        take: 1,
        ..About::default()
    };
    shelf.put(&id, RECORDING, &[(id.clone(), Some(about))], NOW).unwrap();
    let line = |n: u32, text: &str, th: &str| {
        let translations = [("th".to_owned(), th.to_owned())].into();
        let (speaker, language, text) = ("1".into(), "en".into(), text.into());
        let line = Line {
            t0: n * 1000,
            t1: n * 1000 + 900,
            speaker,
            language,
            text,
            translations,
            incomplete: false,
        };
        (format!("{id}.1.{n:05}"), Some(Said { line, via: BTreeMap::new() }))
    };
    shelf.put(&id, LINE, &[line(0, "Hello", "สวัสดี"), line(1, "Thanks", "ขอบคุณ")], NOW).unwrap();
    shelf
        .put(&id, SPEAKER, &[(format!("{id}.1"), Some(Words { text: "Chair".into() }))], NOW)
        .unwrap();
    shelf.put(&id, NOTE, &[(id.clone(), Some(Words { text: "Only mine".into() }))], NOW).unwrap();
    let mut tape =
        tape::Writer::create(&data.join("tape").join(&id), &keys::derive("tape", &id).unwrap())
            .unwrap();
    for t in 0..ms / 100 {
        tape.append(&Block { t_ms: t * 100, pcm: vec![t as i16; 1600] }).unwrap();
    }
    id
}

/// A city: a Zu host on a port of this machine. It confirms its DID, answers queries, takes
/// records signed by whom they say and meant for it, and the files its current records name.
/// What `refuse` names gets that answer instead: a type a word, or `blob` a status.
#[derive(Clone, Default)]
struct City(Arc<Mutex<Hold>>);

#[derive(Default)]
struct Hold {
    held: Held,
    files: BTreeMap<Cid, Vec<u8>>,
    refuse: Option<(&'static str, &'static str)>,
}

impl Hold {
    /// The records the paths refer to, with their bodies.
    fn current(&self, query: &Query) -> Held {
        let paths = zu::record::paths(&self.held.records).into_values();
        let current = paths.filter_map(|records| records.first().copied().cloned());
        let records: Vec<Record> =
            current.filter(|r| r.body.is_some() && query.matches(r)).collect();
        let body = |r: &Record| Some((r.body?, self.held.bodies.get(&r.body?)?.clone()));
        Held { bodies: records.iter().filter_map(body).collect(), records }
    }
}

impl Respond for City {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let mut hold = self.0.lock().unwrap();
        let (method, path) = (request.method.as_str(), request.url.path());
        let param = |name: &str| {
            request.url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
        };
        match (method, path) {
            ("GET", "/.well-known/zu-did") => ResponseTemplate::new(200).set_body_string(CITY),
            ("GET", "/.well-known/zu-recs") => {
                let (by, to, ty, key) = (param("by"), param("to"), param("type"), param("key"));
                let held = hold.current(&Query { by, to, ty, key, ..Query::default() });
                ResponseTemplate::new(200).set_body_bytes(zu::query::answer(&held, None))
            }
            ("POST", "/.well-known/zu-recs") => {
                let Ok((given, _)) = Query::default().read(&request.body) else {
                    return ResponseTemplate::new(400);
                };
                let mut stat = BTreeMap::new();
                for record in given.records {
                    let signer = key(&record.path.by).public();
                    let signed = record.verify(
                        |by, pubkey: &PublicKey, _| by == ME && *pubkey == signer,
                        |_| None,
                    );
                    let word = match hold.refuse {
                        Some((ty, word)) if ty == record.path.ty => word,
                        _ if signed.is_err() || record.to.as_deref() != Some(CITY) => "bad",
                        _ => "ok",
                    };
                    stat.insert(record.cid().to_string(), Value::Str(word.into()));
                    if let (true, Some(body)) =
                        (word == "ok", record.body.and_then(|cid| given.bodies.get(&cid)))
                    {
                        hold.held.bodies.insert(record.body.unwrap(), body.clone());
                        hold.held.records.push(record);
                    }
                }
                let answer = Value::Map([("stat".to_owned(), Value::Map(stat))].into());
                ResponseTemplate::new(200).set_body_bytes(answer.encode())
            }
            ("PUT", _) => {
                let Some(Ok(cid)) =
                    path.strip_prefix("/.well-known/zu-blob/").map(str::parse::<Cid>)
                else {
                    return ResponseTemplate::new(404);
                };
                let named = hold
                    .current(&Query::default())
                    .bodies
                    .values()
                    .flat_map(zu::blob::refs)
                    .any(|b| b.cid == cid);
                match hold.refuse {
                    Some(("blob", status)) => ResponseTemplate::new(status.parse::<u16>().unwrap()),
                    _ if !named => ResponseTemplate::new(403),
                    _ if zu::blob::check(&cid, &request.body).is_err() => {
                        ResponseTemplate::new(400)
                    }
                    _ => {
                        hold.files.insert(cid, request.body.clone());
                        ResponseTemplate::new(200)
                    }
                }
            }
            _ => ResponseTemplate::new(404),
        }
    }
}

impl City {
    async fn open() -> (City, MockServer) {
        let (city, server) = (City::default(), MockServer::start().await);
        Mock::given(any()).respond_with(city.clone()).mount(&server).await;
        (city, server)
    }

    /// An event in its calendar, by its organizer: its path.
    fn event(&self, name: &str, body: serde_json::Value) -> String {
        let body = Value::from_json(&body.to_string()).unwrap();
        let (cid, path) = (
            Cid::of(Codec::Drisl, &body.encode()),
            zu::Path::new(ORGANIZER, "ev/event", name).unwrap(),
        );
        let record =
            Record::sign(&key(ORGANIZER), path.clone(), Some(cid), [], NOW, Some(CITY)).unwrap();
        let mut hold = self.0.lock().unwrap();
        hold.held.records.push(record);
        hold.held.bodies.insert(cid, body);
        path.to_string()
    }

    /// The bodies of the records the city holds now at the paths of `key`, by type and key.
    fn bodies(&self, key: &str) -> BTreeMap<(String, String), serde_json::Value> {
        let held =
            self.0.lock().unwrap().current(&Query { by: Some(ME.into()), ..Query::default() });
        let body = |r: &Record| {
            serde_json::from_str(&held.bodies[&r.body.unwrap()].to_json().unwrap()).unwrap()
        };
        let ours = held.records.iter().filter(|r| r.path.key.starts_with(key));
        ours.map(|r| ((r.path.ty.clone(), r.path.key.clone()), body(r))).collect()
    }
}

fn filed(state: &State) -> Option<&Filing> {
    state.open.as_ref()?.filed.as_ref().filter(|f| !f.state.is_empty())
}

#[test]
fn a_city_is_named_by_its_domain_or_by_a_port_of_this_machine() {
    assert_eq!(city::origin(" pol.4seas.org/ "), Ok("https://pol.4seas.org".into()));
    assert_eq!(city::origin("https://Pol.4seas.org"), Ok("https://pol.4seas.org".into()));
    assert_eq!(city::origin("http://127.0.0.1:8080"), Ok("http://127.0.0.1:8080".into()));
    for not in ["http://pol.4seas.org", "pol.example", "localhost", "http://localhost:x", "a b", ""]
    {
        assert_eq!(city::origin(not), Err("city.address"), "{not}");
    }
}

#[test]
fn what_goes_is_the_persons_own_records_meant_for_the_city_and_never_the_note() {
    let _store = STORE.blocking_lock();
    signed_in();
    let data = tempfile::tempdir().unwrap();
    let id = recording(data.path(), 1_000);
    let read = shelf(data.path()).read(&id).unwrap().unwrap();
    let me = Me { did: ME.into(), key: Arc::new(key(ME)) };
    let filing = Filing {
        did: CITY.into(),
        event: "zu://did:zu:organizer/ev/event/e1".into(),
        key: "f".repeat(32),
        ..Filing::default()
    };
    let file = Blob::of(b"RIFF", "audio/wav").unwrap();
    let held = city::records(&me, &filing, &read, &[(0, file.clone())], NOW).unwrap();
    let keys: Vec<_> = held
        .records
        .iter()
        .map(|r| (r.path.ty.as_str(), r.path.key.replace(&filing.key, "f")))
        .collect();
    assert_eq!(
        keys,
        [
            ("talk/recording", "f".into()),
            ("talk/line", "f.1.00000".into()),
            ("talk/line", "f.1.00001".into()),
            ("talk/speaker", "f.1".into())
        ]
    );
    for (n, record) in held.records.iter().enumerate() {
        assert_eq!(
            (record.path.by.as_str(), record.to.as_deref(), record.at),
            (ME, Some(CITY), NOW + n as i64)
        );
        record.verify(|_, pubkey, _| *pubkey == key(ME).public(), |_| None).unwrap();
    }
    let text: Vec<String> = held.bodies.values().map(|body| body.to_json().unwrap()).collect();
    assert!(text.iter().all(|body| !body.contains("Only mine") && !body.contains(&id)), "{text:?}");
    let head = &held.bodies[&held.records[0].body.unwrap()];
    assert!(zu::blob::refs(head) == [file], "{head:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_recording_goes_to_an_event_of_the_city_with_its_audio_and_again_without() {
    let _store = STORE.lock().await;
    signed_in();
    let (city, server) = City::open().await;
    let event = city.event("e1", json!({ "title": "Quarterly review", "start": NOW - 60_000 }));
    city.event("e2", json!({ "start": NOW })); // no title: it cannot be chosen
    let data = tempfile::tempdir().unwrap();
    let id = recording(data.path(), 300_000 + 4_000);
    let mut app = App::with(config(data.path()));
    app.until(|s| s.library.len() == 1).await;
    // Filed nowhere yet: there is no city to file to, and what is no city's address is said.
    app.send(Command::File { id: id.clone(), event: event.clone(), audio: true });
    app.notice("city.none").await;
    app.send(Command::City { address: "not a city".into() });
    app.notice("city.address").await;
    let silent = MockServer::start().await;
    app.send(Command::City { address: silent.uri() });
    app.notice("city.unknown").await;
    app.send(Command::City { address: server.uri() });
    let calendar = app.until(|s| s.city.is_some()).await.city.unwrap();
    assert_eq!((calendar.did.as_str(), calendar.events.len()), (CITY, 1));
    assert_eq!(
        (calendar.events[0].path.as_str(), calendar.events[0].title.as_str()),
        (event.as_str(), "Quarterly review")
    );
    // Filed with its audio: the records first, then two files, each part of the audio as WAV.
    app.send(Command::Open { id: id.clone() });
    app.send(Command::File { id: id.clone(), event: event.clone(), audio: true });
    app.until(|s| s.filing.iter().any(|w| w.id == id)).await;
    app.notice("city.filed").await;
    let state = app.until(|s| filed(s).is_some() && s.filing.is_empty()).await;
    let filing = filed(&state).unwrap().clone();
    assert_eq!(
        (filing.state.as_str(), filing.audio, filing.did.as_str()),
        ("city.filed", true, CITY)
    );
    assert_eq!(
        (filing.title.as_str(), filing.event.as_str()),
        ("Quarterly review", event.as_str())
    );
    assert!(!filing.key.contains(&id) && filing.key.len() == 32);
    let bodies = city.bodies(&filing.key);
    let head = &bodies[&("talk/recording".into(), filing.key.clone())];
    assert_eq!(
        (head["title"].as_str(), head["event"].as_str(), head["take"].as_u64()),
        (Some("Budget"), Some(&*event), Some(1))
    );
    let line = &bodies[&("talk/line".into(), format!("{}.1.00001", filing.key))];
    assert_eq!(
        (line["text"].as_str(), line["translations"]["th"].as_str()),
        (Some("Thanks"), Some("ขอบคุณ"))
    );
    assert_eq!(bodies[&("talk/speaker".into(), format!("{}.1", filing.key))]["text"], "Chair");
    assert_eq!(bodies.len(), 4, "{:?}", bodies.keys()); // the note stays home
    let parts = head["audio"].as_array().unwrap();
    assert_eq!(parts.iter().map(|p| p["t0"].as_u64().unwrap()).collect::<Vec<_>>(), [0, 300_000]);
    let files = city.0.lock().unwrap().files.clone();
    assert_eq!(files.len(), 2);
    for (part, (t0, blocks)) in parts.iter().zip([(0, 3_000), (3_000, 40)]) {
        let cid: Cid = part["file"]["ref"]["$link"].as_str().unwrap().parse().unwrap();
        let wav = &files[&cid];
        assert_eq!((&wav[..4], wav.len()), (&b"RIFF"[..], 44 + blocks * 3_200));
        assert_eq!(i16::from_le_bytes([wav[44], wav[45]]), t0 as i16); // where the part starts
    }
    // Filed again without its audio: what goes now replaces what went, and names no file.
    app.send(Command::File { id: id.clone(), event: event.clone(), audio: false });
    app.notice("city.filed").await;
    let state = app.until(|s| filed(s).is_some_and(|f| !f.audio) && s.filing.is_empty()).await;
    assert_eq!(filed(&state).unwrap().key, filing.key);
    let head = &city.bodies(&filing.key)[&("talk/recording".into(), filing.key.clone())];
    assert_eq!(head["audio"], json!([]));
}

#[tokio::test(flavor = "multi_thread")]
async fn each_refusal_is_said_and_kept_and_the_audio_alone_can_be_refused() {
    let _store = STORE.lock().await;
    signed_in();
    let (city, server) = City::open().await;
    let event = city.event("e1", json!({ "title": "Open mic" }));
    let data = tempfile::tempdir().unwrap();
    let id = recording(data.path(), 2_000);
    let mut app = App::with(config(data.path()));
    app.until(|s| s.library.len() == 1).await;
    app.send(Command::City { address: server.uri() });
    app.until(|s| s.city.is_some()).await;
    app.send(Command::Open { id: id.clone() });
    let refused = [
        (("talk/line", "full"), "city.full"),
        (("talk/speaker", "bad"), "city.bad"),
        (("talk/recording", "nope"), "city.deny"),
        (("blob", "413"), "city.audio.big"),
        (("blob", "403"), "city.audio.deny"),
    ];
    for (refuse, code) in refused {
        city.0.lock().unwrap().refuse = Some(refuse);
        app.send(Command::File { id: id.clone(), event: event.clone(), audio: true });
        app.notice(code).await;
        app.until(|s| filed(s).is_some_and(|f| f.state == code) && s.filing.is_empty()).await;
    }
    // An event taken out of the calendar since it was read takes nothing.
    city.0.lock().unwrap().refuse = None;
    city.0.lock().unwrap().held.records.retain(|r| r.path.ty != "ev/event");
    app.send(Command::File { id: id.clone(), event, audio: true });
    app.notice("city.event.gone").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filing_a_restart_left_on_its_way_goes_on() {
    let _store = STORE.lock().await;
    signed_in();
    let (city, server) = City::open().await;
    let event = city.event("e1", json!({ "title": "Open mic" }));
    let data = tempfile::tempdir().unwrap();
    let id = recording(data.path(), 2_000);
    let key = "a".repeat(32);
    let owed = Filing {
        city: server.uri(),
        did: CITY.into(),
        event,
        title: "Open mic".into(),
        audio: true,
        key: key.clone(),
        state: String::new(),
    };
    shelf(data.path()).put("", FILING, &[(id.clone(), Some(owed))], NOW).unwrap();
    let mut app = App::with(config(data.path()));
    app.notice("city.filed").await;
    let bodies = city.bodies(&key);
    assert_eq!(bodies.len(), 4, "{:?}", bodies.keys());
    assert_eq!(city.0.lock().unwrap().files.len(), 1);
    app.send(Command::Open { id });
    let state = app.until(|s| filed(s).is_some()).await;
    assert_eq!(filed(&state).unwrap().state, "city.filed");
}
