//! Live captions. The recording's audio goes to the Soniox realtime service, one connection
//! for each caption column, and comes back as lines for the eyes: a draft, never the record.
//! So a connection that breaks is opened again, not repaired. Nothing is sent twice; what was
//! lost is told as a gap, and the lane carries on from the live edge.

use crate::capture::clock::{BLOCK, Block, RATE};
use crate::id::keys::{Keys, Service};
use crate::live::frame::MAX_COLUMNS;
use crate::live::lines::{Assembler, Boundary, Line, Token};
use crate::live::wire::{Dial, Wire};
use crate::rt::queue::{FanoutRx, RecvError, Tx};
use crate::rt::stats::{Counter, Spread, Stats};
use crate::rt::supervise::{End, Part, Restarts, supervise_counted};
use crate::rt::water::Gauge;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, OwnedMutexGuard, watch};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_tungstenite::tungstenite::{Error, Message};
use zutalk_log::{info, warn};

/// Fixed task and part names for the up to three caption lanes of one recording (`P1.md` §3.13.10).
pub const LANE_NAMES: [&str; 3] = ["lane.0", "lane.1", "lane.2"];

const BLOCK_MS: u32 = (BLOCK as u64 * 1000 / RATE) as u32; // a block's length, worked out
/// Taking a key lease, dialing, or writing a frame longer than this is a dead attempt. 0.8's.
const PATIENCE_MS: u64 = 10_000;
/// While paused the service is sent no audio, and closes a connection that says nothing: a
/// keepalive this often. 0.8's.
const KEEPALIVE_MS: u64 = 10_000;
/// How long the last words may take to come once the audio has ended. 0.8's drain time.
const TAIL_MS: u64 = 5_000;
/// Kept up over minutes, the service takes audio in at the speed of speech and no faster, so
/// what is late stays late. More blocks than this waiting (20 × 100 ms = 2 s), or a service
/// this far behind what it was sent, is not live any more. 15 s is the most 0.8 would catch up
/// (LESSONS.md, Soniox).
const BACKLOG_BLOCKS: usize = 20;
/// See above: 0.8's 15 s window to catch up in (LESSONS.md, Soniox).
const BEHIND_MS: u32 = 15_000;
/// A connection near its key's cap gives way at a pause in its last minute. 0.8's handover lead.
const HAND_OVER_LEAD_MS: u64 = 60_000;
/// …and with this much left, whether a pause came or not. 0.8's handover margin.
const HAND_OVER_MARGIN_MS: u64 = 15_000;

#[derive(Debug, Clone, Default)]
pub struct Setup {
    pub endpoint: String,
    pub model: String,                            // of the `stt-rt-` kind
    pub spoken: Vec<String>,                      // the languages expected in the room
    pub general: Vec<(String, String)>,           // structured topic facts (`context.general`)
    pub text: String,                             // bounded topic background (`context.text`)
    pub terms: Vec<String>,                       // names and words to listen for
    pub translation_terms: Vec<(String, String)>, // and how some of them are translated
}

/// What one connection is asked for. A caption column is a stream; streams are not aligned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stream {
    Transcribe,
    TwoWay(String, String), // each line with its translation into the other language
    OneWay(String),         // each line with its translation into this one
}

/// The streams that fill these columns (up to [`MAX_COLUMNS`]): one connection translates
/// into one language, or both ways between two.
pub fn plan(shown: &[String]) -> Vec<Stream> {
    let shown = &shown[..shown.len().min(MAX_COLUMNS)];
    let [a, b, more @ ..] = shown else {
        return shown.iter().map(|_| Stream::Transcribe).collect();
    };
    let both = Stream::TwoWay(a.clone(), b.clone());
    std::iter::once(both).chain(more.iter().cloned().map(Stream::OneWay)).collect()
}

/// The streams a recording opens for its captions, or the one reason it has none: the first
/// row that holds. Known before the recording starts, so a recording without captions opens
/// no connection and fails at nothing.
pub fn captions(file: bool, shown: &[String], key: bool) -> Result<Vec<Stream>, &'static str> {
    if file {
        return Err("file");
    }
    let streams = plan(shown);
    if streams.is_empty() {
        return Err("captions.off");
    }
    if !key {
        return Err("captions.no_key");
    }
    Ok(streams)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Recording,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Live,                  // audio is reaching the service
    Behind,                // it could not keep up with the audio, and skips to the live edge
    Stopped(&'static str), // for good: `done`, or the code of what ended it
}

