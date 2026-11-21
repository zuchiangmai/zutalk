//! The live lane: its rules as values, then the lane against a scripted service on a local
//! socket. The last test talks to the real service, and is ignored unless asked for.

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{WebSocketStream, accept_async, connect_async};
use zutalk_core::capture::clock::{BLOCK, Block};
use zutalk_core::id::keys::{Keys, Lease, Secret, Service};
use zutalk_core::live::lane::{
    Event, Frame, Health, Mode, Setup, Stream, config, failure, frame, hand_over, overlong, plan,
    quiet, run,
};
use zutalk_core::live::lines::{Line, Token};
use zutalk_core::rt::supervise::End;

fn strings(of: &[&str]) -> Vec<String> {
    of.iter().map(|s| s.to_string()).collect()
}

fn setup(endpoint: &str) -> Setup {
    Setup {
        endpoint: endpoint.into(),
        model: "stt-rt-v6".into(),
        spoken: strings(&["en", "zh"]),
        ..Setup::default()
    }
}

fn two_way() -> Stream {
    Stream::TwoWay("en".into(), "zh".into())
}

// ---- 1. a caption column is a stream ----

#[test]
fn one_language_is_transcribed_two_are_translated_both_ways_each_further_one_one_way() {
    assert_eq!(plan(&[]), []);
    assert_eq!(plan(&strings(&["en"])), [Stream::Transcribe]);
    assert_eq!(plan(&strings(&["en", "zh"])), [two_way()]);
    assert_eq!(
        plan(&strings(&["en", "zh", "th", "ja"])),
        [two_way(), Stream::OneWay("th".into()), Stream::OneWay("ja".into())]
    );
}

// ---- 2. the first message ----

#[test]
fn the_first_message_says_who_asks_and_for_what_and_nothing_else() {
    assert_eq!(
        config("the-key", &Stream::Transcribe, &setup("")),
        json!({
            "api_key": "the-key",
            "model": "stt-rt-v6",
            "audio_format": "pcm_s16le",
            "sample_rate": 16_000,
            "num_channels": 1,
            "language_hints": ["en", "zh"],
            "enable_language_identification": true,
            "enable_speaker_diarization": true,
            "enable_endpoint_detection": true,
            "endpoint_latency_adjustment_level": 0,
            "endpoint_sensitivity": 0.0,
            "max_endpoint_delay_ms": 2_000,
        })
    );
}

#[test]
fn a_stream_asks_for_its_translation() {
    let wire = config("k", &two_way(), &setup(""));
    assert_eq!(
        wire["translation"],
        json!({"type": "two_way", "language_a": "en", "language_b": "zh"})
    );
    let wire = config("k", &Stream::OneWay("th".into()), &setup(""));
    assert_eq!(wire["translation"], json!({"type": "one_way", "target_language": "th"}));
}

/// Lesson: recognition is where the critical errors come from, so the glossary goes to the
/// service too. Members with nothing in them are left out, not sent empty.
#[test]
fn the_glossary_goes_along_and_nothing_empty_does() {
    let mut setup = setup("");
    setup.terms = strings(&["ZuTalk", "  exact spacing  "]);
    let wire = config("k", &Stream::Transcribe, &setup);
    assert_eq!(wire["context"], json!({"terms": ["ZuTalk", "  exact spacing  "]}));
    setup.translation_terms = vec![("ZuTalk".into(), "语音工具".into())];
    let wire = config("k", &Stream::Transcribe, &setup);
    assert_eq!(
        wire["context"]["translation_terms"],
        json!([{"source": "ZuTalk", "target": "语音工具"}])
    );
    setup.spoken.clear();
    let wire = config("k", &Stream::Transcribe, &setup);
    assert!(wire.get("language_hints").is_none() && wire.get("translation").is_none());
}

// ---- 3. one response ----

fn read(response: Value) -> Result<Frame, End> {
    frame(&response.to_string())
}

#[test]
fn a_response_is_its_tokens_and_what_it_ended() {
    // As an older model sent it: a translation follows the `<end>` of what it translates in
    // the same response, with no times and no speaker. The last token is still a guess.
    let got = read(json!({
        "tokens": [
            {"text": "Hello", "start_ms": 120, "end_ms": 480, "is_final": true, "confidence": 0.98,
             "translation_status": "original", "language": "en", "speaker": "1"},
            {"text": "<end>", "is_final": true},
            {"text": "你", "is_final": true, "translation_status": "translation",
             "language": "zh", "source_language": "en"},
            {"text": " wor", "start_ms": 500, "end_ms": 620, "language": "en", "speaker": "1"}
        ],
        "final_audio_proc_ms": 480,
        "total_audio_proc_ms": 640,
        "finished": false
    }));
    let hello = Token {
        text: "Hello".into(),
        start_ms: 120,
        end_ms: 480,
        is_final: true,
        speaker: "1".into(),
        language: "en".into(),
        into: None,
    };
    let ni = Token {
        text: "你".into(),
        is_final: true,
        language: "en".into(),
        into: Some("zh".into()),
        ..Token::default()
    };
    let wor =
        Token { text: " wor".into(), start_ms: 500, end_ms: 620, is_final: false, ..hello.clone() };
    let tokens = vec![hello, ni, wor];
    assert_eq!(got, Ok(Frame { tokens, ended: true, clock_ms: 640, ..Frame::default() }));
}

/// Frames as `stt-rt-v6` sent them on 2026-11-15, for synthesized speech. A translation has
/// a speaker and no times; `<end>` comes alone, after the translation of what it ends; the
/// answer to a finalize is spelled as a token with a speaker and a language; and its clock
/// then stands at exactly the audio sent, 9.6 s here: this model adds no time of its own.
const REAL_FRAMES: [&str; 5] = [
    r#"{"tokens": [{"text": "Good", "start_ms": 180, "end_ms": 240, "confidence": 1, "is_final": false, "speaker": "1", "language": "en", "translation_status": "original"}, {"text": " mor", "start_ms": 480, "end_ms": 540, "confidence": 1, "is_final": false, "speaker": "1", "language": "en", "translation_status": "original"}], "final_audio_proc_ms": 0, "total_audio_proc_ms": 1440}"#,
    r#"{"tokens": [{"text": "大", "confidence": 0.984, "is_final": true, "speaker": "1", "language": "zh", "translation_status": "translation", "source_language": "en"}, {"text": "家", "confidence": 1, "is_final": true, "speaker": "1", "language": "zh", "translation_status": "translation", "source_language": "en"}], "final_audio_proc_ms": 2280, "total_audio_proc_ms": 2400}"#,
    r#"{"tokens": [{"text": "<end>", "confidence": 1, "is_final": true, "translation_status": "none"}], "final_audio_proc_ms": 9480, "total_audio_proc_ms": 9480}"#,
    r#"{"tokens": [{"text": "<fin>", "confidence": 0, "is_final": true, "speaker": "1", "language": "en", "translation_status": "original"}], "final_audio_proc_ms": 9600, "total_audio_proc_ms": 9600}"#,
    r#"{"tokens": [], "final_audio_proc_ms": 21000, "total_audio_proc_ms": 21000, "finished": true}"#,
];

