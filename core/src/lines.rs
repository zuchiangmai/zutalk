//! Speech-recognition tokens become lines. The same rules run on the live stream and, after
//! the session, on the whole-session transcription. Values in, values out: no IO, no clock.

use crate::lang::{self, Script, profile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// A new speaker label on the same language is another person only after this much
/// silence: the service's labels flip inside a word now and then.
const TURN_PAUSE_MS: u32 = 200;
/// A word that follows a longer silence than this starts something new.
const EMBED_PAUSE_MS: u32 = 700;
/// A sentence in another script takes in this many Latin words in a row, and no more.
const EMBED_MAX: usize = 2;

/// One piece of text from the service. A word is one or more tokens and opens with a space.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub start_ms: u32, // on the capture clock; a translation carries no times
    pub end_ms: u32,
    pub is_final: bool,   // settled; otherwise a guess the next response replaces
    pub speaker: String,  // the service's label for the voice; empty when it gave none
    pub language: String, // the language spoken, as the service labelled it; empty when not
    pub into: Option<String>, // on a translation: the language it is in
}

/// What a response ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    None,
    Provider, // the service's own `<end>` or `<fin>`: the connection carries on past it
    Seam,     // a reconnect or the stop: nothing more will come
}

/// What one voice said in one language, from `t0` to `t1` on the capture clock.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Line {
    pub t0: u32,
    pub t1: u32,
    pub speaker: String,
    pub language: String, // `und` until a letter or a label says which
    pub text: String,
    pub translations: BTreeMap<String, String>,
    pub incomplete: bool, // holds words the service never settled
}

#[derive(Debug, Clone, Default)]
pub struct Assembler {
    spoken: Vec<String>,
    lines: Vec<Line>,    // since the last boundary, spelled as the service sent them
    guesses: Vec<Token>, // of the latest response
    cursor: usize,       // the line translations have reached: they come in line order
    /// The Latin words the last line took in since its own script last spoke: the byte
    /// they start at, when they started, and when the line ended before them.
    run: Option<(usize, u32, u32)>,
}

/// An unknown label fits any.
fn same(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || a == b
}

fn latin(text: impl IntoIterator<Item = char>) -> bool {
    lang::script(text) == Some(Script::Latin)
}

impl Assembler {
    /// `spoken` are the session's languages: evidence for words that come with no usable label.
    pub fn new(spoken: &[&str]) -> Self {
        Self { spoken: spoken.iter().map(|l| l.to_string()).collect(), ..Self::default() }
    }

    /// One response and what it ended. Lines finish at a boundary, all at once: until then
    /// a translation may still be on its way to any of them.
    pub fn push(&mut self, tokens: &[Token], boundary: Boundary) -> Vec<Line> {
        // Guesses are a whole revision, not a delta: the previous ones go.
        self.guesses = tokens.iter().filter(|t| !t.is_final).cloned().collect();
        tokens.iter().filter(|t| t.is_final).for_each(|t| self.take(t));
        if boundary == Boundary::None { Vec::new() } else { self.close(boundary) }
    }

    /// The lines still open, guesses included, as a stop right now would leave them.
    pub fn tail(&self) -> Vec<Line> {
        self.clone().close(Boundary::Seam)
    }

    /// At a seam the guesses are the only record of that speech, and are kept. At the
    /// service's own boundary they open the next sentence and will come again; only a
    /// translation still trailing belongs to what was said before it.
    fn close(&mut self, at: Boundary) -> Vec<Line> {
        let kept = |t: &&Token| at == Boundary::Seam || t.into.is_some();
        std::mem::take(&mut self.guesses).iter().filter(kept).for_each(|t| self.take(t));
        (self.cursor, self.run) = (0, None);
        let lines = std::mem::take(&mut self.lines).into_iter().map(|mut line| {
            if line.language.is_empty() {
                line.language = "und".into();
            }
            // Set after joining: a letter split across two tokens can then compose.
            let said = profile(&line.language);
            line.text = said.normalize(&line.text).trim().to_string();
            // A line that turned into the language of its translation has none in it.
            line.translations.retain(|into, text| {
                *text = profile(into).polish(text, said).trim().to_string();
                *into != line.language && !text.is_empty()
            });
            line
        });
        // The word separator is a token of its own: alone, it is not a line.
        lines.filter(|line| !line.text.is_empty()).collect()
    }

