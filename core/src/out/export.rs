//! A recording written out for other software. An exporter is a row of `EXPORTERS`:
//! adding a format is adding a row.

use crate::live::lang::profile;
use crate::live::lines::Line;
use std::collections::BTreeMap;
use std::fmt::Write;

pub struct Doc<'a> {
    pub title: &'a str,
    pub lines: &'a [Line],
    /// The names the person gave to the service's speaker labels.
    pub speakers: &'a BTreeMap<String, String>,
}

pub struct Exporter {
    pub id: &'static str,
    pub ext: &'static str,
    /// The second argument is the languages to write, in order. None: each line as spoken.
    pub render: fn(&Doc, &[String]) -> String,
}

pub static EXPORTERS: &[Exporter] = &[
    Exporter { id: "markdown", ext: "md", render: markdown },
    Exporter { id: "srt", ext: "srt", render: |doc, langs| cues(doc, langs, ',', "") },
    Exporter { id: "vtt", ext: "vtt", render: |doc, langs| cues(doc, langs, '.', "WEBVTT\n\n") },
];

/// A line in `lang`: as it was spoken if that is its language, else its translation.
fn text_in<'a>(line: &'a Line, lang: &str) -> Option<&'a str> {
    if line.language == lang { Some(&line.text) } else { line.translations.get(lang).map(|t| &**t) }
}

fn clock(ms: u32, sep: char) -> String {
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1_000 % 60);
    format!("{h:02}:{m:02}:{s:02}{sep}{:03}", ms % 1_000)
}

/// Each line under who said it and when, with the translations asked for quoted below it.
fn markdown(doc: &Doc, langs: &[String]) -> String {
    let mut out = format!("# {}\n", doc.title);
    for line in doc.lines {
        let who = doc.speakers.get(&line.speaker).unwrap_or(&line.speaker);
        let who = if who.is_empty() { String::new() } else { format!("**{who}** ") };
        let _ = write!(out, "\n{who}{}\n{}\n", &clock(line.t0, '.')[..8], line.text);
        let others = langs.iter().filter(|l| **l != line.language);
        others.filter_map(|l| text_in(line, l)).for_each(|t| _ = writeln!(out, "> {t}"));
    }
    out
}

/// The byte a UTF-16 offset falls on.
fn byte_at(s: &str, offset: u16) -> usize {
    let mut units = 0;
    for (byte, c) in s.char_indices() {
        if units >= offset as usize {
            return byte;
        }
        units += c.len_utf16();
    }
    s.len()
}

/// Subtitles in one language: the first asked for, or each line as it was spoken. A long
/// line becomes several cues, cut where that language's display units end, and they
/// share the line's time by length.
fn cues(doc: &Doc, langs: &[String], sep: char, head: &str) -> String {
    let (mut out, mut n) = (head.to_string(), 0);
    for line in doc.lines {
        let lang = langs.first().unwrap_or(&line.language);
        let Some(text) = text_in(line, lang) else { continue };
        let rules = profile(lang);
        let ends = rules.units(text);
        let whole = ends.last().copied().unwrap_or(1) as u32;
        let when = |at: u16| line.t0 + line.t1.saturating_sub(line.t0) * at as u32 / whole;
        let mut from = 0;
        for cue in ends.chunks(rules.cue_max.max(1) as usize) {
            let to = cue[cue.len() - 1];
            let said = text[byte_at(text, from)..byte_at(text, to)].trim();
            n += 1;
            let (start, end) = (clock(when(from), sep), clock(when(to), sep));
            let _ = write!(out, "{n}\n{start} --> {end}\n{said}\n\n");
            from = to;
        }
    }
    out
}