#[test]
fn the_services_own_frames_read_as_the_rules_expect() {
    let [guesses, translation, end, fin, finished] = REAL_FRAMES.map(|text| frame(text).unwrap());
    let good = Token {
        text: "Good".into(),
        start_ms: 180,
        end_ms: 240,
        speaker: "1".into(),
        language: "en".into(),
        ..Token::default()
    };
    let mor = Token { text: " mor".into(), start_ms: 480, end_ms: 540, ..good.clone() };
    assert_eq!(guesses, Frame { tokens: vec![good, mor], clock_ms: 1_440, ..Frame::default() });
    let da = Token {
        text: "大".into(),
        is_final: true,
        speaker: "1".into(),
        language: "en".into(),
        into: Some("zh".into()),
        ..Token::default()
    };
    let jia = Token { text: "家".into(), ..da.clone() };
    let expected = Frame { tokens: vec![da, jia], clock_ms: 2_400, ..Frame::default() };
    assert_eq!(translation, expected);
    assert_eq!(end, Frame { ended: true, clock_ms: 9_480, ..Frame::default() });
    assert_eq!(fin, Frame { ended: true, clock_ms: 9_600, ..Frame::default() });
    assert_eq!(finished, Frame { finished: true, clock_ms: 21_000, ..Frame::default() });
    assert_eq!(read(json!({})), Ok(Frame::default()));
}

/// The frame the service sends when a temporary key's session runs out, as captured from it.
fn session_expired() -> Value {
    json!({
        "tokens": [],
        "final_audio_proc_ms": 4080,
        "total_audio_proc_ms": 9120,
        "error_code": 403,
        "error_type": "temp_api_key_session_expired",
        "error_message": "Temporary API key session duration limit exceeded. \
            Create a new temporary API key to start a new session.",
        "more_info": "https://soniox.com/docs/api-reference/errors#temp-api-key-session-expired",
        "request_id": "7fdf405e-d0b9-4267-a7b2-1f0c920f21fb"
    })
}

/// Lesson: an error response read as "no tokens" left a lane silent with nobody told.
#[test]
fn an_error_is_never_an_empty_response() {
    assert_eq!(read(session_expired()), Err(End::Retry("cap")));
    let quota = json!({"error_code": 402, "error_type": "quota_exhausted", "request_id": "r"});
    assert_eq!(read(quota), Err(End::Fatal("quota")));
    let invalid = json!({"tokens": [], "error_code": 422, "error_type": "invalid_request"});
    assert_eq!(read(invalid), Err(End::Fatal("request")));
    // A code that is not a number is still an error.
    assert_eq!(read(json!({"error_code": "broken"})), Err(End::Fatal("request")));
    for not_the_protocol in ["", "not json", "[]", "3", "null"] {
        assert_eq!(frame(not_the_protocol), Err(End::Fatal("protocol")), "{not_the_protocol:?}");
    }
}

// ---- 4. what a failure means ----

#[test]
fn what_a_failure_means_is_a_table() {
    let table = [
        // The end of a session's allowed length is a good key reaching it, whatever its code.
        (403, "temp_api_key_session_expired", End::Retry("cap")),
        (413, "max_duration_reached", End::Retry("cap")),
        (413, "", End::Retry("cap")),
        // The same code under another name is the key being refused.
        (403, "permission_denied", End::Fatal("key")),
        (403, "", End::Fatal("key")),
        (401, "unauthenticated", End::Fatal("key")),
        (402, "organization_balance_exhausted", End::Fatal("quota")),
        (408, "request_timeout", End::Retry("timeout")),
        (429, "rate_limited", End::Retry("busy")),
        (500, "internal_error", End::Retry("service")),
        (503, "service_unavailable", End::Retry("service")),
        // It would refuse the same request again: an unsupported language, a bad model.
        (400, "invalid_request", End::Fatal("request")),
        (422, "invalid_request", End::Fatal("request")),
        (0, "", End::Fatal("request")),
        // The codes a connection is closed with.
        (1000, "", End::Retry("closed")),
        (1006, "", End::Retry("closed")),
        (1011, "", End::Retry("closed")),
        (1008, "", End::Fatal("key")),
        (4001, "", End::Fatal("key")),
        (4003, "", End::Fatal("key")),
    ];
    for (code, name, means) in table {
        assert_eq!(failure(code, name), means, "{code} {name}");
    }
}

// ---- 5. giving way before a key's session ends ----

#[test]
fn a_capped_key_gives_way_at_a_pause_in_its_last_minute_and_at_the_deadline_regardless() {
    let s = Duration::from_secs;
    let cap = s(3 * 3600);
    assert!(!hand_over(cap - s(61), cap, true));
    assert!(hand_over(cap - s(60), cap, true));
    assert!(!hand_over(cap - s(16), cap, false));
    assert!(hand_over(cap - s(15), cap, false));
    // A short session watches for a pause in its last quarter, and gives way a tenth early.
    let cap = s(40);
    assert!(!hand_over(s(29), cap, true));
    assert!(hand_over(s(30), cap, true));
    assert!(!hand_over(s(35), cap, false));
    assert!(hand_over(s(36), cap, false));
}

fn loud() -> Vec<i16> {
    (0..BLOCK).map(|i| if i % 2 == 0 { 8_000 } else { -8_000 }).collect()
}

fn silent() -> Vec<i16> {
    vec![0; BLOCK]
}

fn noise(amplitude: i16) -> Vec<i16> {
    (0..BLOCK).map(|i| if i % 2 == 0 { amplitude } else { -amplitude }).collect()
}