    fn take(&mut self, t: &Token) {
        let Some(into) = &t.into else { return self.speak(t) };
        // A translation carries no times, but translations come in the order of their
        // lines: it goes to the first line it can belong to, from the last one served on.
        let fits = |l: &Line| {
            l.language != *into && same(&l.speaker, &t.speaker) && same(&l.language, &t.language)
        };
        let n = self.lines.len();
        if let Some(i) = (self.cursor..n).chain(0..self.cursor).find(|&i| fits(&self.lines[i])) {
            self.cursor = i;
            *self.lines[i].translations.entry(into.clone()).or_default() += &t.text;
        }
    }

    /// A word joins the last line, or opens the next when the voice or the language changed.
    fn speak(&mut self, t: &Token) {
        // Letters only one spoken language is written in name it, whatever the label. A
        // token with no letters (a number, punctuation) names none and takes its line's.
        let unique = lang::written_in(&t.text, &self.spoken);
        let lettered = t.text.chars().any(char::is_alphabetic);
        let mut language = unique.unwrap_or(if lettered { &t.language } else { "" });
        let mut next = Line { t0: t.start_ms, t1: t.start_ms, ..Line::default() };
        let mut turn = true;
        if let Some(line) = self.lines.last_mut() {
            let pause = t.start_ms.saturating_sub(line.t1);
            let (voice, paused) = (!same(&line.speaker, &t.speaker), pause >= TURN_PAUSE_MS);
            // No space after a Latin letter: the rest of that word ("Hotsp" + "ot"). It is
            // not a word more, and names no language whatever its label.
            let word = latin(t.text.chars().next()) && latin(line.text.chars().next_back());
            let word = word && !(voice && paused);
            language = if word { "" } else { language };
            // Another language in the same voice with no real pause may not be a turn.
            let tongue = !same(&line.language, language);
            let soon = tongue && !voice && pause < EMBED_PAUSE_MS;
            let words = |text: &str| text.split_whitespace().count();
            // A Latin word or two inside a sentence in another script is part of it.
            let embedded = soon
                && latin(t.text.chars())
                && !profile(&line.language).scripts.contains(&Script::Latin)
                && self.run.is_none_or(|(at, ..)| words(&line.text[at..]) < EMBED_MAX);
            // A sentence in another script may open with a Latin word or two. "Of course."
            // and a pause was a sentence; a misheard "Shots." that ran straight on was not.
            let head = line.text.trim_end();
            let opens = soon
                && unique.is_some()
                && latin(head.chars())
                && words(head) <= EMBED_MAX
                && !(paused && head.ends_with(['.', '?', '!']));
            turn = if tongue { !embedded && !opens } else { voice && paused };
            if turn
                && soon
                && latin(t.text.chars())
                && let Some((at, start, end)) = self.run
            {
                // One Latin word more: the language changed, and the words before it began
                // the new sentence ("…4 ทุ่ม If you" / "are joining…").
                (next.text, next.t0, line.t1) = (line.text.split_off(at), start, end);
            } else if embedded {
                self.run.get_or_insert((line.text.len(), t.start_ms, line.t1));
                language = ""; // it takes the line's language, not its own
            } else if !tongue && !word {
                self.run = None;
            }
        }
        if turn {
            self.lines.push(next);
            self.run = None;
        }
        let Some(line) = self.lines.last_mut() else { return };
        line.text += &t.text;
        (line.t0, line.t1) = (line.t0.min(t.start_ms), line.t1.max(t.end_ms));
        line.incomplete |= !t.is_final;
        if !t.speaker.is_empty() {
            line.speaker.clone_from(&t.speaker);
        }
        if !language.is_empty() {
            line.language = language.to_string();
        }
    }
}