impl Link {
    /// Stable plain code for structured log events.
    pub const fn code(&self) -> &'static str {
        match self {
            Link::Connecting => "Connecting",
            Link::Live => "Live",
            Link::Behind => "Behind",
            Link::Stopped(_) => "Stopped",
        }
    }

    /// Pure transition table: repeating a state is no change, and once `Stopped` only starting a
    /// fresh lane can leave it (`P1.md` §3.13.11).
    pub fn next(self, to: Link) -> Option<Link> {
        match (self, to) {
            (Link::Stopped(_), _) => None,
            (Link::Connecting, Link::Connecting)
            | (Link::Live, Link::Live)
            | (Link::Behind, Link::Behind) => None,
            (Link::Connecting | Link::Live | Link::Behind, next) => Some(next),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Lines { stream: usize, lines: Vec<Line> }, // finished: they will not change
    Tail { stream: usize, tail: Vec<Line> },   // still open: each one replaces the last
    Gap { stream: usize, t0: u32, t1: u32, why: &'static str }, // no text for this stretch
    Link { stream: usize, state: Link },
}

/// The first message on a connection: who is asking, and for what.
pub fn config(key: &str, kind: &Stream, setup: &Setup) -> Value {
    let translation = match kind {
        Stream::Transcribe => Value::Null,
        Stream::TwoWay(a, b) => json!({"type": "two_way", "language_a": a, "language_b": b}),
        Stream::OneWay(into) => json!({"type": "one_way", "target_language": into}),
    };
    let general: Vec<Value> = setup
        .general
        .iter()
        .filter(|(k, v)| !k.trim().is_empty() && !v.trim().is_empty())
        .map(|(k, v)| json!({"key": k, "value": v}))
        .collect();
    let text = if setup.text.trim().is_empty() { Value::Null } else { json!(setup.text) };
    let pair = |(source, target): &(String, String)| json!({"source": source, "target": target});
    let pairs: Vec<Value> = setup.translation_terms.iter().map(pair).collect();
    #[rustfmt::skip]
    let config = json!({
        "api_key": key, "model": setup.model,
        "audio_format": "pcm_s16le", "sample_rate": RATE, "num_channels": 1,
        // Hints, not limits: a language nobody expected is still written down.
        "language_hints": setup.spoken, "enable_language_identification": true,
        "enable_speaker_diarization": true,
        // The service ends a line where the speaker pauses. These are its balanced settings.
        "enable_endpoint_detection": true, "max_endpoint_delay_ms": 2_000,
        "endpoint_latency_adjustment_level": 0, "endpoint_sensitivity": 0.0,
        "context": stated(json!({
            "general": general,
            "text": text,
            "terms": setup.terms,
            "translation_terms": pairs,
        })),
        "translation": translation,
    });
    stated(config)
}

/// Without the members that say nothing: the service takes absent, not null or empty.
fn stated(mut value: Value) -> Value {
    let empty = |v: &Value| v.is_null() || [json!([]), json!({})].contains(v);
    value.as_object_mut().into_iter().for_each(|members| members.retain(|_, v| !empty(v)));
    value
}

/// One response from the service.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub tokens: Vec<Token>, // their times are on the service's clock
    pub ended: bool,        // `<end>`, or `<fin>` when asked to: the service closed its lines
    pub clock_ms: u32,      // how much of the audio it has taken in
    pub finished: bool,     // nothing more will come
}

/// Reads one response. An error in it is how the connection ends; what is not a JSON
/// object is not the protocol.
pub fn reply(text: &str) -> Result<Reply, End> {
    let wire = Value::Object(serde_json::from_str(text).map_err(|_| End::Fatal("protocol"))?);
    let string = |of: &Value| of.as_str().unwrap_or_default().to_string();
    let ms = |of: &Value| of.as_u64().unwrap_or(0) as u32;
    // An error is one whatever its code looks like: never an empty response.
    if !wire["error_code"].is_null() {
        return Err(failure(ms(&wire["error_code"]) as u16, &string(&wire["error_type"])));
    }
    let (clock_ms, finished) = (ms(&wire["total_audio_proc_ms"]), wire["finished"] == true);
    let mut reply = Reply { clock_ms, finished, ..Reply::default() };
    for token in wire["tokens"].as_array().into_iter().flatten() {
        let translation = token["translation_status"] == "translation";
        match token["text"].as_str().unwrap_or_default() {
            "<end>" | "<fin>" => reply.ended = true,
            text => reply.tokens.push(Token {
                text: text.into(),
                start_ms: ms(&token["start_ms"]),
                end_ms: ms(&token["end_ms"]),
                is_final: token["is_final"] == true,
                speaker: string(&token["speaker"]),
                // A translation is labelled with the language it is in, and names the one spoken.
                language: string(&token[if translation { "source_language" } else { "language" }]),
                into: translation.then(|| string(&token["language"])),
            }),
        }
    }
    Ok(reply)
}

/// What the service's error, or the code it closed the connection with, means for the lane.
pub fn failure(code: u16, name: &str) -> End {
    match (code, name) {
        // The key's session ran its length, or the service's longest did. It shares 403 with
        // a key that lacks permission: only the name tells that the key was good.
        (_, "temp_api_key_session_expired" | "max_duration_reached") | (413, _) => {
            End::Retry("cap")
        }
        (401 | 403 | 1008 | 4001 | 4003, _) => End::Fatal("key"),
        (402, _) => End::Fatal("quota"),
        (408, _) => End::Retry("timeout"),
        (429, _) => End::Retry("busy"),
        (500..=599, _) => End::Retry("service"),
        (1000.., _) => End::Retry("closed"),
        _ => End::Fatal("request"), // it would refuse the same request again
    }
}

/// Whether a connection this old, on a key good for `cap`, gives way to a fresh one now: at
/// a pause in its last minute, and with a quarter of a minute left whether one came or not.
pub fn hand_over(age: Duration, cap: Duration, pause: bool) -> bool {
    let lead = Duration::from_millis(HAND_OVER_LEAD_MS).min(cap / 4);
    let margin = Duration::from_millis(HAND_OVER_MARGIN_MS).min(cap / 10);
    age + margin >= cap || (pause && age + lead >= cap)
}

/// Whether a block is a pause in the speech: near silence, or 20 dB under the level lately,
/// which `level` keeps from block to block. A cut there falls between words.
pub fn quiet(pcm: &[i16], level: &mut f64) -> bool {
    let power: f64 = pcm.iter().map(|&sample| f64::from(sample).powi(2)).sum();
    let rms = (power / pcm.len().max(1) as f64).sqrt();
    let quiet = rms <= 100.0 || rms <= *level * 0.1;
    *level = if *level == 0.0 { rms } else { *level * 0.95 + rms * 0.05 };
    quiet
}

/// Whether to have the service close a line open for `open_ms`, the audio `quiet_ms` into a
/// pause. Dense speech rarely gets it to end one by itself: past ten seconds the line is
/// closed at the speaker's next pause, and at twenty-five wherever it stands.
pub fn overlong(open_ms: u32, quiet_ms: u32) -> bool {
    open_ms >= 25_000 || (open_ms >= 10_000 && quiet_ms >= 200)
}

/// What one connection was sent, and how far the service has got with it.
#[derive(Default)]
struct Feed {
    sent: u32,                // ms of audio
    anchors: Vec<(u32, u32)>, // from this much audio on, the capture time it was recorded at
    heard: u32,               // where the service's clock stands
    asked: u32,               // `sent` when it was last asked to finalize
    hush: u32,                // ms of pause the audio ends in
    level: f64,               // how loud it has been lately
}

impl Feed {
    /// A time on the service's clock, on the capture clock. The service counts the audio it
    /// is sent, from zero: what was paused over or skipped is not in its count. (An older
    /// model also ran its clock on by a tenth of a second at each finalize. `stt-rt-v6`
    /// answers one with exactly the audio it was sent, so there is nothing to take back.)
    fn capture(&self, ms: u32) -> u32 {
        let ms = ms.min(self.sent);
        let anchor = self.anchors.iter().rev().find(|(from, _)| *from <= ms);
        anchor.map_or(ms, |(from, t_ms)| t_ms + (ms - from))
    }
}

/// Where a lane begins on the capture clock (`edge = 0, why = "start"` at recording start, or
/// the previous `Sent` position and break code when restarted by `Command::Reconnect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub edge: u32,
    pub why: &'static str,
}