/// Lesson: the service's own end of a line can be a minute coming, and a change of
/// connection forced in the middle of a word lost a syllable on each side. One block far
/// below the level lately is a pause; a room's steady noise is not, and needs none.
#[test]
fn a_pause_is_a_block_near_silence_or_far_below_the_level_lately() {
    let mut level = 0.0;
    assert!(quiet(&silent(), &mut level));
    assert!(quiet(&noise(90), &mut level), "near silence, whatever came before");
    let mut level = 0.0;
    assert!((0..20).all(|_| !quiet(&loud(), &mut level)), "speech");
    assert!(quiet(&noise(400), &mut level), "20 dB under the speech before it");
    let mut level = 0.0;
    assert!((0..20).all(|_| !quiet(&noise(400), &mut level)), "a noisy room with nobody speaking");
}

// ---- 6. a line the service leaves open ----

/// Lesson: dense speech rarely gets an `<end>` (lines ran to 49 s at the median). Past ten
/// seconds a line is closed at the speaker's next pause, 200 ms of it; at 25 s regardless.
#[test]
fn a_long_line_is_closed_at_the_next_pause_and_at_the_cap_regardless() {
    assert!(!overlong(9_900, 2_000), "a short line is left to the service");
    assert!(!overlong(10_000, 100), "a gap between words is not a pause");
    assert!(overlong(10_000, 200));
    assert!(!overlong(24_900, 0));
    assert!(overlong(25_000, 0));
}

// ---- the lane against a scripted service ----

type Socket = WebSocketStream<TcpStream>;

/// Hands out a fresh key for every connection, and counts.
#[derive(Default)]
struct TestKeys {
    leased: AtomicUsize,
    refused: AtomicUsize,
    cap: Option<Duration>,
    fail: Option<End>,
}

impl Keys for TestKeys {
    async fn lease(&self, _: Service) -> Result<Lease, End> {
        let n = self.leased.fetch_add(1, SeqCst);
        match self.fail {
            Some(end) => Err(end),
            None => Ok(Lease { key: Secret::new(format!("key-{n}")), session_cap: self.cap }),
        }
    }

    fn refused(&self, _: Service) {
        self.refused.fetch_add(1, SeqCst);
    }
}

/// Long enough for anything on a local socket, and longer than any of the lane's own waits,
/// so that with the clock paused those come first.
async fn soon<T>(what: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(60), what).await.expect("it did not happen")
}

async fn listen() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    (listener, endpoint)
}

/// What the lane sent to the service.
#[derive(Debug, PartialEq)]
enum Sent {
    Text(Value),
    Audio(usize),
    Finish,
    Gone,
}

async fn sent(ws: &mut Socket) -> Sent {
    match soon(ws.next()).await {
        Some(Ok(Message::Text(text))) if text.is_empty() => Sent::Finish,
        Some(Ok(Message::Text(text))) => Sent::Text(serde_json::from_str(&text).unwrap()),
        Some(Ok(Message::Binary(pcm))) => Sent::Audio(pcm.len()),
        _ => Sent::Gone,
    }
}

fn control(kind: &str) -> Sent {
    Sent::Text(json!({"type": kind}))
}

/// The service's side of the lane's next connection, and the configuration it opened with.
async fn accept(listener: &TcpListener) -> (Socket, Value) {
    let (stream, _) = soon(listener.accept()).await.unwrap();
    let mut ws = accept_async(stream).await.unwrap();
    let Sent::Text(config) = sent(&mut ws).await else { panic!("no configuration") };
    (ws, config)
}

async fn say(ws: &mut Socket, response: Value) {
    ws.send(Message::text(response.to_string())).await.unwrap();
}

/// A settled word, as the service sends it.
fn word(text: &str, start_ms: u32, end_ms: u32) -> Value {
    json!({"text": text, "start_ms": start_ms, "end_ms": end_ms, "is_final": true,
           "language": "en", "speaker": "1"})
}

fn guess(text: &str, start_ms: u32, end_ms: u32) -> Value {
    json!({"text": text, "start_ms": start_ms, "end_ms": end_ms, "language": "en", "speaker": "1"})
}

fn end() -> Value {
    json!({"text": "<end>", "is_final": true})
}

struct Lane {
    audio: broadcast::Sender<Arc<Block>>,
    mode: watch::Sender<Mode>,
    events: mpsc::Receiver<Event>,
    task: JoinHandle<End>,
    keys: Arc<TestKeys>,
    t_ms: u32, // the capture clock
}

fn start(endpoint: &str, kind: Stream, keys: TestKeys) -> Lane {
    let (audio, heard) = broadcast::channel(64);
    let (mode, mode_rx) = watch::channel(Mode::Recording);
    let (out, events) = mpsc::channel(1024);
    let (keys, setup) = (Arc::new(keys), setup(endpoint));
    let lane_keys = keys.clone();
    // Spawned: the lane has to be `Send` to run beside the rest of the app.
    let task =
        tokio::spawn(async move { run(0, kind, &setup, lane_keys, heard, mode_rx, out).await });
    Lane { audio, mode, events, task, keys, t_ms: 0 }
}

impl Lane {
    /// The recording goes on by one block, whether or not the lane keeps up.
    fn record(&mut self, pcm: Vec<i16>) {
        let _ = self.audio.send(Arc::new(Block { t_ms: self.t_ms, pcm }));
        self.t_ms += 100;
    }

    /// Speech is recorded and reaches the service, block by block.
    async fn speak(&mut self, ws: &mut Socket, blocks: usize) {
        for _ in 0..blocks {
            self.record(loud());
            assert_eq!(sent(ws).await, Sent::Audio(BLOCK * 2));
        }
    }

    /// And so does silence.
    async fn hush(&mut self, ws: &mut Socket, blocks: usize) {
        for _ in 0..blocks {
            self.record(silent());
            assert_eq!(sent(ws).await, Sent::Audio(BLOCK * 2));
        }
    }

    async fn event(&mut self) -> Event {
        soon(self.events.recv()).await.expect("the lane said nothing more")
    }

    /// Everything the lane says up to and including the first event that `is` what is awaited.
    async fn until(&mut self, is: impl Fn(&Event) -> bool) -> Vec<Event> {
        let mut events = Vec::new();
        while !events.last().is_some_and(&is) {
            events.push(self.event().await);
        }
        events
    }

    async fn ended(self) -> (End, Vec<Event>) {
        let Lane { mut events, task, .. } = self;
        let end = soon(task).await.unwrap();
        let mut rest = Vec::new();
        while let Ok(event) = events.try_recv() {
            rest.push(event);
        }
        (end, rest)
    }
}

