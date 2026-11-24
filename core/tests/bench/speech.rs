//! Deterministic synthetic speech schedule for the simulation bench (`P1.md` §3.10).
//!
//! No random crate is used: a fixed linear congruential generator builds the same schedule on
//! every run. Words carry unique markers (`w000001`, `词000001`) so the probe can track first
//! appearance in O(1) and the privacy test can scan logs for every spoken and translated word.

/// Duration of one audio block in milliseconds (`capture::clock::BLOCK` at 16 kHz).
pub const BLOCK_MS: u32 = 100;
/// Token span in milliseconds (`docs/LESSONS.md`: Soniox stt-rt-v6 tokens are 60 ms wide).
pub const WORD_DURATION_MS: u32 = 60;
/// Offset of every word's `start_ms` inside its 100 ms block so `end_ms = start_ms + 60` stays
/// inside the same block for exact capture-clock round-tripping.
const WORD_BLOCK_OFFSET_MS: u32 = 20;
/// Every N-th sentence is a continuous 24-second utterance with no internal silence.
const LONG_EVERY_SENTENCES: usize = 10;
/// Word spacing during a 24-second continuous sentence (60 words * 400 ms = 24,000 ms).
const LONG_WORD_STEP_MS: u32 = 400;
/// Word count of a 24-second continuous sentence.
const LONG_SENTENCE_WORDS: usize = 60;
/// Word count of a 150-character Chinese line (15 words * 10 Han/digit chars = 150 chars).
const LONG_ZH_WORDS: usize = 15;
/// Initial silence before speech begins so connections can open before the first word.
const LEAD_SILENCE_MS: u32 = 500;

/// Distinct speaker labels that never collide with JSON keys, numbers, or log event codes.
pub const SPEAKERS: [&str; 2] = ["spk_alpha", "spk_beta"];

/// Parameters for generating a deterministic `Speech` schedule.
#[derive(Clone, Debug)]
pub struct SpeechSpec {
    /// Total duration of the speech schedule on the capture clock, in milliseconds.
    pub duration_ms: u32,
    /// Whether to include 150-character Chinese lines (for the 3-language frame-size test).
    pub long_zh_lines: bool,
    /// Fixed seed for the LCG.
    pub seed: u64,
}

impl SpeechSpec {
    pub fn standard(duration_ms: u32) -> Self {
        Self { duration_ms, long_zh_lines: false, seed: 0x5A55_7A1C_0000_0001 }
    }

    pub fn three_languages(duration_ms: u32) -> Self {
        Self { duration_ms, long_zh_lines: true, seed: 0x5A55_7A1C_0000_0003 }
    }
}

/// One spoken word on the capture clock, with its unique spoken and translated tokens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// 1-based unique word id (index + 1).
    pub id: u32,
    /// 0-based sentence index.
    pub sentence: usize,
    /// Language spoken (`"en"` or `"zh"`).
    pub language: &'static str,
    /// Speaker label from [`SPEAKERS`].
    pub speaker: &'static str,
    /// Start time on the capture clock, in ms.
    pub start_ms: u32,
    /// End time on the capture clock, in ms (`start_ms + WORD_DURATION_MS`).
    pub end_ms: u32,
    /// Token text as the realtime service sends it (English opens with a leading space).
    pub token: String,
    /// Substring guaranteed to appear verbatim in normalized captions.
    pub needle: String,
    /// Unique translation tokens by target language (`"en"`, `"zh"`, `"es"`).
    pub trans_en: String,
    pub trans_zh: String,
    pub trans_es: String,
    /// True if this is the final word of its sentence.
    pub sentence_end: bool,
}

impl Word {
    /// Translation token text for target language `into` (or `None` if same as spoken).
    pub fn translation(&self, into: &str) -> Option<&str> {
        if into == self.language {
            return None;
        }
        match into {
            "en" => Some(&self.trans_en),
            "zh" => Some(&self.trans_zh),
            "es" => Some(&self.trans_es),
            _ => Some(&self.trans_en),
        }
    }
}

/// One continuous sentence span on the capture clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sentence {
    pub index: usize,
    pub language: &'static str,
    pub speaker: &'static str,
    pub start_ms: u32,
    pub end_ms: u32,
    pub first_word: usize,
    pub word_count: usize,
}

/// Precomputed deterministic speech schedule with O(1) per-block lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Speech {
    pub duration_ms: u32,
    pub words: Vec<Word>,
    pub sentences: Vec<Sentence>,
    /// True for every 100 ms block inside a spoken sentence (loud); false during pauses (silent).
    block_loud: Vec<bool>,
    /// Index into `words` for the word in this 100 ms block, or `u32::MAX` if none.
    block_word: Vec<u32>,
}

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn range(&mut self, lo: u32, hi: u32) -> u32 {
        lo + ((self.next() >> 32) as u32 % (hi - lo + 1))
    }
}