impl Default for Origin {
    fn default() -> Self {
        Self { edge: 0, why: "start" }
    }
}

/// Watermark gauges reported by one caption lane (`P1.md` §3.5).
#[derive(Clone, Default)]
pub struct Gauges {
    pub sent: Arc<Gauge>,
    pub heard: Arc<Gauge>,
    pub final_at: Arc<Gauge>,
}

/// One stream, across its connections. A connection holds it for as long as it lives.
struct Lane<K> {
    stream: usize,
    kind: Stream,
    setup: Setup,
    keys: Arc<K>,
    dial: Dial,
    gaps: Counter,
    keys_ms: Spread,
    dial_ms: Spread,
    gauges: Gauges,
    out: Tx<Event>,
    audio: FanoutRx<Arc<Block>>,
    mode: watch::Receiver<Mode>,
    kick: watch::Receiver<u64>,
    lines: Assembler,
    tail: Vec<Line>,   // the open lines, as last told
    edge: u32,         // the capture time text is accounted for up to
    why: &'static str, // why none has come since; empty while it does
    link: Link,        // the link state last said
    paused: bool,
}

/// Runs one stream from the start of a recording (`Origin::default()`).
#[allow(clippy::too_many_arguments)]
pub async fn run<K: Keys>(
    stream: usize,
    kind: Stream,
    setup: &Setup,
    keys: Arc<K>,
    dial: Dial,
    stats: Stats,
    gauges: Gauges,
    audio: FanoutRx<Arc<Block>>,
    mode: watch::Receiver<Mode>,
    kick: watch::Receiver<u64>,
    out: Tx<Event>,
) -> End {
    run_from(
        stream,
        kind,
        setup,
        keys,
        dial,
        stats,
        gauges,
        Origin::default(),
        audio,
        mode,
        kick,
        out,
    )
    .await
}