fn health(state: Health) -> Event {
    Event::Health { stream: 0, state }
}

fn lines(events: &[Event]) -> Vec<Line> {
    let told = events.iter().filter_map(|event| match event {
        Event::Lines { lines, .. } => Some(lines.clone()),
        _ => None,
    });
    told.flatten().collect()
}

fn texts(lines: &[Line]) -> Vec<&str> {
    lines.iter().map(|line| line.text.as_str()).collect()
}

fn is_live(event: &Event) -> bool {
    *event == health(Health::Live)
}

fn is_lines(event: &Event) -> bool {
    matches!(event, Event::Lines { .. })
}

// ---- 7. words come out as lines ----

#[tokio::test]
async fn words_come_out_as_lines_on_the_capture_clock() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, two_way(), TestKeys::default());
    lane.t_ms = 5_000; // the lane joins a recording that is five seconds in
    let (mut ws, config) = accept(&listener).await;
    assert_eq!(config["api_key"], "key-0");
    assert_eq!(config["translation"]["type"], "two_way");
    assert_eq!(lane.event().await, health(Health::Connecting));
    lane.speak(&mut ws, 5).await;
    assert_eq!(lane.event().await, health(Health::Live));

    say(&mut ws, json!({"tokens": [guess("Hel", 120, 300)], "total_audio_proc_ms": 300})).await;
    let Event::Tail { stream: 0, tail } = lane.event().await else { panic!("no open line") };
    assert_eq!((tail[0].text.as_str(), tail[0].t0, tail[0].incomplete), ("Hel", 5_120, true));

    let ni = json!({"text": "你", "is_final": true, "translation_status": "translation",
                    "language": "zh", "source_language": "en"});
    let hao = json!({"text": "好", "is_final": true, "translation_status": "translation",
                     "language": "zh", "source_language": "en"});
    let tokens = json!([word("Hello", 120, 480), end(), ni, hao]);
    say(&mut ws, json!({"tokens": tokens, "total_audio_proc_ms": 500})).await;
    let hello = Line {
        t0: 5_120,
        t1: 5_480,
        speaker: "1".into(),
        language: "en".into(),
        text: "Hello".into(),
        translations: [("zh".to_string(), "你好".to_string())].into(),
        incomplete: false,
    };
    assert_eq!(lane.event().await, Event::Lines { stream: 0, lines: vec![hello] });
    assert_eq!(lane.event().await, Event::Tail { stream: 0, tail: vec![] });
}

// ---- 8. a broken connection is opened again, not repaired ----

#[tokio::test]
async fn a_dropped_connection_is_opened_again_with_a_gap_and_nothing_twice() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 5).await;
    // The service got as far as 300 ms of the half second it was sent, then went away.
    let tokens = json!([word("one", 100, 250), guess(" two", 250, 300)]);
    say(&mut ws, json!({"tokens": tokens, "total_audio_proc_ms": 300})).await;
    lane.until(|event| matches!(event, Event::Tail { .. })).await;
    drop(ws);

    // What it was still guessing at is kept, marked as never settled.
    let seam = lane.until(is_lines).await;
    let last = &lines(&seam)[0];
    assert_eq!(
        (last.text.as_str(), last.t0, last.t1, last.incomplete),
        ("one two", 100, 300, true)
    );
    let (mut ws, config) = accept(&listener).await;
    assert_eq!(config["api_key"], "key-1", "a connection, a lease");
    lane.speak(&mut ws, 2).await;
    let back = lane.until(is_live).await;
    assert!(back.contains(&health(Health::Connecting)));
    // The 200 ms the first connection was sent and never got to are lost, and said to be.
    assert!(back.contains(&Event::Gap { stream: 0, t0: 300, t1: 500, why: "closed" }), "{back:?}");

    // The new connection counts from zero: its words land where the audio was recorded.
    say(&mut ws, json!({"tokens": [word("three", 0, 150), end()], "total_audio_proc_ms": 200}))
        .await;
    let three = &lines(&lane.until(is_lines).await)[0];
    assert_eq!((three.text.as_str(), three.t0, three.t1), ("three", 500, 650));
    // Nothing was sent twice: after its two blocks the next thing it hears is the stop.
    lane.mode.send(Mode::Stopped).unwrap();
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    say(&mut ws, json!({"tokens": [], "finished": true})).await;
    let (end, rest) = lane.ended().await;
    assert_eq!(end, End::Done);
    assert!(lines(&rest).is_empty(), "no line twice: {rest:?}");
    assert_eq!(rest.last(), Some(&health(Health::Stopped("done"))));
}

#[tokio::test]
async fn the_service_ending_a_live_stream_on_its_own_is_one_more_break() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 2).await;
    say(&mut ws, json!({"tokens": [], "finished": true, "total_audio_proc_ms": 200})).await;
    let (_ws, config) = accept(&listener).await;
    assert_eq!(config["api_key"], "key-1");
}

// ---- 9. behind is not dead ----

/// Lesson: a full queue used to be taken for a dead lane, and the captions never came back.
#[tokio::test]
async fn a_lane_the_audio_ran_away_from_skips_to_the_live_edge_and_says_what_it_lost() {
    for flood in [30, 100] {
        let (listener, endpoint) = listen().await;
        let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
        let (mut ws, _) = accept(&listener).await;
        lane.speak(&mut ws, 2).await;
        lane.until(is_live).await;
        say(&mut ws, json!({"tokens": [guess("be", 100, 160)], "total_audio_proc_ms": 200})).await;
        lane.until(|event| matches!(event, Event::Tail { .. })).await;
        // More audio than the lane may send at once (30 blocks), then more than the channel
        // holds (100): the recording never waited for it either time.
        (0..flood).for_each(|_| lane.record(loud()));
        assert_eq!(lane.event().await, health(Health::Behind));

        let t1 = 200 + flood * 100;
        lane.speak(&mut ws, 1).await;
        let gap = Event::Gap { stream: 0, t0: 200, t1, why: "behind" };
        assert_eq!(lane.until(is_live).await, [gap, health(Health::Live)]);
        // Same connection, and none of what it skipped was sent after all.
        lane.mode.send(Mode::Stopped).unwrap();
        assert_eq!(sent(&mut ws).await, Sent::Finish);
        assert_eq!(lane.keys.leased.load(SeqCst), 1);
        // The service counts only what it was sent. A word from before the hole that it
        // settles afterwards stays before it, and a word from after it is placed after it.
        let tokens = json!([word("before", 100, 160), end()]);
        say(&mut ws, json!({"tokens": tokens, "total_audio_proc_ms": 250})).await;
        let tokens = json!([word("after", 250, 300)]);
        say(&mut ws, json!({"tokens": tokens, "finished": true, "total_audio_proc_ms": 300})).await;
        let (end, rest) = lane.ended().await;
        assert_eq!(end, End::Done);
        let told = lines(&rest);
        let placed: Vec<(&str, u32)> = told.iter().map(|l| (l.text.as_str(), l.t0)).collect();
        assert_eq!(placed, [("before", 100), ("after", t1 + 50)]);
    }
}

