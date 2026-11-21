//! The glossary: names and words the person wants heard and spelled right, in the captions and
//! in the record. Each term is one of the person's records, kept apart from the recordings: it
//! is what they know, so it goes where they go, and nobody else signed in here reads it.

use crate::live::lane::{Setup, Stream};
use crate::live::lang::profile;
use crate::record::shelf::{Shelf, TERM};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use ts_rs::TS;

/// A term: how it is said, and how each language writes it where that is not the same.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct Term {
    pub said: String,
    pub written: BTreeMap<String, String>,
}

/// A term's key: however its case or width is typed, one term is one path.
fn key(said: &str) -> String {
    let digest = Sha256::digest(profile("").key(said));
    digest[..16].iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Puts a term in the glossary in place of what it said before, or (none) takes it out.
pub fn put(
    shelf: &Shelf,
    said: &str,
    written: Option<BTreeMap<String, String>>,
    now: i64,
) -> Result<(), &'static str> {
    let said = said.trim();
    if said.is_empty() {
        return Err("term.empty");
    }
    let term = written.map(|written| {
        let written = written.into_iter().map(|(lang, form)| (lang, form.trim().to_string()));
        Term { said: said.into(), written: written.filter(|(_, form)| !form.is_empty()).collect() }
    });
    shelf.put("", TERM, &[(key(said), term)], now).map_err(|_| "shelf")
}

/// The glossary, in the order its terms are said.
pub fn read(shelf: &Shelf) -> Result<Vec<Term>, &'static str> {
    let terms = shelf.kept(TERM).map_err(|_| "shelf")?.into_iter().map(|(_, term)| term);
    let mut terms: Vec<Term> = terms.collect();
    terms.sort_by(|a, b| a.said.cmp(&b.said));
    Ok(terms)
}

/// A stream's setup with the glossary in it: every form of every term to listen for, and for
/// each language the stream translates into, its form there for each of the others. A language
/// that has no form of its own writes a term as it is said.
pub fn live(terms: &[Term], stream: &Stream, setup: Setup) -> Setup {
    let into = match stream {
        Stream::Transcribe => vec![],
        Stream::TwoWay(a, b) => vec![a, b],
        Stream::OneWay(into) => vec![into],
    };
    let (mut heard, mut pairs) = (BTreeSet::new(), BTreeSet::new());
    for term in terms {
        let forms: BTreeSet<&String> = term.written.values().chain([&term.said]).collect();
        heard.extend(forms.iter().map(|form| form.to_string()));
        for target in into.iter().map(|into| term.written.get(*into).unwrap_or(&term.said)) {
            let other = forms.iter().filter(|form| **form != target);
            pairs.extend(other.map(|form| (form.to_string(), target.clone())));
        }
    }
    let (terms, translation_terms) = (heard.into_iter().collect(), pairs.into_iter().collect());
    Setup { terms, translation_terms, ..setup }
}