/// Runs one stream from `origin` until the recording stops or nothing can keep it going. Audio
/// capture never waits on it: a lane that cannot keep up skips, and one that ends says so.
#[allow(clippy::too_many_arguments)]
pub async fn run_from<K: Keys>(
    stream: usize,
    kind: Stream,
    setup: &Setup,
    keys: Arc<K>,
    dial: Dial,
    stats: Stats,
    gauges: Gauges,
    origin: Origin,
    audio: FanoutRx<Arc<Block>>,
    mode: watch::Receiver<Mode>,
    kick: watch::Receiver<u64>,
    out: Tx<Event>,
) -> End {
    let spoken: Vec<&str> = setup.spoken.iter().map(String::as_str).collect();
    let (lines, tail, mut stop) = (Assembler::new(&spoken), Vec::new(), mode.clone());
    let (setup, edge, why, paused) =
        (setup.clone(), origin.edge, origin.why, *mode.borrow() == Mode::Paused);
    gauges.sent.because(why);
    let gaps = stats.counter("lane.gaps");
    let keys_ms = stats.spread("lane.keys_ms");
    let dial_ms = stats.spread("lane.dial_ms");
    let broke_sent = gauges.sent.clone();
    let lane = Lane {
        stream,
        kind,
        setup,
        keys,
        dial,
        gaps,
        keys_ms,
        dial_ms,
        gauges,
        out,
        audio,
        mode,
        kick,
        lines,
        tail,
        edge,
        why,
        link: Link::Connecting,
        paused,
    };
    let lane = Arc::new(Mutex::new(lane));
    let stopped = async {
        let _ = stop.wait_for(|mode| *mode == Mode::Stopped).await; // dropped: sender dropped means the recording has already stopped
        // A connection has this long to bring the last words. With none, there are none.
        let last_words_ms = TAIL_MS + PATIENCE_MS;
        let _ = timeout(Duration::from_millis(last_words_ms), lane.lock()).await; // dropped: timeout expiry just means no last words arrived in time
    };
    // What `supervise` is given owns what it uses: borrowing here, the lane could not be
    // proved `Send`, and so could not run beside the rest of the app.
    let name = LANE_NAMES[stream];
    let (opener, broken) = (lane.clone(), lane.clone());
    let broke = move |why| {
        broke_sent.because(why);
        if let Ok(mut lane) = broken.try_lock() {
            lane.why = why;
            if why == "panic" {
                let spoken: Vec<&str> = lane.setup.spoken.iter().map(String::as_str).collect();
                lane.lines = Assembler::new(&spoken);
                lane.tail.clear();
            }
        }
    };
    let end = tokio::select! {
        biased;
        end = supervise_counted(
            name,
            async move || open(opener.clone().lock_owned().await).await,
            broke,
            Restarts::default(),
            &stats,
        ) => end,
        () = stopped => End::Done,
    };
    let mut lane = lane.lock().await;
    lane.seam().await;
    let code = if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" };
    lane.gauges.sent.ended(code);
    lane.say(Link::Stopped(code)).await;
    end
}

