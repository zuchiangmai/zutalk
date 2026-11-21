use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};
use zu::SecretKey;
use zutalk_core::clock::{BLOCK, Block};
use zutalk_core::keys::{Keys, Lease, Secret, Service};
use zutalk_core::lines::{Line, Token};
use zutalk_core::shelf::{About, RECORDING, Said, Shelf};
use zutalk_core::supervise::End;
use zutalk_core::tape::Writer;
use zutalk_core::write::{
    Answer, Draft, GUARDS, Journal, MODELS, Plan, answers, ends, judge, lines, numbers, request,
    status, sweep, tokens, transcription, wav_header, write,
};

const ME: &str = "did:zu:ewvi7nxzyoun6zhxrhs64oiz";
const ID: &str = "kx7q2m5v4tbn3hpw";
const NAME: &str = "zutalk-kx7q2m5v4tbn3hpw.wav";
const NOW: i64 = 1_800_000_000_000;
const TAPE_KEY: [u8; 32] = [7; 32];

fn owned(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

fn strings(all: &[&str]) -> Vec<String> {
    all.iter().map(|s| s.to_string()).collect()
}

/// A token as the whole-session service sends it: settled, and with its times.
fn token(text: &str, language: &str, start_ms: u32, end_ms: u32) -> Token {
    Token {
        text: text.into(),
        start_ms,
        end_ms,
        is_final: true,
        speaker: "1".into(),
        language: language.into(),
        into: None,
    }
}

/// Pieces said one after another, each `ms` long, from `at`.
fn run(pieces: &[&str], language: &str, at: u32, ms: u32) -> Vec<Token> {
    let at = |i: usize| at + i as u32 * ms;
    pieces.iter().enumerate().map(|(i, p)| token(p, language, at(i), at(i) + ms)).collect()
}

fn line(language: &str, text: &str) -> Line {
    Line {
        t0: 0,
        t1: 3_000,
        speaker: "1".into(),
        language: language.into(),
        text: text.into(),
        ..Line::default()
    }
}

fn plan(soniox: &str, openrouter: &str, into: &[&str]) -> Plan {
    Plan {
        id: ID.into(),
        spoken: strings(&["en", "zh"]),
        into: strings(into),
        terms: vec![("ZuTalk".into(), owned(&[("zh", "ZuTalk"), ("th", "ซูทอล์ก")]))],
        topic: "Shipping schedule".into(),
        model: "stt-async-v6".into(),
        soniox: format!("{soniox}/v1"),
        openrouter: format!("{openrouter}/api/v1"),
    }
}

// ---- 1. where a line ends ----

fn texts(lines: &[Line]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

#[test]
fn a_silence_over_two_seconds_ends_a_line_and_a_shorter_one_does_not() {
    let mut tokens = run(&[" We", " ship", " in", " March"], "en", 0, 300);
    tokens.extend(run(&[" Then", " we", " invoice"], "en", 1_200 + 2_100, 300));
    tokens.extend(run(&[" and", " wait"], "en", 4_200 + 1_900, 300));
    let spoken = strings(&["en"]);
    assert_eq!(texts(&lines(&tokens, &spoken)), ["We ship in March", "Then we invoice and wait"]);
}

#[test]
fn a_sentence_end_ends_a_line_only_once_it_has_run_four_seconds() {
    let short = run(&[" Yes", ".", " We", " ship", " in", " March", "."], "en", 0, 400);
    let mut tokens = short.clone();
    tokens.extend(run(&[" Next", " week", " we", " invoice", "."], "en", 2_800, 400));
    let spoken = strings(&["en"]);
    // "Yes." ran 0.8 s; "…March." ran 2.8 s; with the next words it runs past 4 s.
    let got = lines(&tokens, &spoken);
    assert_eq!(texts(&got), ["Yes. We ship in March. Next week we invoice."]);

    let slow = run(&[" We", " ship", " five", " thousand", " units", "."], "en", 0, 800);
    let mut tokens = slow;
    tokens.extend(run(&[" Then", " we", " invoice", "."], "en", 4_800, 300));
    assert_eq!(
        texts(&lines(&tokens, &spoken)),
        ["We ship five thousand units.", "Then we invoice."]
    );
}

#[test]
fn chinese_ends_at_its_own_full_stop() {
    let said = ["我", "们", "下", "个", "月", "发", "货", "五", "千", "件", "。"];
    let mut tokens = run(&said, "zh", 0, 400);
    tokens.extend(run(&["好", "的", "。"], "zh", 4_400, 300));
    tokens.extend(run(&["谢", "谢", "。"], "zh", 5_300, 300));
    let spoken = strings(&["zh", "en"]);
    assert_eq!(texts(&lines(&tokens, &spoken)), ["我们下个月发货五千件。", "好的。谢谢。"]);
    // An English full stop in a Chinese line is not the row's mark.
    let latin = run(&["我", "们", "发", "货", ".", "好", "的"], "zh", 0, 800);
    assert_eq!(lines(&latin, &spoken).len(), 1);
}

#[test]
fn a_line_with_no_end_is_cut_before_twenty_seconds_and_never_inside_a_word() {
    // Thai marks no sentence end. The service sends a word in pieces, and a piece inside a
    // word is no place to cut.
    let words = ["ประชุม", "สินค้า", "ราคา", "ลูกค้า", "โรงงาน", "สัปดาห์"];
    let mut pieces = Vec::new();
    for n in 0..30 {
        let word = words[n % words.len()];
        let split = word.char_indices().nth(1).map(|(at, _)| at).unwrap_or(word.len());
        pieces.extend([&word[..split], &word[split..]]);
    }
    let tokens = run(&pieces, "th", 0, 450);
    let got = lines(&tokens, &strings(&["th", "en"]));
    assert!(got.len() >= 2, "{:?}", texts(&got));
    for line in &got {
        assert!(line.t1 - line.t0 <= 20_000, "{line:?}");
        assert!(words.iter().any(|w| line.text.starts_with(w)), "{}", line.text);
        assert!(words.iter().any(|w| line.text.ends_with(w)), "{}", line.text);
    }
    assert_eq!(got.iter().map(|l| l.text.clone()).collect::<String>(), pieces.concat());
}

#[test]
fn a_line_is_cut_before_two_hundred_and_forty_characters() {
    let pieces: Vec<String> = (0..60).map(|n| format!(" word{n:02}")).collect();
    let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
    let got = lines(&run(&pieces, "en", 0, 100), &strings(&["en"]));
    assert_eq!(got.len(), 2);
    assert!(got.iter().all(|l| l.text.chars().count() <= 240));
    assert!(got[1].text.starts_with("word"));
}

#[test]
fn ends_is_a_rule_of_the_open_line_and_the_next_token() {
    let open = Line { t0: 0, t1: 4_500, text: "We ship.\u{201d}".into(), ..line("en", "") };
    assert!(ends(&open, &token(" Then", "en", 4_600, 4_900)), "a mark behind a closer");
    let early = Line { t1: 3_900, ..open.clone() };
    assert!(!ends(&early, &token(" Then", "en", 4_000, 4_300)));
    assert!(ends(&early, &token(" Then", "en", 6_000, 6_300)), "a silence ends it anyway");
    let long = Line { t0: 0, t1: 19_900, text: "and the hotsp".into(), ..line("en", "") };
    assert!(!ends(&long, &token("ot", "en", 19_900, 20_200)), "not inside a word");
    assert!(ends(&long, &token(" then", "en", 19_900, 20_200)));
    // Times that run backwards are not a reason to panic.
    let odd = Line { t0: 9_000, t1: 100, ..open };
    let _ = ends(&odd, &token(" x", "en", 0, 0));
}

// ---- 2. the upload ----

#[test]
fn the_wav_header_says_16_khz_mono_16_bit_and_the_length() {
    let header = wav_header(16_000);
    let word = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let half = |at: usize| u16::from_le_bytes(header[at..at + 2].try_into().unwrap());
    assert_eq!(header.len(), 44);
    assert_eq!(
        (&header[0..4], &header[8..16], &header[36..40]),
        (&b"RIFF"[..], &b"WAVEfmt "[..], &b"data"[..])
    );
    assert_eq!((word(4), word(16), half(20), half(22)), (36 + 32_000, 16, 1, 1));
    assert_eq!((word(24), word(28), half(32), half(34), word(40)), (16_000, 32_000, 2, 16, 32_000));
}

#[test]
fn the_transcription_is_asked_with_the_glossary_and_the_topic() {
    let plan = plan("https://s", "https://o", &["zh", "th"]);
    let body = transcription(&plan, NAME, "file-1");
    assert_eq!(body["model"], "stt-async-v6");
    assert_eq!(body["file_id"], "file-1");
    assert_eq!(body["client_reference_id"], NAME);
    assert_eq!(body["language_hints"], json!(["en", "zh"]));
    assert_eq!(body["enable_language_identification"], true);
    assert_eq!(body["enable_speaker_diarization"], true);
    assert_eq!(body["context"]["terms"], json!(["ZuTalk", "ซูทอล์ก"]));
    assert_eq!(
        body["context"]["general"],
        json!([{ "key": "topic", "value": "Shipping schedule" }])
    );
    let bare = Plan { terms: Vec::new(), topic: String::new(), ..plan };
    assert!(transcription(&bare, NAME, "file-1")["context"].is_null());
}

#[test]
fn a_transcript_becomes_settled_tokens_and_anything_else_is_no_transcript() {
    let transcript = json!({ "tokens": [
        { "text": "你好", "start_ms": 10, "end_ms": 250, "confidence": 0.9, "language": "zh", "speaker": 2 },
        { "text": " hi", "speaker": "3" },
    ]});
    let got = tokens(&transcript).unwrap();
    assert_eq!(got[0], Token { speaker: "2".into(), ..token("你好", "zh", 10, 250) });
    assert_eq!(got[1], Token { speaker: "3".into(), ..token(" hi", "", 0, 0) });
    assert_eq!(tokens(&json!({ "tokens": [] })), Some(Vec::new()));
    assert_eq!(tokens(&json!({ "error_type": "x" })), None);
}

#[test]
fn a_status_is_a_code_from_one_table() {
    assert_eq!(status(200), Ok(()));
    assert_eq!(status(201), Ok(()));
    assert_eq!(status(401), Err(End::Fatal("key-refused")));
    assert_eq!(status(403), Err(End::Fatal("key-refused")));
    assert_eq!(status(402), Err(End::Fatal("out-of-credit")));
    for busy in [408, 429, 500, 503] {
        assert_eq!(status(busy), Err(End::Retry("service-busy")));
    }
    assert_eq!(status(400), Err(End::Fatal("request-refused")));
}

#[test]
fn the_journal_is_whole_on_disk_or_not_there() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(Journal::load(dir.path()), Ok(Journal::default()));
    let journal = Journal {
        name: Some(NAME.into()),
        owed: strings(&["files/f1", "transcriptions/t1"]),
        take: 2,
    };
    journal.save(dir.path()).unwrap();
    assert_eq!(Journal::load(dir.path()), Ok(journal));
    assert!(!dir.path().join("write.json.new").exists());
    std::fs::write(dir.path().join(Journal::FILE), b"{ torn").unwrap();
    assert_eq!(Journal::load(dir.path()), Err(End::Fatal("journal")));
}

// ---- 3. the translator ----

#[test]
fn every_request_says_nothing_may_be_kept_and_holds_the_translator_to_the_schema() {
    let plan = plan("https://s", "https://o", &["zh", "th"]);
    let (before, said) = (line("en", "Good morning."), line("en", "We ship 5,000 units."));
    for model in MODELS {
        let body = request(model, &plan, &[&before], &[(7, &said)]);
        let provider = &body["provider"];
        assert_eq!(provider["zdr"], true);
        assert_eq!(provider["data_collection"], "deny");
        assert_eq!(provider["require_parameters"], true);
        assert_eq!(provider["only"], json!(model.only));
        assert!(!model.only.is_empty());
        assert_eq!(body["model"], model.id);
        assert_eq!(body["reasoning"]["effort"], model.reasoning);
        let format = &body["response_format"];
        assert_eq!(
            (&format["type"], &format["json_schema"]["strict"]),
            (&json!("json_schema"), &json!(true))
        );
        let answer = &format["json_schema"]["schema"]["properties"]["lines"]["items"];
        assert_eq!(answer["additionalProperties"], false);
        assert_eq!(answer["properties"]["to"]["required"], json!(["zh", "th"]));
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Shipping schedule") && system.contains("ซูทอล์ก"));
        assert!(system.contains("never use ผม"), "the Thai row's register");
        let user: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(user["before"][0]["text"], "Good morning.");
        assert_eq!(user["lines"][0]["id"], 7);
        assert_eq!(user["lines"][0]["line"]["text"], "We ship 5,000 units.");
    }
    // Sonnet 5.5 cannot reason less than `low`; the others are asked not to.
    assert_eq!(MODELS[0].reasoning, "low");
}

fn reply(content: &str) -> Value {
    json!({ "choices": [{ "message": { "content": content }, "finish_reason": "stop" }] })
}

const ONE: &str =
    r#"{"lines":[{"id":3,"unclear":false,"facts":["5000"],"to":{"zh":"我们发货5,000件。"}}]}"#;

#[test]
fn a_reply_is_answers_or_a_failure_however_it_is_dressed() {
    let answer = Answer {
        id: 3,
        unclear: false,
        facts: strings(&["5000"]),
        to: owned(&[("zh", "我们发货5,000件。")]),
    };
    assert_eq!(answers(&reply(ONE)), Ok(vec![answer]));
    let failed = End::Retry("translator-failed");
    // HTTP 200, and the failure in the body.
    assert_eq!(answers(&json!({ "error": { "code": 502, "message": "upstream" } })), Err(failed));
    let mut choice = reply(ONE);
    choice["choices"][0]["finish_reason"] = json!("error");
    assert_eq!(answers(&choice), Err(failed));
    choice["choices"][0]["finish_reason"] = json!("stop");
    choice["choices"][0]["error"] = json!({ "code": 500 });
    assert_eq!(answers(&choice), Err(failed));
    // Out of form: not JSON, a field the schema does not have, a field missing.
    let off = End::Retry("off-form");
    assert_eq!(answers(&reply("Sure! Here is")), Err(off));
    assert_eq!(answers(&reply(&ONE.replace("\"unclear\"", "\"maybe\""))), Err(off));
    assert_eq!(answers(&reply(r#"{"lines":[{"id":3}]}"#)), Err(off));
    assert_eq!(answers(&json!({})), Err(off));
}

fn draft<'a>(
    line: &'a Line,
    into: &'a str,
    text: &'a str,
    facts: &'a [String],
    plan: &'a Plan,
) -> Draft<'a> {
    Draft { line, into, text, facts, terms: &plan.terms }
}

fn guard(name: &str) -> zutalk_core::write::Guard {
    GUARDS.iter().find(|(n, _)| *n == name).unwrap().1
}

#[test]
fn numbers_are_read_by_value_however_they_are_written() {
    assert_eq!(numbers("5,000 baht", false), [5_000.0]);
    assert_eq!(numbers("5k", false), [5_000.0]);
    assert_eq!(numbers("五千", true), [5_000.0]);
    assert_eq!(numbers("五千", false), Vec::<f64>::new());
    assert_eq!(numbers("5千万元", true), [50_000_000.0]);
    assert_eq!(numbers("三千四百五十万", true), [34_500_000.0]);
    assert_eq!(numbers("二〇二六年", true), [2_026.0]);
    assert_eq!(numbers("两千", true), [2_000.0]);
    assert_eq!(numbers("5.000 Stück", false), [5_000.0]);
    assert_eq!(numbers("0,5 %", false), [0.5]);
    assert_eq!(numbers("1.234,5", false), [1_234.5]);
    assert_eq!(numbers("5\u{202f}000 €", false), [5_000.0]);
    assert_eq!(numbers("5 millions d'euros", false), [5_000_000.0]);
    assert_eq!(numbers("5 Millionen", false), [5_000_000.0]);
    assert_eq!(numbers("5 ล้านบาท", false), [5_000_000.0]);
    assert_eq!(numbers("5 km, at 0.5%.", false), [5.0, 0.5]);
    // A month is named in other languages, so it is no number to find there.
    assert_eq!(numbers("3月15日", true), [15.0]);
    assert_eq!(numbers("3월", false), Vec::<f64>::new());
    assert_eq!(numbers("一起去", true), [1.0], "read only where a number is looked for");
}

#[test]
fn the_facts_guard_compares_values_not_digit_strings() {
    let plan = plan("", "", &["zh", "th", "en"]);
    let facts = guard("facts");
    let said = line("en", "We ship 5,000 units in March.");
    let five = strings(&["5000"]);
    for good in ["我们三月发货5,000件。", "我们三月发货五千件。", "我们三月发货5千件。"]
    {
        assert!(facts(&draft(&said, "zh", good, &five, &plan)), "{good}");
    }
    assert!(!facts(&draft(&said, "zh", "我们三月发货500件。", &five, &plan)));
    assert!(!facts(&draft(&said, "zh", "我们三月发货。", &[], &plan)), "the line's own digits");
    let spelled = line("en", "We ship five thousand units.");
    assert!(facts(&draft(&spelled, "th", "เราส่งสินค้า 5k ชิ้น", &five, &plan)));
    assert!(!facts(&draft(&spelled, "th", "เราส่งสินค้า 50 ชิ้น", &five, &plan)));
    let chinese = line("zh", "我们发货五千件。");
    assert!(facts(&draft(&chinese, "en", "We ship 5,000 units.", &five, &plan)));
    assert!(!facts(&draft(&chinese, "en", "We ship some units.", &five, &plan)));
}

#[test]
fn the_glossary_guard_wants_a_term_said_written_the_glossarys_way() {
    let plan = plan("", "", &["zh", "th"]);
    let glossary = guard("glossary");
    let said = line("en", "Open zutalk on the laptop.");
    assert!(glossary(&draft(&said, "th", "เปิด ซูทอล์ก บนแล็ปท็อป", &[], &plan)));
    assert!(!glossary(&draft(&said, "th", "เปิดแอปบนแล็ปท็อป", &[], &plan)), "dropped");
    assert!(!glossary(&draft(&said, "th", "เปิด ZuTalk บนแล็ปท็อป", &[], &plan)), "not its way");
    let other = line("en", "Open the laptop.");
    assert!(glossary(&draft(&other, "th", "เปิดแล็ปท็อป", &[], &plan)), "not said, not wanted");
    let thai = line("th", "เปิด ซูทอล์ก ก่อน");
    assert!(glossary(&draft(&thai, "zh", "先打开ZuTalk", &[], &plan)), "said in its written form");
}

#[test]
fn the_script_guard_turns_down_an_answer_in_another_language() {
    let plan = plan("", "", &["zh", "th", "en"]);
    let script = guard("script");
    let said = line("en", "What is the difference between SKU and MOQ?");
    assert!(script(&draft(&said, "zh", "SKU 和 MOQ 有什么区别？", &[], &plan)));
    assert!(!script(&draft(&said, "th", "What is the difference?", &[], &plan)));
    assert!(script(&draft(&said, "th", "SKU กับ MOQ ต่างกันอย่างไร", &[], &plan)));
    assert!(!script(&draft(&said, "zh", "SKUとMOQの違いは何ですか", &[], &plan)), "Japanese");
    assert!(script(&draft(&line("en", "5,000."), "zh", "5,000。", &[], &plan)), "no letters");
}

#[test]
fn the_length_guard_turns_down_nothing_and_wild_lengths() {
    let plan = plan("", "", &["zh", "en"]);
    let length = guard("length");
    let said = line("en", "We ship in March.");
    assert!(length(&draft(&said, "zh", "我们三月发货。", &[], &plan)));
    assert!(!length(&draft(&said, "zh", " ", &[], &plan)));
    let long = "我们三月发货。".repeat(12);
    assert!(!length(&draft(&said, "zh", &long, &[], &plan)));
    let chinese = line("zh", "好的，我们三月发货，四月开票，五月收款。");
    assert!(!length(&draft(&chinese, "en", "OK.", &[], &plan)));
}

#[test]
fn the_forbidden_guard_keeps_a_gendered_thai_particle_out() {
    let plan = plan("", "", &["th"]);
    let forbidden = guard("forbidden");
    let said = line("en", "Thank you.");
    assert!(!forbidden(&draft(&said, "th", "ขอบคุณครับ", &[], &plan)));
    assert!(!forbidden(&draft(&said, "th", "ผมขอบคุณ", &[], &plan)));
    assert!(forbidden(&draft(&said, "th", "ขอบคุณ", &[], &plan)));
    assert!(forbidden(&draft(&said, "zh", "谢谢", &[], &plan)), "only a language's own words");
}

fn unsaid(language: &str, text: &str) -> Said {
    Said { line: line(language, text), via: owned(&[("text", "stt-async-v6")]) }
}

#[test]
fn judging_keeps_what_passes_and_marks_the_rest_unclear_never_a_guess() {
    let plan = plan("", "", &["zh", "th", "en"]);
    let mut said = unsaid("en", "Thank you.");
    let answer = Answer {
        id: 0,
        unclear: false,
        facts: Vec::new(),
        to: owned(&[("zh", " 谢谢。"), ("th", "ขอบคุณครับ"), ("en", "")]),
    };
    assert!(!judge(&mut said, Some(&answer), "model-a", &plan));
    assert_eq!(said.line.translations, owned(&[("zh", "谢谢。"), ("th", "")]));
    assert_eq!(said.via, owned(&[("text", "stt-async-v6"), ("zh", "model-a"), ("th", "unclear")]));
    // Asked again: a passing answer replaces the mark, and a failing one keeps what passed.
    let again =
        Answer { to: owned(&[("zh", "OK"), ("th", "ขอบคุณ")]), ..answer.clone() };
    assert!(judge(&mut said, Some(&again), "model-b", &plan));
    assert_eq!(said.line.translations, owned(&[("zh", "谢谢。"), ("th", "ขอบคุณ")]));
    assert_eq!(said.via, owned(&[("text", "stt-async-v6"), ("zh", "model-a"), ("th", "model-b")]));
    // A line the translator itself could not make out is unclear in every language.
    let mut unclear = unsaid("en", "Shots ten mesh.");
    let doubt =
        Answer {
            unclear: true, to: owned(&[("zh", "十张网"), ("th", "สิบตาข่าย")]), ..answer
        };
    assert!(!judge(&mut unclear, Some(&doubt), "model-a", &plan));
    assert_eq!(unclear.line.translations, owned(&[("zh", ""), ("th", "")]));
    assert!(!judge(&mut unsaid("en", "Hello."), None, "model-a", &plan), "no answer at all");
}

// ---- 4. the services, stood in for ----

struct Keyring {
    soniox: Option<&'static str>,
    openrouter: Option<&'static str>,
    refused: Mutex<Vec<Service>>,
}

impl Keyring {
    fn both() -> Self {
        Keyring {
            soniox: Some("soniox-key"),
            openrouter: Some("openrouter-key"),
            refused: Mutex::default(),
        }
    }
}

impl Keys for Keyring {
    async fn lease(&self, service: Service) -> Result<Lease, End> {
        let key = match service {
            Service::Soniox => self.soniox,
            Service::OpenRouter => self.openrouter,
        };
        let key = key.ok_or(End::Fatal("no-key"))?;
        Ok(Lease { key: Secret::new(key), session_cap: None })
    }

    fn refused(&self, service: Service) {
        self.refused.lock().unwrap().push(service);
    }
}

/// A recording: two seconds of tape, and its live draft as the recording's text.
struct Recording {
    _dir: tempfile::TempDir,
    root: PathBuf,
    tape: PathBuf,
    shelf: Shelf,
}

fn recording(seconds: u32) -> Recording {
    let dir = tempfile::tempdir().unwrap();
    let (root, shelf) = (
        dir.path().join("tapes"),
        Shelf::new(dir.path().join("shelf"), ME, Arc::new(SecretKey::from_seed(&[5; 32]))),
    );
    let tape = root.join(ID);
    let mut writer = Writer::create(&tape, &TAPE_KEY).unwrap();
    for n in 0..seconds * 10 {
        let pcm = (0..BLOCK).map(|i| ((i as i32 * 37 + n as i32) % 2_000 - 1_000) as i16).collect();
        writer.append(&Block { t_ms: n * 100, pcm }).unwrap();
    }
    drop(writer);
    let about = About {
        started: NOW,
        length_ms: 1_900,
        preview: "We ship".into(),
        spoken: strings(&["en", "zh"]),
        ..About::default()
    };
    shelf.put(ID, RECORDING, &[(ID.to_string(), Some(about))], NOW).unwrap();
    Recording { _dir: dir, root, tape, shelf }
}

/// What the service was asked, in order, as `METHOD /path`.
type Log = Arc<Mutex<Vec<String>>>;

fn logged(log: &Log, answer: ResponseTemplate) -> impl Fn(&Request) -> ResponseTemplate + use<> {
    let log = log.clone();
    move |request: &Request| {
        log.lock().unwrap().push(format!("{} {}", request.method, request.url.path()));
        answer.clone()
    }
}

/// What the transcription service says this recording held.
fn transcript() -> Value {
    let mut said = run(&[" We", " ship", " 5,000", " units", " in", " March", "."], "en", 0, 300);
    said.extend(run(&[" Thank", " you", "."], "en", 4_600, 300));
    let wire = |t: &Token| json!({ "text": t.text, "start_ms": t.start_ms, "end_ms": t.end_ms, "language": "en", "speaker": 1 });
    json!({ "id": "tr-1", "tokens": said.iter().map(wire).collect::<Vec<_>>() })
}

/// A transcription service that keeps what it is sent until it is deleted.
async fn soniox(log: &Log) -> MockServer {
    let server = MockServer::start().await;
    let created = |id: &str| ResponseTemplate::new(201).set_body_json(json!({ "id": id }));
    let upload = {
        let log = log.clone();
        move |request: &Request| {
            log.lock().unwrap().push("POST /v1/files".into());
            let body = String::from_utf8_lossy(&request.body);
            let wav = request.body.windows(4).position(|w| w == b"RIFF").unwrap();
            let size = u32::from_le_bytes(request.body[wav + 40..wav + 44].try_into().unwrap());
            assert_eq!(size, 2 * 32_000, "two seconds of 16-bit samples at 16 kHz");
            assert!(body.contains(&format!("filename=\"{NAME}\"")));
            assert_eq!(request.headers["authorization"], "Bearer soniox-key");
            ResponseTemplate::new(201).set_body_json(json!({ "id": "file-1" }))
        }
    };
    Mock::given(method("POST")).and(path("/v1/files")).respond_with(upload).mount(&server).await;
    let job = logged(log, created("tr-1"));
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(job)
        .mount(&server)
        .await;
    let queued = ResponseTemplate::new(200).set_body_json(json!({ "status": "queued" }));
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-1"))
        .respond_with(queued)
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let done = ResponseTemplate::new(200).set_body_json(json!({ "status": "completed" }));
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-1"))
        .respond_with(done)
        .mount(&server)
        .await;
    let text = ResponseTemplate::new(200).set_body_json(transcript());
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions/tr-1/transcript"))
        .respond_with(text)
        .mount(&server)
        .await;
    for kind in ["files", "transcriptions"] {
        let empty =
            ResponseTemplate::new(200).set_body_json(json!({ kind: [], "next_page_cursor": null }));
        Mock::given(method("GET"))
            .and(path(format!("/v1/{kind}")))
            .respond_with(logged(log, empty))
            .mount(&server)
            .await;
    }
    let gone = logged(log, ResponseTemplate::new(204));
    Mock::given(method("DELETE"))
        .and(path_regex("^/v1/(files|transcriptions)/"))
        .respond_with(gone)
        .mount(&server)
        .await;
    server
}

/// What a faithful translator would say of the lines in the transcript.
fn translated(text: &str) -> &'static str {
    match text {
        "We ship 5,000 units in March." => "我们三月发货5,000件。",
        "Thank you." => "谢谢。",
        _ => "",
    }
}

/// A translator: each model row answers as `answer` says, given the lines it was asked about.
async fn openrouter(answer: fn(&str, &[(u64, String)]) -> ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    let respond = move |request: &Request| {
        let body: Value = request.body_json().unwrap();
        let user: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let lines = user["lines"].as_array().unwrap().iter();
        let lines: Vec<(u64, String)> = lines
            .map(|l| (l["id"].as_u64().unwrap(), l["line"]["text"].as_str().unwrap().into()))
            .collect();
        answer(body["model"].as_str().unwrap(), &lines)
    };
    Mock::given(method("POST"))
        .and(path("/api/v1/chat/completions"))
        .respond_with(respond)
        .mount(&server)
        .await;
    server
}

fn faithful(lines: &[(u64, String)], unclear: &str) -> ResponseTemplate {
    let answer = |(id, text): &(u64, String)| {
        let facts: Vec<String> = numbers(text, false).iter().map(|n| n.to_string()).collect();
        json!({ "id": id, "unclear": text == unclear, "facts": facts, "to": { "zh": translated(text) } })
    };
    let lines: Vec<Value> = lines.iter().map(answer).collect();
    ResponseTemplate::new(200).set_body_json(reply(&json!({ "lines": lines }).to_string()))
}

async fn asked(server: &MockServer) -> Vec<Value> {
    let requests = server.received_requests().await.unwrap();
    requests.iter().map(|r| r.body_json().unwrap()).collect()
}

fn read(recording: &Recording) -> zutalk_core::shelf::Recording {
    recording.shelf.read(ID).unwrap().unwrap()
}

#[tokio::test]
async fn a_written_take_replaces_the_draft_only_once_nothing_is_left_at_the_service() {
    let (log, recording) = (Log::default(), recording(2));
    let (stt, mt) = (soniox(&log).await, openrouter(|_, lines| faithful(lines, "")).await);
    let plan = plan(&stt.uri(), &mt.uri(), &["zh"]);
    let steps = log.clone();
    let progress = move |step: &'static str, _, _| steps.lock().unwrap().push(step.into());
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW + 1,
        progress,
    )
    .await;
    assert_eq!(end, End::Done);

    let written = read(&recording);
    assert_eq!(written.about.take, 1);
    assert_eq!(
        (written.about.length_ms, written.about.preview.as_str()),
        (2_000, "We ship 5,000 units in March. Thank you.")
    );
    let said: Vec<&Said> = written.lines.iter().map(|(_, said)| said).collect();
    assert_eq!(said.len(), 2);
    assert_eq!(said[0].line.translations, owned(&[("zh", "我们三月发货5,000件。")]));
    assert_eq!(said[0].via, owned(&[("text", "stt-async-v6"), ("zh", MODELS[0].id)]));
    assert_eq!(written.lines[0].0, format!("{ID}.1.00000"));

    let log = log.lock().unwrap().clone();
    let at = |entry: &str| {
        log.iter().position(|e| e == entry).unwrap_or_else(|| panic!("{entry} in {log:?}"))
    };
    assert!(at("POST /v1/files") < at("POST /v1/transcriptions"));
    assert!(at("DELETE /v1/transcriptions/tr-1") < at("DELETE /v1/files/file-1"));
    assert!(at("DELETE /v1/files/file-1") < at("translating"), "gone before anything else is done");
    assert!(at("translating") < at("signing"));
    assert!(!recording.tape.join(Journal::FILE).exists());
    for body in asked(&mt).await {
        assert_eq!(body["provider"]["zdr"], true);
        assert_eq!(body["provider"]["data_collection"], "deny");
    }
}