/// Lesson: the service accepting audio is not the service keeping up. Captions fifteen
/// seconds late are not live; the lane cuts the connection and rejoins at the live edge.
#[tokio::test]
async fn a_service_that_falls_far_behind_is_left_for_the_live_edge() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 20).await;
    lane.until(is_live).await;
    say(&mut ws, json!({"tokens": [], "total_audio_proc_ms": 2_000})).await;
    // It never gets past two seconds. Fifteen seconds of audio later the lane gives up on it.
    lane.speak(&mut ws, 151).await;
    assert_eq!(sent(&mut ws).await, Sent::Gone);
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 1).await;
    let back = lane.until(is_live).await;
    assert!(
        back.contains(&Event::Gap { stream: 0, t0: 2_000, t1: 17_100, why: "behind" }),
        "{back:?}"
    );
}

// ---- 10. long lines ----

#[tokio::test]
async fn a_line_open_too_long_is_finalized_at_a_pause_and_the_next_one_counts_from_there() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 2).await;
    say(&mut ws, json!({"tokens": [word("long", 100, 160)], "total_audio_proc_ms": 200})).await;
    lane.until(|event| matches!(event, Event::Tail { .. })).await;
    // The line opened at 100 ms. A breath before it is ten seconds old closes nothing.
    lane.speak(&mut ws, 60).await;
    lane.hush(&mut ws, 3).await;
    lane.speak(&mut ws, 50).await;
    say(&mut ws, json!({"tokens": [], "total_audio_proc_ms": 11_500})).await;
    // The first 200 ms of silence after that does: the finalize follows the second block.
    lane.hush(&mut ws, 2).await;
    assert_eq!(sent(&mut ws).await, control("finalize"));
    let fin = json!({"text": "<fin>", "is_final": true});
    let tokens = json!([word(" speech", 11_400, 11_460), fin]);
    say(&mut ws, json!({"tokens": tokens, "total_audio_proc_ms": 11_700})).await;
    let long = &lines(&lane.until(is_lines).await)[0];
    assert_eq!((long.text.as_str(), long.t0, long.t1), ("long speech", 100, 11_460));

    // The pause goes on: nothing more is asked. Then speech with no pause at all runs to
    // the cap, 25 s after its line opened at 12 s.
    lane.hush(&mut ws, 3).await;
    let still =
        |heard: u32| json!({"tokens": [guess("on", 12_000, 12_060)], "total_audio_proc_ms": heard});
    say(&mut ws, still(12_000)).await;
    lane.until(|event| matches!(event, Event::Tail { tail, .. } if !tail.is_empty())).await;
    for heard in [22_000, 32_000] {
        lane.speak(&mut ws, 100).await;
        say(&mut ws, still(heard)).await;
    }
    lane.speak(&mut ws, 50).await;
    assert_eq!(sent(&mut ws).await, control("finalize"));
    // The service does not answer this one. It is not asked again at the very next pause,
    // but at the first one ten seconds on.
    lane.speak(&mut ws, 20).await;
    lane.hush(&mut ws, 2).await;
    say(&mut ws, still(39_000)).await;
    lane.speak(&mut ws, 78).await;
    lane.hush(&mut ws, 2).await;
    assert_eq!(sent(&mut ws).await, control("finalize"));
}

// ---- 11. pause ----

#[tokio::test]
async fn a_pause_finalizes_then_keeps_the_connection_alive_and_resuming_carries_on_with_it() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    lane.mode.send(Mode::Paused).unwrap();
    assert_eq!(sent(&mut ws).await, control("finalize"));
    assert_eq!(sent(&mut ws).await, control("keepalive"));
    // Then every ten seconds for as long as the pause lasts.
    tokio::time::pause();
    let paused_at = Instant::now();
    assert_eq!(sent(&mut ws).await, control("keepalive"));
    assert_eq!(sent(&mut ws).await, control("keepalive"));
    assert!(paused_at.elapsed() >= Duration::from_secs(19), "{:?}", paused_at.elapsed());
    tokio::time::resume();

    // The capture records nothing while paused, and its clock stands still. After the pause
    // the same connection carries on. (While the test's clock stood still the lane may have
    // sent more keepalives than were read.)
    lane.mode.send(Mode::Recording).unwrap();
    lane.record(loud());
    let mut next = sent(&mut ws).await;
    while next == control("keepalive") {
        next = sent(&mut ws).await;
    }
    assert_eq!(next, Sent::Audio(BLOCK * 2));
    lane.speak(&mut ws, 1).await;
    say(&mut ws, json!({"tokens": [word("back", 300, 360), end()], "total_audio_proc_ms": 500}))
        .await;
    let back = &lines(&lane.until(is_lines).await)[0];
    assert_eq!((back.text.as_str(), back.t0), ("back", 300));
    lane.mode.send(Mode::Stopped).unwrap();
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    assert_eq!(lane.keys.leased.load(SeqCst), 1);
}

// ---- 12. a key good for a limited time ----

fn capped(seconds: u64) -> TestKeys {
    TestKeys { cap: Some(Duration::from_secs(seconds)), ..TestKeys::default() }
}

/// Moves the clock on without waiting. Nothing else is due meanwhile.
async fn later(seconds: u64) {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(seconds)).await;
    tokio::time::resume();
}