fn end_code(end: End) -> &'static str {
    if let End::Retry(code) | End::Fatal(code) = end { code } else { "done" }
}

async fn open<K: Keys>(mut lane: OwnedMutexGuard<Lane<K>>) -> Result<Conn<K>, End> {
    if lane.why == "panic" {
        lane.tell(Event::Tail { stream: lane.stream, tail: Vec::new() }).await;
    }
    // A connection that gave way on purpose is no news, unless the next one fails.
    if lane.why != "cap" {
        lane.say(Link::Connecting).await;
    }
    let stream = lane.stream;
    let t_keys = Instant::now();
    let leased =
        timeout(Duration::from_millis(PATIENCE_MS), lane.keys.lease(Service::Soniox)).await;
    let keys_ms = t_keys.elapsed().as_millis() as u64;
    lane.keys_ms.note(keys_ms);
    let mut dial_ms = 0u64;

    let result: Result<Conn<K>, End> = async {
        let lease = match leased {
            Ok(Ok(lease)) => lease,
            Ok(Err(end)) => return Err(end),
            Err(_) => return Err(End::Retry("keys")),
        };
        let dialed = Instant::now(); // before the socket opens: the service's count starts later
        let dial =
            timeout(Duration::from_millis(PATIENCE_MS), (lane.dial)(lane.setup.endpoint.as_str()))
                .await;
        dial_ms = dialed.elapsed().as_millis() as u64;
        lane.dial_ms.note(dial_ms);
        let ws = match dial {
            Ok(Ok(ws)) => ws,
            Ok(Err(end)) => return Err(end),
            Err(_) => return Err(End::Retry("connect")),
        };
        let hello = config(lease.key.expose(), &lane.kind, &lane.setup).to_string();
        let (cap, feed) = (lease.session_cap, Feed::default());
        let mut conn = Conn { lane, ws, feed, dialed, cap, ending: None, wake: dialed };
        conn.send(Message::text(hello)).await?;
        Ok(conn)
    }
    .await;

    match &result {
        Ok(_) => {
            info!("lane.open", lane = stream, code = "ok", keys_ms = keys_ms, dial_ms = dial_ms,)
        }
        Err(end) => warn!(
            "lane.open",
            lane = stream,
            code = end_code(*end),
            keys_ms = keys_ms,
            dial_ms = dial_ms,
        ),
    }
    result
}

impl<K: Keys> Lane<K> {
    async fn tell(&self, event: Event) {
        let _ = self.out.send(event).await; // dropped: nobody listening is not this lane's failure
    }

    async fn say(&mut self, state: Link) {
        if let Some(next) = self.link.next(state) {
            let (from, to) = (self.link.code(), next.code());
            let why = match next {
                Link::Stopped(code) => code,
                _ => self.why,
            };
            if matches!(next, Link::Behind) || matches!(next, Link::Stopped(code) if code != "done")
            {
                warn!("lane.link", lane = self.stream, from = from, to = to, why = why);
            } else {
                info!("lane.link", lane = self.stream, from = from, to = to, why = why);
            }
            self.link = next;
        }
        self.tell(Event::Link { stream: self.stream, state }).await;
    }