impl Speech {
    pub fn build(spec: SpeechSpec) -> Self {
        let total_blocks = (spec.duration_ms / BLOCK_MS) as usize + 1;
        let mut block_loud = vec![false; total_blocks];
        let mut block_word = vec![u32::MAX; total_blocks];
        let mut words = Vec::new();
        let mut sentences = Vec::new();

        let mut rng = Lcg::new(spec.seed);
        let mut cursor_ms = LEAD_SILENCE_MS;
        let mut sentence_idx = 0usize;
        let mut next_id = 1u32;

        // Leave room at the end for the final sentence's words and `<end>` to settle.
        let cutoff_ms = spec.duration_ms.saturating_sub(2_500);

        while cursor_ms < cutoff_ms {
            let is_long_24s = sentence_idx % LONG_EVERY_SENTENCES == LONG_EVERY_SENTENCES - 1;
            let is_long_zh = spec.long_zh_lines && !is_long_24s && sentence_idx % 3 == 1;
            let language =
                if is_long_zh || (!is_long_24s && sentence_idx % 3 == 2) { "zh" } else { "en" };
            let speaker = SPEAKERS[sentence_idx % SPEAKERS.len()];

            let (count, step_ms) = if is_long_24s {
                (LONG_SENTENCE_WORDS, LONG_WORD_STEP_MS)
            } else if is_long_zh {
                (LONG_ZH_WORDS, 300)
            } else {
                (rng.range(6, 10) as usize, rng.range(3, 4) * BLOCK_MS)
            };

            let span_ms = (count as u32) * step_ms;
            if cursor_ms + span_ms > cutoff_ms {
                break;
            }

            let sentence_start = cursor_ms;
            let sentence_end = cursor_ms + span_ms;
            let first_word = words.len();

            for pos in 0..count {
                let block_t_ms = cursor_ms + (pos as u32) * step_ms;
                let start_ms = block_t_ms + WORD_BLOCK_OFFSET_MS;
                let end_ms = start_ms + WORD_DURATION_MS;
                let id = next_id;
                next_id += 1;

                let (token, needle) = match (language, is_long_zh) {
                    ("en", _) => (format!(" w{id:06}"), format!("w{id:06}")),
                    ("zh", true) => (format!("词{id:06}甲乙丙"), format!("词{id:06}")),
                    ("zh", false) => (format!("词{id:06}语"), format!("词{id:06}")),
                    _ => unreachable!(),
                };
                let trans_en = format!(" en{id:06}");
                let trans_zh = format!("译{id:06}字");
                let trans_es = format!(" es{id:06}");

                let word_idx = words.len();
                let block_idx = (block_t_ms / BLOCK_MS) as usize;
                if block_idx < block_word.len() {
                    block_word[block_idx] = word_idx as u32;
                }
                words.push(Word {
                    id,
                    sentence: sentence_idx,
                    language,
                    speaker,
                    start_ms,
                    end_ms,
                    token,
                    needle,
                    trans_en,
                    trans_zh,
                    trans_es,
                    sentence_end: pos + 1 == count,
                });
            }

            let start_block = (sentence_start / BLOCK_MS) as usize;
            let end_block = ((sentence_end / BLOCK_MS) as usize).min(block_loud.len());
            for loud in &mut block_loud[start_block..end_block] {
                *loud = true;
            }

            sentences.push(Sentence {
                index: sentence_idx,
                language,
                speaker,
                start_ms: sentence_start,
                end_ms: sentence_end,
                first_word,
                word_count: count,
            });

            sentence_idx += 1;
            let pause_ms = rng.range(14, 20) * BLOCK_MS;
            cursor_ms = sentence_end + pause_ms;
        }

        Self { duration_ms: spec.duration_ms, words, sentences, block_loud, block_word }
    }

    /// Whether the 100 ms block `block_idx` falls inside spoken audio.
    pub fn is_loud(&self, block_idx: usize) -> bool {
        self.block_loud.get(block_idx).copied().unwrap_or(false)
    }

    /// Index into `self.words` for the word whose block is `block_idx`, if any.
    pub fn word_at_block(&self, block_idx: usize) -> Option<usize> {
        let idx = *self.block_word.get(block_idx)?;
        (idx != u32::MAX).then_some(idx as usize)
    }

    /// Every spoken and translated token marker for privacy log scanning.
    pub fn forbidden_words(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.words.len() * 3);
        for word in &self.words {
            out.push(word.needle.clone());
            out.push(word.trans_en.trim().to_owned());
            out.push(word.trans_zh.clone());
            out.push(word.trans_es.trim().to_owned());
        }
        out
    }
}