/// Lesson: a key's session length is also the reservation. Shortly before it ends the lane
/// gives way to a fresh connection at a silent block, where no word is cut, and the audio
/// recorded during the change goes to the new connection: no hole.
#[tokio::test]
async fn a_capped_key_gives_way_at_a_silent_block_to_a_fresh_lease_without_a_hole() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, capped(600));
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    lane.until(is_live).await;
    later(541).await; // into its last minute
    lane.speak(&mut ws, 2).await; // speech: not yet
    lane.record(silent());
    assert_eq!(sent(&mut ws).await, Sent::Audio(BLOCK * 2));
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    // Recorded while the old connection brings its last words: for the new one.
    lane.record(loud());
    lane.record(loud());
    let tokens = json!([word("bye", 300, 400)]);
    say(&mut ws, json!({"tokens": tokens, "finished": true, "total_audio_proc_ms": 600})).await;

    let (mut ws, config) = accept(&listener).await;
    assert_eq!(config["api_key"], "key-1");
    assert_eq!(sent(&mut ws).await, Sent::Audio(BLOCK * 2));
    assert_eq!(sent(&mut ws).await, Sent::Audio(BLOCK * 2));
    let change = lane.until(is_live).await;
    assert_eq!(texts(&lines(&change)), ["bye"]);
    // A change made on purpose is no news: no gap, and not "connecting".
    let news = |event: &&Event| !matches!(event, Event::Lines { .. } | Event::Tail { .. });
    assert_eq!(change.iter().filter(news).collect::<Vec<_>>(), [&health(Health::Live)]);
    say(&mut ws, json!({"tokens": [word("hi", 0, 150), end()], "total_audio_proc_ms": 200})).await;
    assert_eq!(lines(&lane.until(is_lines).await)[0].t0, 600);
}

#[tokio::test]
async fn with_no_pause_a_capped_key_gives_way_at_the_deadline_and_an_end_counts_as_a_pause() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, capped(600));
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    later(586).await; // a quarter of a minute is left
    lane.speak(&mut ws, 1).await;
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    say(&mut ws, json!({"tokens": [], "finished": true, "total_audio_proc_ms": 400})).await;

    // In its last minute, the service's own end of a line is the pause to give way at.
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    later(541).await;
    say(&mut ws, json!({"tokens": [word("so", 0, 200), end()], "total_audio_proc_ms": 300})).await;
    assert_eq!(sent(&mut ws).await, Sent::Finish);
}

/// Lesson: the service ends a session at its allowed length with the status it gives a key
/// without permission. Read by status, every long recording ended as "key refused".
#[tokio::test]
async fn a_session_that_reaches_its_allowed_length_rolls_over_onto_a_fresh_key() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 2).await;
    lane.until(is_live).await;
    say(&mut ws, json!({"tokens": [], "total_audio_proc_ms": 200})).await;
    say(&mut ws, session_expired()).await;
    let cut = Instant::now();
    let (mut ws, config) = accept(&listener).await;
    assert_eq!(config["api_key"], "key-1");
    // Not at once: a key issued with no time on it must not spin the lane.
    assert!(cut.elapsed() >= Duration::from_millis(200), "{:?}", cut.elapsed());
    lane.speak(&mut ws, 1).await;
    assert_eq!(lane.until(is_live).await, [health(Health::Live)], "neither refused nor news");
    assert_eq!(lane.keys.refused.load(SeqCst), 0);
}

// ---- 13. what cannot be fixed by opening again ----

#[tokio::test]
async fn a_refused_key_is_reported_to_the_keys_and_ends_the_lane() {
    for refusal in [
        json!({"error_code": 401, "error_type": "unauthenticated", "error_message": "…"}),
        json!({"tokens": [], "error_code": 403, "error_type": "permission_denied"}),
    ] {
        let (listener, endpoint) = listen().await;
        let lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
        let (mut ws, _) = accept(&listener).await;
        say(&mut ws, refusal).await;
        let keys = lane.keys.clone();
        let (end, events) = lane.ended().await;
        assert_eq!(end, End::Fatal("key"));
        assert_eq!(events, [health(Health::Connecting), health(Health::Stopped("key"))]);
        assert_eq!((keys.leased.load(SeqCst), keys.refused.load(SeqCst)), (1, 1));
    }
}

#[tokio::test]
async fn a_request_the_service_rejects_ends_the_lane_and_blames_no_key() {
    let (listener, endpoint) = listen().await;
    let lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    say(&mut ws, json!({"error_code": 400, "error_type": "invalid_request"})).await;
    let keys = lane.keys.clone();
    let (end, events) = lane.ended().await;
    assert_eq!(end, End::Fatal("request"));
    assert_eq!(events.last(), Some(&health(Health::Stopped("request"))));
    assert_eq!(keys.refused.load(SeqCst), 0);
}

#[tokio::test]
async fn with_no_key_to_be_had_the_lane_ends_saying_so() {
    let keys = TestKeys { fail: Some(End::Fatal("no key")), ..TestKeys::default() };
    let (end, events) = start("ws://127.0.0.1:1", Stream::Transcribe, keys).ended().await;
    assert_eq!(end, End::Fatal("no key"));
    assert_eq!(events, [health(Health::Connecting), health(Health::Stopped("no key"))]);
}

// ---- 14. stop ----

#[tokio::test]
async fn a_stop_sends_what_was_recorded_then_waits_for_the_last_words() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    say(&mut ws, json!({"tokens": [guess("last wor", 100, 280)], "total_audio_proc_ms": 300}))
        .await;
    lane.until(|event| matches!(event, Event::Tail { .. })).await;
    // Recorded just before the stop: it still goes out, ahead of the end of the stream.
    lane.record(loud());
    lane.mode.send(Mode::Stopped).unwrap();
    assert_eq!(sent(&mut ws).await, Sent::Audio(BLOCK * 2));
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    let tokens = json!([word("last words", 100, 380)]);
    say(&mut ws, json!({"tokens": tokens, "finished": true, "total_audio_proc_ms": 400})).await;
    let (end, rest) = lane.ended().await;
    assert_eq!(end, End::Done);
    let last = &lines(&rest)[0];
    assert_eq!((last.text.as_str(), last.t1, last.incomplete), ("last words", 380, false));
    assert_eq!(rest.last(), Some(&health(Health::Stopped("done"))));
}