    /// Tells the lines that finished, and the open ones when they changed.
    async fn show(&mut self, lines: Vec<Line>) {
        if !lines.is_empty() {
            if let Some(last) = lines.last() {
                self.gauges.final_at.set(u64::from(last.t1));
            }
            self.tell(Event::Lines { stream: self.stream, lines }).await;
        }
        let tail = self.lines.tail();
        if tail != self.tail {
            self.tail.clone_from(&tail);
            self.tell(Event::Tail { stream: self.stream, tail }).await;
        }
    }

    /// Nothing more will come for the open lines. What the service was still guessing at is
    /// the only record of that speech, and is kept.
    async fn seam(&mut self) {
        let last = self.lines.tail();
        self.lines.push(&[], Boundary::Seam);
        self.show(last).await;
    }
}

struct Conn<K> {
    lane: OwnedMutexGuard<Lane<K>>,
    ws: Wire,
    feed: Feed,
    dialed: Instant,
    cap: Option<Duration>, // how long the key lets one connection last
    ending: Option<End>,   // the audio is over: how this ends once the last words are in
    wake: Instant,         // the next keepalive while paused; the end of the wait when ending
}

impl<K> Drop for Conn<K> {
    fn drop(&mut self) {
        // Text is accounted for as far as the service got, not as far as it was sent.
        if self.feed.sent > 0 {
            self.lane.edge = self.lane.edge.min(self.feed.capture(self.feed.heard));
        }
    }
}

impl<K: Keys> Part for Conn<K> {
    async fn run(&mut self) -> End {
        let Err(end) = self.live().await;
        self.lane.seam().await;
        if end == End::Fatal("key") {
            self.lane.keys.refused(Service::Soniox);
        }
        end
    }
}