#[tokio::test]
async fn a_remote_file_left_by_an_interrupted_write_is_deleted_by_the_next_sweep() {
    let (log, recording) = (Log::default(), recording(2));
    let stt = MockServer::start().await;
    let file = ResponseTemplate::new(201).set_body_json(json!({ "id": "file-1" }));
    Mock::given(method("POST")).and(path("/v1/files")).respond_with(file).mount(&stt).await;
    // The app quits while the service is still making the transcription.
    let slow = ResponseTemplate::new(201)
        .set_body_json(json!({ "id": "tr-9" }))
        .set_delay(Duration::from_secs(30));
    Mock::given(method("POST"))
        .and(path("/v1/transcriptions"))
        .respond_with(slow)
        .mount(&stt)
        .await;
    let (plan, keys) = (plan(&stt.uri(), "http://127.0.0.1:9", &[]), Keyring::both());
    let attempt =
        write(&recording.shelf, (&recording.tape, &TAPE_KEY), &plan, &keys, || NOW, |_, _, _| {});
    assert!(tokio::time::timeout(Duration::from_millis(1_500), attempt).await.is_err());
    let left = Journal::load(&recording.tape).unwrap();
    assert_eq!(
        left,
        Journal { name: Some(NAME.into()), owed: strings(&["files/file-1"]), take: 0 }
    );

    // At the next start, the file is deleted by its id, and the transcription found by name.
    stt.reset().await;
    let listed = |kind: &str, entries: Value| {
        ResponseTemplate::new(200).set_body_json(json!({ kind: entries }))
    };
    let files = listed(
        "files",
        json!([{ "id": "file-1", "filename": NAME }, { "id": "other", "filename": "zutalk-another.wav" }]),
    );
    Mock::given(method("GET"))
        .and(path("/v1/files"))
        .respond_with(logged(&log, files))
        .mount(&stt)
        .await;
    let jobs = listed("transcriptions", json!([{ "id": "tr-9", "client_reference_id": NAME }]));
    Mock::given(method("GET"))
        .and(path("/v1/transcriptions"))
        .respond_with(logged(&log, jobs))
        .mount(&stt)
        .await;
    let gone = logged(&log, ResponseTemplate::new(204));
    Mock::given(method("DELETE")).respond_with(gone).mount(&stt).await;
    assert_eq!(sweep(&recording.root, &plan.soniox, &Keyring::both()).await, End::Done);
    let deleted: Vec<String> =
        log.lock().unwrap().iter().filter(|e| e.starts_with("DELETE")).cloned().collect();
    assert_eq!(
        deleted,
        ["DELETE /v1/transcriptions/tr-9", "DELETE /v1/files/file-1", "DELETE /v1/files/file-1"]
    );
    assert_eq!(Journal::load(&recording.tape).unwrap().name, None);
    assert_eq!(read(&recording).about.take, 0, "nothing was written");

    // Nothing owed: the next sweep asks the service nothing.
    stt.reset().await;
    assert_eq!(sweep(&recording.root, &plan.soniox, &Keyring::both()).await, End::Done);
    assert!(stt.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn nothing_is_written_while_the_audio_is_still_at_the_service() {
    let (log, recording) = (Log::default(), recording(2));
    let stt = soniox(&log).await;
    let refused = ResponseTemplate::new(500);
    Mock::given(method("DELETE")).respond_with(refused).with_priority(1).mount(&stt).await;
    let plan = plan(&stt.uri(), "http://127.0.0.1:9", &[]);
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Retry("not-deleted"));
    assert_eq!(read(&recording).about.take, 0, "the transcript came back, and is not the text");
    let owed = Journal::load(&recording.tape).unwrap().owed;
    assert_eq!(owed, ["files/file-1", "transcriptions/tr-1"]);
}

#[tokio::test]
async fn when_the_first_translator_fails_the_next_row_writes_and_is_named() {
    let (log, recording) = (Log::default(), recording(2));
    let stt = soniox(&log).await;
    let mt = openrouter(|model, lines| match model == MODELS[0].id {
        // The service took the request, and the provider failed after.
        true => ResponseTemplate::new(200)
            .set_body_json(json!({ "error": { "code": 502, "message": "upstream" } })),
        false => faithful(lines, ""),
    })
    .await;
    let plan = plan(&stt.uri(), &mt.uri(), &["zh"]);
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Done);
    let models: Vec<Value> = asked(&mt).await.iter().map(|body| body["model"].clone()).collect();
    assert_eq!(models, [MODELS[0].id, MODELS[1].id]);
    for (_, said) in read(&recording).lines {
        assert_eq!(said.via["zh"], MODELS[1].id);
    }
}