#[tokio::test]
async fn last_words_that_do_not_come_in_time_are_kept_as_they_stood() {
    let (listener, endpoint) = listen().await;
    let mut lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.speak(&mut ws, 3).await;
    say(&mut ws, json!({"tokens": [guess("unsettled", 100, 280)], "total_audio_proc_ms": 300}))
        .await;
    lane.until(|event| matches!(event, Event::Tail { .. })).await;
    lane.mode.send(Mode::Stopped).unwrap();
    assert_eq!(sent(&mut ws).await, Sent::Finish);
    // The service says nothing more. The wait is five seconds, not for ever.
    tokio::time::pause();
    let stopped_at = Instant::now();
    let (end, rest) = lane.ended().await;
    assert_eq!(end, End::Done);
    let waited = stopped_at.elapsed();
    assert!(waited >= Duration::from_secs(4) && waited < Duration::from_secs(6), "{waited:?}");
    let last = &lines(&rest)[0];
    assert_eq!((last.text.as_str(), last.incomplete), ("unsettled", true));
}

/// Lesson: the service refuses to finish a connection that never carried audio.
#[tokio::test]
async fn a_connection_that_never_carried_audio_ends_without_waiting() {
    let (listener, endpoint) = listen().await;
    let lane = start(&endpoint, Stream::Transcribe, TestKeys::default());
    let (mut ws, _) = accept(&listener).await;
    lane.mode.send(Mode::Stopped).unwrap();
    assert_eq!(sent(&mut ws).await, Sent::Gone, "no end of stream is sent on it");
    let (end, events) = lane.ended().await;
    assert_eq!(end, End::Done);
    assert_eq!(events, [health(Health::Connecting), health(Health::Stopped("done"))]);
}

#[tokio::test]
async fn a_stop_while_there_is_no_connection_ends_the_lane_at_once() {
    let lane = start("ws://127.0.0.1:1", Stream::Transcribe, TestKeys::default());
    tokio::time::sleep(Duration::from_millis(400)).await; // it is between two attempts
    let stopped_at = Instant::now();
    lane.mode.send(Mode::Stopped).unwrap();
    let (end, events) = lane.ended().await;
    assert_eq!(end, End::Done);
    assert!(stopped_at.elapsed() < Duration::from_millis(200), "{:?}", stopped_at.elapsed());
    assert_eq!(events.first(), Some(&health(Health::Connecting)));
    assert_eq!(events.last(), Some(&health(Health::Stopped("done"))));
}

// ---- the real service ----

/// The person's own key, every time.
struct OneKey(String);

impl Keys for OneKey {
    async fn lease(&self, _: Service) -> Result<Lease, End> {
        Ok(Lease { key: Secret::new(self.0.clone()), session_cap: None })
    }

    fn refused(&self, _: Service) {}
}