impl<K: Keys> Conn<K> {
    async fn live(&mut self) -> Result<Infallible, End> {
        // Ignore any kick issued before this connection opened.
        self.lane.kick.mark_unchanged();
        let mut kick_open = true;
        loop {
            let asleep = self.lane.paused || self.ending.is_some();
            let Lane { audio, mode, kick, .. } = &mut *self.lane;
            tokio::select! {
                biased;
                // Responses before audio: they say how far the service has got. Audio before
                // the mode: what was recorded ahead of a pause or the stop still goes out, and
                // the capture records nothing while paused.
                message = self.ws.next() => self.hear(message).await?,
                block = audio.recv(), if self.ending.is_none() => self.take(block).await?,
                changed = kick.changed(), if kick_open && self.ending.is_none() => {
                    match changed {
                        Ok(()) => {
                            kick.mark_unchanged();
                            return Err(End::Retry("silent"));
                        }
                        Err(_) => kick_open = false,
                    }
                }
                changed = mode.changed(), if self.ending != Some(End::Done) => {
                    let mode = changed.map_or(Mode::Stopped, |()| *mode.borrow_and_update());
                    self.lane.paused = mode == Mode::Paused;
                    match mode {
                        Mode::Stopped => self.finish(End::Done).await?,
                        Mode::Paused if self.feed.sent > 0 && self.ending.is_none() => {
                            self.finalize().await?;
                        }
                        _ => {}
                    }
                }
                () = sleep_until(self.wake), if asleep => {
                    if let Some(end) = self.ending {
                        return Err(end); // the last words did not come in time
                    }
                    self.wake = Instant::now() + Duration::from_millis(KEEPALIVE_MS);
                    self.send(Message::text(r#"{"type":"keepalive"}"#)).await?;
                }
            }
        }
    }

    async fn send(&mut self, message: Message) -> Result<(), End> {
        let sent = timeout(Duration::from_millis(PATIENCE_MS), self.ws.send(message)).await;
        if matches!(sent, Ok(Ok(()))) { Ok(()) } else { Err(End::Retry("socket")) }
    }

    /// Has the service settle the lines it has open. It answers with `<fin>`.
    async fn finalize(&mut self) -> Result<(), End> {
        self.feed.asked = self.feed.sent;
        self.send(Message::text(r#"{"type":"finalize"}"#)).await
    }

    /// No more audio for this connection: an empty frame has the service settle everything
    /// and say `finished`. One that carried none has no last words, and is refused that.
    async fn finish(&mut self, end: End) -> Result<(), End> {
        if self.feed.sent == 0 {
            return Err(end);
        }
        if self.ending.replace(end).is_some() {
            return Ok(());
        }
        self.wake = Instant::now() + Duration::from_millis(TAIL_MS);
        self.send(Message::text("")).await
    }

    fn due(&self, pause: bool) -> bool {
        self.ending.is_none()
            && self.cap.is_some_and(|cap| hand_over(self.dialed.elapsed(), cap, pause))
    }

    async fn hear(&mut self, message: Option<Result<Message, Error>>) -> Result<(), End> {
        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(Some(close)))) => return Err(failure(close.code.into(), "")),
            Some(Ok(Message::Close(None)) | Err(_)) | None => return Err(End::Retry("closed")),
            Some(Ok(_)) => return Ok(()),
        };
        let (mut reply, feed) = (reply(&text)?, &mut self.feed);
        for token in reply.tokens.iter_mut().filter(|token| token.into.is_none()) {
            (token.start_ms, token.end_ms) =
                (feed.capture(token.start_ms), feed.capture(token.end_ms));
        }
        feed.heard = feed.heard.max(reply.clock_ms);
        let heard_cap = feed.capture(feed.heard);
        self.lane.gauges.heard.set(u64::from(heard_cap));
        let boundary = if reply.ended { Boundary::Provider } else { Boundary::None };
        let lines = self.lane.lines.push(&reply.tokens, boundary);
        self.lane.show(lines).await;
        if reply.finished {
            return Err(self.ending.unwrap_or(End::Retry("finished")));
        }
        if reply.ended && self.due(true) {
            self.finish(End::Retry("cap")).await?;
        }
        Ok(())
    }

    async fn take(&mut self, block: Result<Arc<Block>, RecvError>) -> Result<(), End> {
        let (lane, feed) = (&mut *self.lane, &mut self.feed);
        let block = match block {
            Ok(block) if lane.audio.len() <= BACKLOG_BLOCKS => block,
            Err(RecvError::Closed) => return self.finish(End::Done).await,
            // Behind, not dead: what waits is skipped, and it carries on from the live edge.
            _ => {
                (lane.audio, lane.why) = (lane.audio.resubscribe(), "behind");
                lane.gauges.sent.because("behind");
                lane.say(Link::Behind).await;
                return Ok(());
            }
        };
        if !lane.why.is_empty() {
            if lane.why != "start" && block.t_ms > lane.edge {
                let (stream, t0, t1, why) = (lane.stream, lane.edge, block.t_ms, lane.why);
                lane.gaps.add(1);
                warn!("lane.gap", lane = stream, t0_ms = t0, t1_ms = t1, why = why);
                lane.tell(Event::Gap { stream, t0, t1, why }).await;
            }
            lane.why = "";
            lane.gauges.sent.because("");
            lane.say(Link::Live).await;
        }
        lane.edge = block.t_ms + BLOCK_MS;
        let pause = quiet(&block.pcm, &mut feed.level);
        feed.hush = if pause { feed.hush + BLOCK_MS } else { 0 };
        if feed.capture(feed.sent) != block.t_ms {
            feed.anchors.push((feed.sent, block.t_ms));
        }
        feed.sent += BLOCK_MS;
        let open = lane.tail.first().map_or(0, |line| lane.edge.saturating_sub(line.t0));
        let close = overlong(open.min(feed.sent - feed.asked), feed.hush);
        let behind = feed.sent.saturating_sub(feed.heard) > BEHIND_MS;
        // As it comes, never faster.
        let pcm: Vec<u8> = block.pcm.iter().flat_map(|sample| sample.to_le_bytes()).collect();
        self.send(Message::binary(pcm)).await?;
        self.lane.gauges.sent.set(u64::from(self.lane.edge));
        if behind {
            return Err(End::Retry("behind"));
        }
        if close {
            self.finalize().await?;
        }
        if self.due(pause) {
            self.finish(End::Retry("cap")).await?;
        }
        Ok(())
    }
}