#[tokio::test]
async fn a_line_no_translator_can_make_out_is_asked_again_alone_then_stored_empty_and_unclear() {
    let (log, recording) = (Log::default(), recording(2));
    let stt = soniox(&log).await;
    let mt = openrouter(|_, lines| faithful(lines, "Thank you.")).await;
    let plan = plan(&stt.uri(), &mt.uri(), &["zh"]);
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Done);
    let batches: Vec<usize> = asked(&mt)
        .await
        .iter()
        .map(|body| {
            let user: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            user["lines"].as_array().unwrap().len()
        })
        .collect();
    assert_eq!(batches, [2, 1], "the batch, then the unclear line alone");
    let lines = read(&recording).lines;
    assert_eq!(lines[0].1.line.translations["zh"], "我们三月发货5,000件。");
    assert_eq!(lines[1].1.line.translations["zh"], "");
    assert_eq!(lines[1].1.via["zh"], "unclear");
}

#[tokio::test]
async fn translators_that_answer_out_of_form_leave_lines_unclear_and_ones_that_cannot_be_reached_fail_the_write()
 {
    let (log, recording) = (Log::default(), recording(2));
    let stt = soniox(&log).await;
    let mt = openrouter(|_, _| {
        ResponseTemplate::new(200).set_body_json(reply("I cannot help with that."))
    })
    .await;
    let plan = plan(&stt.uri(), &mt.uri(), &["zh"]);
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Done);
    assert!(read(&recording).lines.iter().all(|(_, said)| said.via["zh"] == "unclear"));
    assert_eq!(asked(&mt).await.len(), 3 * MODELS.len(), "a batch, then each line alone");

    let busy = openrouter(|_, _| ResponseTemplate::new(503)).await;
    let plan = Plan { openrouter: format!("{}/api/v1", busy.uri()), ..plan };
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Retry("service-busy"));
    assert_eq!(read(&recording).about.take, 1, "the take before stays the text");
}