/// Speech from the system's synthesizer, as the capture clock's samples. macOS only.
fn synthesize(dir: &std::path::Path, name: &str, text: &str) -> Option<Vec<i16>> {
    use std::process::Command;
    let (aiff, wav) = (dir.join(format!("{name}.aiff")), dir.join(format!("{name}.wav")));
    Command::new("say").arg("-o").arg(&aiff).arg(text).status().ok()?.success().then_some(())?;
    let convert = ["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"];
    let converted = Command::new("afconvert").args(convert).arg(&aiff).arg(&wav).status().ok()?;
    converted.success().then_some(())?;
    let bytes = std::fs::read(wav).ok()?;
    let data = bytes.windows(4).position(|tag| tag == b"data")? + 8;
    Some(bytes[data..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

/// What passed through the relay: when, whether it came from the lane, how much audio the
/// lane had sent by then, and what was said.
type Passed = (u128, bool, u32, Value);

/// Stands between the lane and the real service and notes what passes, without the key.
async fn relay(listener: TcpListener, upstream: String, log: mpsc::UnboundedSender<Passed>) {
    while let Ok((stream, _)) = listener.accept().await {
        let mut near = accept_async(stream).await.unwrap();
        let (mut far, _) = connect_async(upstream.as_str()).await.expect("the real service");
        let (mut sent_ms, opened) = (0, std::time::Instant::now());
        loop {
            let at = opened.elapsed().as_millis();
            let read = |text: &str| serde_json::from_str::<Value>(text).unwrap_or(json!(text));
            tokio::select! {
                message = near.next() => {
                    let Some(Ok(message)) = message else { break };
                    match &message {
                        Message::Binary(_) => sent_ms += 100,
                        Message::Text(text) => {
                            let mut said = read(text);
                            if said.get("api_key").is_some() {
                                said["api_key"] = json!("<key>");
                            }
                            let _ = log.send((at, true, sent_ms, said));
                        }
                        _ => {}
                    }
                    if far.send(message).await.is_err() { break }
                }
                message = far.next() => {
                    let Some(Ok(message)) = message else { break };
                    if let Message::Text(text) = &message {
                        let _ = log.send((at, false, sent_ms, read(text)));
                    }
                    if near.send(message).await.is_err() { break }
                }
            }
        }
    }
}

const REAL: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
const FIRST: &str = "Good morning everyone. Today we are testing live captions for a business \
    meeting about the third quarter budget.";
/// Half a minute without a real pause: more than a line may stay open.
const LONG: &str = "Thank you all for joining this quarterly review, I would like to begin with \
    the sales figures for the northern region, which grew by twelve percent compared with the \
    same period last year, mainly because of the two new distribution partners we signed in \
    March, and then move on to the supply chain, where the shipping delays we saw in spring \
    have now been resolved, so that delivery times are back to eleven days on average, and \
    finally we will look at the hiring plan for the engineering team, which adds six positions \
    before the end of the year.";
const SECOND: &str = "The next item on the agenda is the delivery schedule. We expect the first \
    shipment on the ninth of December.";

/// Synthesized speech through the real service: `SONIOX_TEST_KEY` holds the key. About
/// fifteen seconds: a sentence, a silence with three short pauses in it (so three finalizes),
/// another sentence. With `SONIOX_TEST_LONG` set it is a minute: half a minute of unbroken
/// speech, seventeen seconds of silence, the sentence. With `SONIOX_TEST_FRAMES` naming a
/// file, the lane goes through a local relay that writes every frame there.
#[tokio::test]
#[ignore = "talks to the real service: needs SONIOX_TEST_KEY, and macOS to synthesize speech"]
async fn real_soniox_hears_synthesized_speech_in_two_languages() {
    let Ok(key) = std::env::var("SONIOX_TEST_KEY") else {
        return eprintln!("skipped: SONIOX_TEST_KEY is not set");
    };
    let long = std::env::var("SONIOX_TEST_LONG").is_ok();
    let dir = tempfile::tempdir().unwrap();
    let first = synthesize(dir.path(), "first", if long { LONG } else { FIRST });
    let (Some(first), Some(second)) = (first, synthesize(dir.path(), "second", SECOND)) else {
        return eprintln!("skipped: no speech synthesizer here");
    };
    let (log, mut frames) = mpsc::unbounded_channel();
    let frames_to = std::env::var("SONIOX_TEST_FRAMES").ok();
    let endpoint = if frames_to.is_some() {
        let (listener, endpoint) = listen().await;
        tokio::spawn(relay(listener, REAL.into(), log));
        endpoint
    } else {
        REAL.into()
    };

    let (audio, heard) = broadcast::channel(64);
    let (mode, mode_rx) = watch::channel(Mode::Recording);
    let (out, mut events) = mpsc::channel(64);
    let (setup, keys) = (Setup { terms: strings(&["ZuTalk"]), ..setup(&endpoint) }, OneKey(key));
    let lane = tokio::spawn(async move {
        run(0, two_way(), &setup, Arc::new(keys), heard, mode_rx, out).await
    });
    // Everything the lane says, with when it said it.
    let started = Instant::now();
    let told = tokio::spawn(async move {
        let mut told = Vec::new();
        while let Some(event) = events.recv().await {
            told.push((started.elapsed().as_millis() as u32, event));
        }
        told
    });

    // A block every 100 ms, as a recording gives them.
    let silence = if long { 170 } else { 20 };
    let second_at = (first.len() / BLOCK + 1 + silence) as u32 * 100;
    let mut pcm = first;
    pcm.resize(second_at as usize / 100 * BLOCK, 0);
    pcm.extend(second);
    pcm.resize((pcm.len() / BLOCK + 1 + 10) * BLOCK, 0);
    let pauses =
        if long { [0; 3] } else { [second_at - 1_700, second_at - 1_100, second_at - 500] };
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    for (n, block) in pcm.chunks_exact(BLOCK).enumerate() {
        tick.tick().await;
        let t_ms = n as u32 * 100;
        if t_ms > 0 && pauses.contains(&t_ms) {
            mode.send(Mode::Paused).unwrap();
        } else if t_ms > 300 && pauses.contains(&(t_ms - 300)) {
            mode.send(Mode::Recording).unwrap();
        }
        audio.send(Arc::new(Block { t_ms, pcm: block.to_vec() })).unwrap();
    }
    let recorded_ms = (pcm.len() / BLOCK) as u32 * 100;
    mode.send(Mode::Stopped).unwrap();
    let stopped = Instant::now();
    let end = timeout(Duration::from_secs(30), lane).await.expect("the lane ends").unwrap();
    let tail_took = stopped.elapsed();
    let told = told.await.unwrap();

    let mut said = Vec::new();
    let (mut first_guess, mut first_line) = (None, None);
    for (at, event) in &told {
        match event {
            Event::Lines { lines, .. } => {
                // How long after its last word a line was finished.
                first_line.get_or_insert(*at as i64 - lines[0].t1 as i64);
                said.extend(lines.iter().cloned());
                eprintln!(
                    "{at:>6} lines {:?}",
                    lines.iter().map(|l| (l.t0, l.t1)).collect::<Vec<_>>()
                );
            }
            Event::Tail { tail, .. } if !tail.is_empty() => {
                first_guess.get_or_insert(*at as i64 - tail[0].t0 as i64);
            }
            Event::Tail { .. } => {}
            other => eprintln!("{at:>6} {other:?}"),
        }
    }
    for line in &said {
        let zh = line.translations.get("zh").map(String::as_str).unwrap_or("-");
        let unsettled = if line.incomplete { " (unsettled)" } else { "" };
        eprintln!(
            "{:>6}..{:<6} [{}|{}]{unsettled} {} => {zh}",
            line.t0, line.t1, line.speaker, line.language, line.text
        );
    }
    eprintln!(
        "recorded {recorded_ms} ms; first words shown {first_guess:?} ms after they began; \
         first line finished {first_line:?} ms after it ended; the last words took {tail_took:?}"
    );
    if let Some(path) = frames_to {
        let (mut all, mut asked) = (Vec::new(), std::collections::VecDeque::new());
        while let Ok((at, up, sent_ms, said)) = frames.try_recv() {
            all.push(format!("{at:>6} {} sent {sent_ms:>6}: {said}", if up { '>' } else { '<' }));
            // Lesson S4, which the lane no longer carries: an older model ran its clock on at
            // each finalize. This one answers with exactly the audio it had been sent.
            if up && said["type"] == "finalize" {
                asked.push_back(json!(sent_ms));
            }
            let is_fin = |tokens: &Vec<Value>| tokens.iter().any(|token| token["text"] == "<fin>");
            if !up && said["tokens"].as_array().is_some_and(is_fin) {
                assert_eq!(
                    Some(&said["total_audio_proc_ms"]),
                    asked.pop_front().as_ref(),
                    "{said}"
                );
            }
        }
        std::fs::write(&path, all.join("\n")).unwrap();
        eprintln!("{} frames written to {path}", all.len());
    }

    assert_eq!(end, End::Done);
    let events: Vec<&Event> = told.iter().map(|(_, event)| event).collect();
    let news: Vec<_> =
        events.iter().filter(|e| !matches!(e, Event::Lines { .. } | Event::Tail { .. })).collect();
    let expected =
        [health(Health::Connecting), health(Health::Live), health(Health::Stopped("done"))];
    assert!(news.iter().map(|e| **e).eq(expected.iter()), "{news:?}");
    let all = texts(&said).join(" ").to_lowercase();
    for heard in [if long { "quarterly" } else { "budget" }, "agenda", "december"] {
        assert!(all.contains(heard), "{heard} is missing from: {all}");
    }
    assert!(said.iter().all(|line| line.language == "en" && !line.speaker.is_empty()), "{said:?}");
    assert!(said.iter().all(|line| line.translations.contains_key("zh")), "a line untranslated");
    assert!(said.iter().all(|line| line.t0 <= line.t1 && line.t1 <= recorded_ms), "{said:?}");
    assert!(said.iter().all(|line| line.t1 - line.t0 < 27_000), "a line longer than the cap");
    // The second sentence is placed where it was fed, whatever was finalized before it.
    let agenda = said.iter().find(|line| line.text.to_lowercase().contains("next item")).unwrap();
    let off = agenda.t0 as i64 - second_at as i64;
    eprintln!("the second sentence, fed at {second_at} ms, is placed {off} ms from there");
    assert!((-200..=500).contains(&off), "placed {off} ms off");
}