#[tokio::test]
async fn without_a_key_nothing_is_sent_and_nothing_changes() {
    let (log, recording) = (Log::default(), recording(2));
    let stt = soniox(&log).await;
    let plan = plan(&stt.uri(), "http://127.0.0.1:9", &["zh"]);
    let keys = Keyring { openrouter: None, ..Keyring::both() };
    let end =
        write(&recording.shelf, (&recording.tape, &TAPE_KEY), &plan, &keys, || NOW, |_, _, _| {})
            .await;
    assert_eq!(end, End::Fatal("no-key"));
    assert!(stt.received_requests().await.unwrap().is_empty());
    assert_eq!(read(&recording).about.take, 0);
    assert!(!recording.tape.join(Journal::FILE).exists());
}

#[tokio::test]
async fn a_refused_key_is_said_and_handed_back() {
    let recording = recording(2);
    let stt = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(401)).mount(&stt).await;
    let plan = plan(&stt.uri(), "http://127.0.0.1:9", &[]);
    let keys = Keyring::both();
    let end =
        write(&recording.shelf, (&recording.tape, &TAPE_KEY), &plan, &keys, || NOW, |_, _, _| {})
            .await;
    assert_eq!(end, End::Fatal("key-refused"));
    assert_eq!(*keys.refused.lock().unwrap(), [Service::Soniox]);
}

#[tokio::test]
async fn offline_is_said_and_nothing_changes() {
    let recording = recording(2);
    let plan = plan("http://127.0.0.1:9", "http://127.0.0.1:9", &[]);
    let end = write(
        &recording.shelf,
        (&recording.tape, &TAPE_KEY),
        &plan,
        &Keyring::both(),
        || NOW,
        |_, _, _| {},
    )
    .await;
    assert_eq!(end, End::Retry("offline"));
    assert_eq!(read(&recording).about.take, 0);
}

// ---- 5. the real service ----

/// Speech made on this machine, as 16 kHz mono samples.
fn speak(voice: &str, text: &str, dir: &Path) -> Vec<i16> {
    let (aiff, wav) = (dir.join(format!("{voice}.aiff")), dir.join(format!("{voice}.wav")));
    let said =
        std::process::Command::new("say").args(["-v", voice, "-o"]).arg(&aiff).arg(text).status();
    assert!(said.unwrap().success());
    let to = ["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"];
    let made = std::process::Command::new("afconvert").args(to).arg(&aiff).arg(&wav).status();
    assert!(made.unwrap().success());
    let bytes = std::fs::read(&wav).unwrap();
    let mut at = 12;
    while &bytes[at..at + 4] != b"data" {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        at += 8 + size.next_multiple_of(2);
    }
    bytes[at + 8..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
}

#[tokio::test]
#[ignore = "sends synthetic speech to the real Soniox; needs SONIOX_TEST_KEY"]
async fn real_soniox_async_writes_a_take_and_leaves_nothing_behind() {
    let key = std::env::var("SONIOX_TEST_KEY").expect("SONIOX_TEST_KEY");
    let (dir, silence) = (tempfile::tempdir().unwrap(), vec![0i16; 40_000]);
    let mut pcm = speak(
        "Samantha",
        "Good morning. We will ship five thousand units of ZuTalk in March.",
        dir.path(),
    );
    pcm.extend(&silence);
    pcm.extend(speak("Tingting", "好的，我们下个月发货。价格是每件十二块。", dir.path()));
    pcm.extend(&silence);
    pcm.extend(speak("Kanya", "ขอบคุณมาก แล้วพบกันสัปดาห์หน้า", dir.path()));
    pcm.resize(pcm.len().div_ceil(BLOCK) * BLOCK, 0);

    let id = format!("realsoniox{}", std::process::id());
    let (tapes, shelf) = (
        dir.path().join("tapes"),
        Shelf::new(dir.path().join("shelf"), ME, Arc::new(SecretKey::from_seed(&[5; 32]))),
    );
    let mut writer = Writer::create(&tapes.join(&id), &TAPE_KEY).unwrap();
    for (n, block) in pcm.chunks(BLOCK).enumerate() {
        writer.append(&Block { t_ms: n as u32 * 100, pcm: block.to_vec() }).unwrap();
    }
    drop(writer);
    let about = About { started: NOW, spoken: strings(&["en", "zh", "th"]), ..About::default() };
    shelf.put(&id, RECORDING, &[(id.clone(), Some(about))], NOW).unwrap();

    let plan = Plan {
        id: id.clone(),
        spoken: strings(&["en", "zh", "th"]),
        into: Vec::new(),
        model: "stt-async-v6".into(),
        soniox: "https://api.soniox.com/v1".into(),
        ..plan("", "", &[])
    };
    let keys = Keyring {
        soniox: Some(Box::leak(key.clone().into_boxed_str())),
        openrouter: None,
        refused: Mutex::default(),
    };
    let end =
        write(&shelf, (&tapes.join(&id), &TAPE_KEY), &plan, &keys, || NOW + 1, |_, _, _| {}).await;
    assert_eq!(end, End::Done);
    let written = shelf.read(&id).unwrap().unwrap();
    let languages: Vec<&str> =
        written.lines.iter().map(|(_, s)| s.line.language.as_str()).collect();
    println!(
        "seconds={} lines={} languages={languages:?}",
        pcm.len() / 16_000,
        written.lines.len()
    );
    assert_eq!(written.about.take, 1);
    assert!(written.lines.len() >= 3, "three sentences apart");
    assert!(written.lines.iter().all(|(_, s)| s.via["text"] == "stt-async-v6"));
    for language in ["en", "zh", "th"] {
        assert!(languages.contains(&language), "{language}");
    }
    assert!(!tapes.join(&id).join(Journal::FILE).exists());

    // Nothing of it is left at the service.
    let http = reqwest::Client::new();
    let name = format!("zutalk-{id}.wav");
    for kind in ["files", "transcriptions"] {
        let url = format!("https://api.soniox.com/v1/{kind}?limit=1000");
        let listed: Value =
            http.get(url).bearer_auth(&key).send().await.unwrap().json().await.unwrap();
        let left = listed[kind].as_array().unwrap().iter();
        let left = left.filter(|e| {
            e["filename"] == name.as_str() || e["client_reference_id"] == name.as_str()
        });
        assert_eq!(left.count(), 0, "{kind} left behind");
    }
}
