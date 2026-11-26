//! Knowledge base organized by named topics (`talk/topic`, `topics.jsonl`), with one pinned
//! `"always"` topic that automatically absorbs legacy `talk/term` entries (`P6.md` §3.2).
//!
//! Pure validation, sizing, and text extraction live here without IO; shelf persistence and
//! bounded UTF-8 file reading live in thin helpers called off-hub on `desk`.

use crate::knowledge::glossary::{self, Term};
use crate::live::lane::{Setup as LaneSetup, Stream};
use crate::live::lang::profile;
use crate::record::shelf::{Shelf, TOPIC};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;
use ts_rs::TS;
use zutalk_log::info;

/// Fixed id of the pinned "used in every recording" topic (`P6.md` §3.2).
pub const ALWAYS_TOPIC_ID: &str = "always";
/// Total character budget of one topic (`Knowledge.dc.html` `x / 10,000` and Soniox ceiling).
pub const TOPIC_MAX_CHARS: usize = 10_000;
/// Maximum background narrative characters stored in one topic (`P6.md` §3.2).
pub const TOPIC_MAX_BACKGROUND_CHARS: usize = 6_000;
/// Maximum structured key-value facts stored in one topic (`P6.md` §3.2).
pub const TOPIC_MAX_FACTS: usize = 16;
/// Maximum vocabulary terms stored in one topic (`P6.md` §3.2).
pub const TOPIC_MAX_TERMS: usize = 200;
/// Maximum explicit translation pairs stored in one topic (`P6.md` §3.2).
pub const TOPIC_MAX_TRANSLATION_PAIRS: usize = 100;
/// Maximum file-extracted candidate terms kept as suggestions per topic (`P6.md` §3.2).
pub const TOPIC_MAX_SUGGESTED_TERMS: usize = 40;
/// Hard byte cap (512 KiB) when reading UTF-8 text/Markdown handouts (`P6.md` §3.2).
pub const TOPIC_IMPORT_MAX_BYTES: usize = 512 * 1024;
/// Maximum background excerpt characters prefilled from an imported file (`P6.md` §3.2).
pub const TOPIC_IMPORT_BACKGROUND_CHARS: usize = 1_200;
/// Hard cap on `Setup.topics` so bounded `State` stays O(1) for I8 (`P6.md` §3.2).
pub const MAX_SELECTED_TOPICS: usize = 8;
/// Maximum byte length of a topic id so `settings.json` stays compact and plain (`P6.md` §3.2).
pub const TOPIC_ID_MAX_BYTES: usize = 64;
/// Bounded Soniox `context.general` items across active topics (`P6.md` §3.2).
pub const CONTEXT_MAX_GENERAL_ITEMS: usize = 24;
/// Bounded Soniox `context.text` chars leaving headroom under the 10k limit (`P6.md` §3.2).
pub const CONTEXT_MAX_TEXT_CHARS: usize = 8_000;
/// Bounded Soniox `context.terms` strings (`P6.md` §3.2).
pub const CONTEXT_MAX_TERMS: usize = 300;
/// Bounded Soniox `context.translation_terms` pairs (`P6.md` §3.2).
pub const CONTEXT_MAX_TRANSLATION_TERMS: usize = 300;
/// Bounded background narrative chars injected into OpenRouter system prompts (`P6.md` §3.2).
pub const CONTEXT_PROMPT_BACKGROUND_CHARS: usize = 2_400;

const MAX_CANDIDATE_CHARS: usize = 40; // short term or phrase suitable as a glossary candidate
const MIN_CANDIDATE_CHARS: usize = 2; // ignore single-character noise when extracting suggestions

/// One key-value context fact on a topic (maps to Soniox `context.general`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct TopicFact {
    pub key: String,
    pub value: String,
}

/// One explicit source -> target translation pair on a topic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct TranslationPair {
    pub source: String,
    pub target: String,
}

/// One named knowledge-base topic stored as a signed `talk/topic` record (`topics.jsonl`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct Topic {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub facts: Vec<TopicFact>,
    pub background: String,
    pub terms: Vec<Term>,
    pub translation_terms: Vec<TranslationPair>,
    pub source_file: Option<String>,
    pub suggested_terms: Vec<String>,
    pub pinned: bool,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// Lightweight list row returned by `Query::Topics` (`P6.md` §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(default)]
pub struct TopicSummary {
    pub id: String,
    pub name: String,
    pub domain: String,
    pub pinned: bool,
    pub terms_count: u32,
    pub size_chars: u32,
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// Pure extraction result from a UTF-8 text/Markdown/CSV/TSV handout (`P6.md` §3.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedTopic {
    pub source_file: String,
    pub background: String,
    pub suggested_terms: Vec<String>,
}

/// Strips any POSIX or Windows directory prefix so only the file basename is ever kept.
fn basename_only(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= TOPIC_ID_MAX_BYTES
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn fresh_topic_id() -> Result<String, &'static str> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "random")?;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut id, "{byte:02x}"); // dropped: formatting into String is infallible
    }
    Ok(id)
}

fn clean_written(written: BTreeMap<String, String>) -> BTreeMap<String, String> {
    written
        .into_iter()
        .filter_map(|(lang, form)| {
            let lang = lang.trim().to_owned();
            let form = form.trim().to_owned();
            (!lang.is_empty() && !form.is_empty()).then_some((lang, form))
        })
        .collect()
}

/// Total character count of a topic against [`TOPIC_MAX_CHARS`].
pub fn size_chars(topic: &Topic) -> usize {
    let facts: usize =
        topic.facts.iter().map(|f| f.key.chars().count() + f.value.chars().count()).sum();
    let terms: usize = topic
        .terms
        .iter()
        .map(|t| {
            t.said.chars().count() + t.written.values().map(|v| v.chars().count()).sum::<usize>()
        })
        .sum();
    let pairs: usize = topic
        .translation_terms
        .iter()
        .map(|p| p.source.chars().count() + p.target.chars().count())
        .sum();
    topic.name.chars().count()
        + topic.domain.chars().count()
        + facts
        + topic.background.chars().count()
        + terms
        + pairs
}

/// Pure normalizer and validator enforcing `pinned == (id == "always")`, basename-only
/// `source_file`, deduplication, and the 10,000-char topic ceiling (`P6.md` §3.2).
pub fn normalize(topic: Topic, now: i64) -> Result<Topic, &'static str> {
    let trimmed_id = topic.id.trim();
    let id = if trimmed_id.is_empty() {
        fresh_topic_id()?
    } else {
        if !valid_id(trimmed_id) {
            return Err("topic.missing");
        }
        trimmed_id.to_owned()
    };

    let pinned = id == ALWAYS_TOPIC_ID;
    let name = topic.name.trim().to_owned();
    if !pinned && name.is_empty() {
        return Err("topic.empty");
    }
    let domain = topic.domain.trim().to_owned();
    let background = topic.background.trim().to_owned();

    let mut seen_facts = BTreeSet::new();
    let mut facts = Vec::new();
    for fact in topic.facts {
        let key = fact.key.trim().to_owned();
        let value = fact.value.trim().to_owned();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        let dedup = (profile("").key(&key), profile("").key(&value));
        if seen_facts.insert(dedup) {
            facts.push(TopicFact { key, value });
            if facts.len() >= TOPIC_MAX_FACTS {
                break;
            }
        }
    }

    let mut term_indices: BTreeMap<String, usize> = BTreeMap::new();
    let mut terms: Vec<Term> = Vec::new();
    for term in topic.terms {
        let said = term.said.trim().to_owned();
        if said.is_empty() {
            continue;
        }
        let written = clean_written(term.written);
        let key = profile("").key(&said);
        if let Some(&idx) = term_indices.get(&key) {
            terms[idx].written.extend(written);
        } else if terms.len() < TOPIC_MAX_TERMS {
            term_indices.insert(key, terms.len());
            terms.push(Term { said, written });
        }
    }

    let mut seen_pairs = BTreeSet::new();
    let mut translation_terms = Vec::new();
    for pair in topic.translation_terms {
        let source = pair.source.trim().to_owned();
        let target = pair.target.trim().to_owned();
        if source.is_empty() || target.is_empty() || source == target {
            continue;
        }
        let dedup = (profile("").key(&source), profile("").key(&target));
        if seen_pairs.insert(dedup) {
            translation_terms.push(TranslationPair { source, target });
            if translation_terms.len() >= TOPIC_MAX_TRANSLATION_PAIRS {
                break;
            }
        }
    }

    let mut existing_term_keys: BTreeSet<String> =
        terms.iter().map(|t| profile("").key(&t.said)).collect();
    for term in &terms {
        for form in term.written.values() {
            existing_term_keys.insert(profile("").key(form));
        }
    }
    let mut seen_suggested = BTreeSet::new();
    let mut suggested_terms = Vec::new();
    for cand in topic.suggested_terms {
        let cand = cand.trim().to_owned();
        if cand.is_empty() {
            continue;
        }
        let key = profile("").key(&cand);
        if existing_term_keys.contains(&key) {
            continue;
        }
        if seen_suggested.insert(key) {
            suggested_terms.push(cand);
            if suggested_terms.len() >= TOPIC_MAX_SUGGESTED_TERMS {
                break;
            }
        }
    }

    let source_file = topic.source_file.as_deref().and_then(basename_only);

    let normalized = Topic {
        id,
        name,
        domain,
        facts,
        background,
        terms,
        translation_terms,
        source_file,
        suggested_terms,
        pinned,
        updated_at: now,
    };

    if normalized.background.chars().count() > TOPIC_MAX_BACKGROUND_CHARS
        || size_chars(&normalized) > TOPIC_MAX_CHARS
    {
        return Err("topic.limit");
    }

    Ok(normalized)
}

fn strip_markdown_inline(line: &str) -> String {
    // Strip markdown links `[label](url)` -> `label`.
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']')
        {
            let close_idx = i + 1 + close_bracket;
            if close_idx + 1 < chars.len()
                && chars[close_idx + 1] == '('
                && let Some(close_paren) = chars[close_idx + 2..].iter().position(|&c| c == ')')
            {
                for &c in &chars[i + 1..close_idx] {
                    out.push(c);
                }
                i = close_idx + 2 + close_paren + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out.replace("**", "").replace("__", "").replace(['`', '*'], "")
}

fn strip_list_or_heading(line: &str) -> (&str, bool) {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix('#') {
        return (rest.trim_start_matches('#').trim(), true);
    }
    for prefix in ["- ", "* ", "+ ", "• "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return (rest.trim(), false);
        }
    }
    (trimmed, false)
}

fn push_candidate(seen: &mut BTreeSet<String>, out: &mut Vec<String>, raw: &str) {
    if out.len() >= TOPIC_MAX_SUGGESTED_TERMS {
        return;
    }
    let cleaned = strip_markdown_inline(raw);
    let trimmed = cleaned.trim().trim_matches(|c: char| {
        matches!(
            c,
            '"' | '\''
                | '“'
                | '”'
                | '‘'
                | '’'
                | '('
                | ')'
                | '['
                | ']'
                | ':'
                | '：'
                | ','
                | '，'
                | '.'
                | '。'
                | ';'
        )
    });
    let len = trimmed.chars().count();
    if !(MIN_CANDIDATE_CHARS..=MAX_CANDIDATE_CHARS).contains(&len) || trimmed.contains('\n') {
        return;
    }
    let key = profile("").key(trimmed);
    if key.is_empty() || !seen.insert(key) {
        return;
    }
    out.push(trimmed.to_owned());
}

fn extract_bracketed(
    text: &str,
    open: char,
    close: char,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) {
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len_utf8()..];
        let Some(end) = after.find(close) else {
            break;
        };
        push_candidate(seen, out, &after[..end]);
        rest = &after[end + close.len_utf8()..];
    }
}

/// Pure, zero-IO extractor turning UTF-8 text/Markdown/CSV/TSV into a bounded background excerpt
/// and deduplicated candidate terms (never auto-added to `Topic.terms`).
pub fn extract_from_text(source_file: &str, content: &str) -> ExtractedTopic {
    let mut seen = BTreeSet::new();
    let mut suggested_terms = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();

    // 1. Bracketed terms (`《…》`, `「…」`, `『…』`).
    extract_bracketed(content, '《', '》', &mut seen, &mut suggested_terms);
    extract_bracketed(content, '「', '」', &mut seen, &mut suggested_terms);
    extract_bracketed(content, '『', '』', &mut seen, &mut suggested_terms);

    // 2. Line-level headings, glossary/KV definitions (`A: B`, `A · B`, `A -> B`), and CSV/TSV rows.
    let mut proper_counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
    for raw_line in content.lines() {
        let (body, is_heading) = strip_list_or_heading(raw_line);
        if body.is_empty() {
            continue;
        }
        let cleaned = strip_markdown_inline(body);
        let cleaned_trim = cleaned.trim();
        if !cleaned_trim.is_empty() {
            plain_lines.push(cleaned_trim.to_owned());
        }
        if is_heading {
            push_candidate(&mut seen, &mut suggested_terms, cleaned_trim);
        }
        for sep in [" -> ", " => ", " → ", " · ", "：", ": ", "\t"] {
            if let Some((left, right)) = cleaned_trim.split_once(sep)
                && !left.trim().is_empty()
                && !right.trim().is_empty()
            {
                let lhs = left.trim();
                if lhs.chars().count() <= MAX_CANDIDATE_CHARS {
                    push_candidate(&mut seen, &mut suggested_terms, lhs);
                }
                break;
            }
        }
        if !cleaned_trim.contains(": ")
            && !cleaned_trim.contains('：')
            && let Some((left, right)) = cleaned_trim.split_once(',')
            && !left.trim().is_empty()
            && !right.trim().is_empty()
            && left.trim().chars().count() <= MAX_CANDIDATE_CHARS
        {
            push_candidate(&mut seen, &mut suggested_terms, left.trim());
        }

        // Count repeated English proper nouns / CamelCase / acronyms.
        for token in
            cleaned_trim.split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        {
            let len = token.len();
            if !(MIN_CANDIDATE_CHARS..=MAX_CANDIDATE_CHARS).contains(&len) {
                continue;
            }
            let mut chars = token.chars();
            let first_upper = chars.next().is_some_and(|c| c.is_ascii_uppercase());
            let has_inner_upper = chars.any(|c| c.is_ascii_uppercase());
            if first_upper {
                let key = token.to_ascii_lowercase();
                let entry = proper_counts.entry(key).or_insert_with(|| (token.to_owned(), 0));
                entry.1 += 1;
                if has_inner_upper {
                    // CamelCase / acronyms (e.g. OpenRouter, ZuTalk, LLM) count immediately.
                    entry.1 = entry.1.max(2);
                }
            }
        }
    }

    for (_, (word, count)) in proper_counts {
        if count >= 2 {
            push_candidate(&mut seen, &mut suggested_terms, &word);
        }
    }

    let joined = plain_lines.join("\n");
    let background: String = joined.chars().take(TOPIC_IMPORT_BACKGROUND_CHARS).collect();
    let source_file = basename_only(source_file).unwrap_or_default();

    ExtractedTopic { source_file, background: background.trim().to_owned(), suggested_terms }
}

/// Reads a bounded UTF-8 text/Markdown/CSV/TSV file (`<= 512 KiB`) and extracts candidate terms
/// and a background excerpt (`P6.md` §3.2).
pub fn extract_from_file(path: &Path) -> Result<ExtractedTopic, &'static str> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if !matches!(ext.as_str(), "txt" | "md" | "markdown" | "csv" | "tsv") {
        return Err("topic.file.format");
    }
    let meta = fs::metadata(path).map_err(|_| "topic.file.read")?;
    if meta.len() > TOPIC_IMPORT_MAX_BYTES as u64 {
        return Err("topic.file.big");
    }
    let file = fs::File::open(path).map_err(|_| "topic.file.read")?;
    let mut buf = Vec::new();
    file.take(TOPIC_IMPORT_MAX_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "topic.file.read")?;
    if buf.len() > TOPIC_IMPORT_MAX_BYTES {
        return Err("topic.file.big");
    }
    let content = String::from_utf8(buf).map_err(|_| "topic.file.format")?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    Ok(extract_from_text(name, &content))
}

fn default_always(terms: Vec<Term>) -> Topic {
    Topic { id: ALWAYS_TOPIC_ID.into(), pinned: true, terms, ..Topic::default() }
}

/// Losslessly migrates legacy `talk/term` records into the pinned `"always"` topic if `"always"`
/// has not yet been written to `topics.jsonl`. Returns `Ok(true)` if a migration occurred.
pub fn ensure_migrated(shelf: &Shelf, now: i64) -> Result<bool, &'static str> {
    let kept = shelf.kept::<Topic>(TOPIC).map_err(|_| "shelf")?;
    if kept.iter().any(|(id, _)| id == ALWAYS_TOPIC_ID) {
        return Ok(false);
    }
    let legacy = glossary::read(shelf)?;
    if legacy.is_empty() {
        return Ok(false);
    }
    let saved = save(shelf, default_always(legacy), now)?;
    info!("topic.saved", op = "migrate", pinned = true, terms = saved.terms.len(), code = "ok");
    Ok(true)
}

/// Lists every topic on `shelf`, always placing the pinned `"always"` topic first (synthesized
/// from legacy `talk/term` without writing to disk if not yet persisted), followed by custom
/// topics newest-updated first.
pub fn list(shelf: &Shelf) -> Result<Vec<Topic>, &'static str> {
    let kept = shelf.kept::<Topic>(TOPIC).map_err(|_| "shelf")?;
    let mut always: Option<Topic> = None;
    let mut others: Vec<Topic> = Vec::new();
    for (key, mut topic) in kept {
        if topic.id.is_empty() {
            topic.id = key;
        }
        topic.pinned = topic.id == ALWAYS_TOPIC_ID;
        if topic.pinned {
            always = Some(topic);
        } else {
            others.push(topic);
        }
    }
    let always = match always {
        Some(t) => t,
        None => default_always(glossary::read(shelf)?),
    };
    others.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then_with(|| a.id.cmp(&b.id)));
    let mut out = Vec::with_capacity(others.len() + 1);
    out.push(always);
    out.extend(others);
    Ok(out)
}

/// Lightweight summaries for `Query::Topics`.
pub fn summaries(shelf: &Shelf) -> Result<Vec<TopicSummary>, &'static str> {
    Ok(list(shelf)?
        .into_iter()
        .map(|t| TopicSummary {
            size_chars: u32::try_from(size_chars(&t)).unwrap_or(u32::MAX),
            terms_count: u32::try_from(t.terms.len()).unwrap_or(u32::MAX),
            id: t.id,
            name: t.name,
            domain: t.domain,
            pinned: t.pinned,
            updated_at: t.updated_at,
        })
        .collect())
}

/// Reads one topic by id (`"always"` is always present, synthesized if not yet on disk).
pub fn get(shelf: &Shelf, id: &str) -> Result<Option<Topic>, &'static str> {
    if id == ALWAYS_TOPIC_ID {
        return Ok(list(shelf)?.into_iter().next());
    }
    let kept = shelf.kept::<Topic>(TOPIC).map_err(|_| "shelf")?;
    Ok(kept.into_iter().find_map(|(key, mut topic)| {
        if topic.id.is_empty() {
            topic.id = key;
        }
        topic.pinned = false;
        (topic.id == id).then_some(topic)
    }))
}

/// Validates, normalizes, and persists one `talk/topic` record onto `shelf`.
pub fn save(shelf: &Shelf, topic: Topic, now: i64) -> Result<Topic, &'static str> {
    let saved = normalize(topic, now)?;
    shelf.put("", TOPIC, &[(saved.id.clone(), Some(saved.clone()))], now).map_err(|_| "shelf")?;
    Ok(saved)
}

/// Removes a custom topic by writing a `None` tombstone. Rejects `"always"` with `"topic.pinned"`.
pub fn remove(shelf: &Shelf, id: &str, now: i64) -> Result<(), &'static str> {
    if id == ALWAYS_TOPIC_ID {
        return Err("topic.pinned");
    }
    let kept = shelf.kept::<Topic>(TOPIC).map_err(|_| "shelf")?;
    if !kept.iter().any(|(key, _)| key == id) {
        return Err("topic.missing");
    }
    shelf.put::<Topic>("", TOPIC, &[(id.to_owned(), None)], now).map_err(|_| "shelf")
}

/// Imports a bounded UTF-8 file into `id` (defaults to `"always"` if empty): stores basename only,
/// prefills `background` only when empty, and writes candidate terms ONLY to `suggested_terms`.
pub fn import_file(shelf: &Shelf, id: &str, path: &Path, now: i64) -> Result<Topic, &'static str> {
    let target = if id.trim().is_empty() { ALWAYS_TOPIC_ID } else { id.trim() };
    let mut topic = get(shelf, target)?.ok_or("topic.missing")?;
    let extracted = extract_from_file(path)?;
    topic.source_file = Some(extracted.source_file);
    if topic.background.trim().is_empty() && !extracted.background.is_empty() {
        let headroom = TOPIC_MAX_CHARS.saturating_sub(size_chars(&topic));
        let budget = headroom.min(TOPIC_IMPORT_BACKGROUND_CHARS).min(TOPIC_MAX_BACKGROUND_CHARS);
        topic.background = extracted.background.chars().take(budget).collect();
    }
    let mut merged = extracted.suggested_terms;
    merged.extend(topic.suggested_terms);
    topic.suggested_terms = merged;
    save(shelf, topic, now)
}

/// Merges one term into `topic_id` (defaults to `"always"` when empty), removing it from
/// `suggested_terms` if present, and persists the updated topic.
pub fn remember_term(
    shelf: &Shelf,
    topic_id: &str,
    said: &str,
    written: BTreeMap<String, String>,
    now: i64,
) -> Result<Topic, &'static str> {
    let said = said.trim();
    if said.is_empty() {
        return Err("term.empty");
    }
    let target = if topic_id.trim().is_empty() { ALWAYS_TOPIC_ID } else { topic_id.trim() };
    let mut topic = get(shelf, target)?.ok_or("topic.missing")?;
    let cleaned = clean_written(written);
    let key = profile("").key(said);
    if let Some(existing) = topic.terms.iter_mut().find(|t| profile("").key(&t.said) == key) {
        existing.said = said.to_owned();
        existing.written.extend(cleaned);
    } else {
        topic.terms.push(Term { said: said.to_owned(), written: cleaned });
    }
    topic.suggested_terms.retain(|cand| profile("").key(cand) != key);
    save(shelf, topic, now)
}

/// Keeps the pinned `"always"` topic in sync with legacy `Command::Term` updates/removals.
pub fn put_always_term(
    shelf: &Shelf,
    said: &str,
    written: Option<BTreeMap<String, String>>,
    now: i64,
) -> Result<(), &'static str> {
    let said = said.trim();
    if said.is_empty() {
        return Err("term.empty");
    }
    let mut always = get(shelf, ALWAYS_TOPIC_ID)?.unwrap_or_else(|| default_always(Vec::new()));
    let key = profile("").key(said);
    match written {
        Some(written) => {
            let cleaned = clean_written(written);
            if let Some(existing) =
                always.terms.iter_mut().find(|t| profile("").key(&t.said) == key)
            {
                existing.said = said.to_owned();
                existing.written = cleaned;
            } else {
                always.terms.push(Term { said: said.to_owned(), written: cleaned });
            }
            always.suggested_terms.retain(|cand| profile("").key(cand) != key);
        }
        None => {
            always.terms.retain(|t| profile("").key(&t.said) != key);
        }
    }
    save(shelf, always, now)?;
    Ok(())
}

/// Assembled context from the pinned `"always"` topic plus this recording's selected topics
/// (`DECISIONS.md` #7). Redacts every content field in `Debug` so terms and background text can
/// never reach logs (`CLAUDE.md` Principle 6).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct TopicContext {
    pub topic_count: usize,
    pub general: Vec<(String, String)>,
    pub text: String,
    pub terms: Vec<Term>,
    pub translation_pairs: Vec<TranslationPair>,
}

impl std::fmt::Debug for TopicContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TopicContext")
            .field("topic_count", &self.topic_count)
            .field("general_len", &self.general.len())
            .field("text_chars", &self.text.chars().count())
            .field("terms_len", &self.terms.len())
            .field("pairs_len", &self.translation_pairs.len())
            .finish()
    }
}

/// Pure sanitizer for `Setup.topics`: trims, excludes `"always"` (always included automatically),
/// drops invalid/oversized ids, deduplicates in order, and caps at [`MAX_SELECTED_TOPICS`].
pub fn normalize_selected_topics(ids: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for raw in ids {
        let id = raw.trim();
        if id.is_empty() || id == ALWAYS_TOPIC_ID || !valid_id(id) {
            continue;
        }
        if seen.insert(id.to_owned()) {
            out.push(id.to_owned());
            if out.len() >= MAX_SELECTED_TOPICS {
                break;
            }
        }
    }
    out
}

/// Pure context assembler (`DECISIONS.md` #7): merges ONLY pinned topics and topics explicitly
/// listed in `selected_ids` (ignoring deleted/missing ids), enforcing all character and item caps.
pub fn assemble_context(
    all_topics: &[Topic],
    selected_ids: &[String],
    recording_title: &str,
) -> TopicContext {
    let mut active: Vec<&Topic> = Vec::new();
    let mut included_ids: BTreeSet<&str> = BTreeSet::new();

    for topic in all_topics {
        if topic.pinned && included_ids.insert(topic.id.as_str()) {
            active.push(topic);
        }
    }
    for sel in selected_ids {
        let sel = sel.trim();
        if sel.is_empty() || sel == ALWAYS_TOPIC_ID {
            continue;
        }
        if let Some(topic) = all_topics.iter().find(|t| !t.pinned && t.id == sel)
            && included_ids.insert(topic.id.as_str())
        {
            active.push(topic);
        }
    }

    let mut seen_general = BTreeSet::new();
    let mut general = Vec::new();
    let mut push_general = |k: &str, v: &str| {
        if general.len() >= CONTEXT_MAX_GENERAL_ITEMS {
            return;
        }
        let k = k.trim();
        let v = v.trim();
        if k.is_empty() || v.is_empty() {
            return;
        }
        let dedup = (profile("").key(k), profile("").key(v));
        if seen_general.insert(dedup) {
            general.push((k.to_owned(), v.to_owned()));
        }
    };
    push_general("topic", recording_title);
    for topic in &active {
        push_general("domain", &topic.domain);
        for fact in &topic.facts {
            push_general(&fact.key, &fact.value);
        }
    }

    let backgrounds: Vec<&str> =
        active.iter().map(|t| t.background.trim()).filter(|bg| !bg.is_empty()).collect();
    let text: String = backgrounds.join("\n\n").chars().take(CONTEXT_MAX_TEXT_CHARS).collect();

    let mut term_indices: BTreeMap<String, usize> = BTreeMap::new();
    let mut terms: Vec<Term> = Vec::new();
    for topic in &active {
        for term in &topic.terms {
            let said = term.said.trim();
            if said.is_empty() {
                continue;
            }
            let key = profile("").key(said);
            if let Some(&idx) = term_indices.get(&key) {
                for (lang, form) in &term.written {
                    let form = form.trim();
                    if !lang.trim().is_empty() && !form.is_empty() {
                        terms[idx].written.insert(lang.trim().to_owned(), form.to_owned());
                    }
                }
            } else if terms.len() < CONTEXT_MAX_TERMS {
                term_indices.insert(key, terms.len());
                terms.push(Term {
                    said: said.to_owned(),
                    written: clean_written(term.written.clone()),
                });
            }
        }
    }

    let mut seen_pairs = BTreeSet::new();
    let mut translation_pairs = Vec::new();
    for topic in &active {
        for pair in &topic.translation_terms {
            if translation_pairs.len() >= CONTEXT_MAX_TRANSLATION_TERMS {
                break;
            }
            let source = pair.source.trim();
            let target = pair.target.trim();
            if source.is_empty() || target.is_empty() || source == target {
                continue;
            }
            let dedup = (profile("").key(source), profile("").key(target));
            if seen_pairs.insert(dedup) {
                translation_pairs
                    .push(TranslationPair { source: source.to_owned(), target: target.to_owned() });
            }
        }
    }

    TopicContext { topic_count: active.len(), general, text, terms, translation_pairs }
}

/// Projects an assembled [`TopicContext`] onto one live Soniox stream setup (`P6.md` §3.3).
pub fn apply_live(ctx: &TopicContext, stream: &Stream, setup: LaneSetup) -> LaneSetup {
    let into = match stream {
        Stream::Transcribe => vec![],
        Stream::TwoWay(a, b) => vec![a, b],
        Stream::OneWay(into) => vec![into],
    };
    let mut heard: BTreeSet<String> = BTreeSet::new();
    let mut pairs: BTreeSet<(String, String)> = BTreeSet::new();

    for term in &ctx.terms {
        let forms: BTreeSet<&String> = term.written.values().chain([&term.said]).collect();
        for form in &forms {
            if !form.is_empty() {
                heard.insert((*form).clone());
            }
        }
        for target in into.iter().map(|into| term.written.get(*into).unwrap_or(&term.said)) {
            for form in forms.iter().filter(|form| **form != target) {
                pairs.insert(((*form).clone(), target.clone()));
            }
        }
    }
    for pair in &ctx.translation_pairs {
        if !pair.source.is_empty() {
            heard.insert(pair.source.clone());
        }
        if !pair.target.is_empty() {
            heard.insert(pair.target.clone());
        }
        if !into.is_empty()
            && !pair.source.is_empty()
            && !pair.target.is_empty()
            && pair.source != pair.target
        {
            pairs.insert((pair.source.clone(), pair.target.clone()));
        }
    }

    let general: Vec<(String, String)> =
        ctx.general.iter().take(CONTEXT_MAX_GENERAL_ITEMS).cloned().collect();
    let text: String = ctx.text.chars().take(CONTEXT_MAX_TEXT_CHARS).collect();
    let terms: Vec<String> = heard.into_iter().take(CONTEXT_MAX_TERMS).collect();
    let translation_terms: Vec<(String, String)> =
        pairs.into_iter().take(CONTEXT_MAX_TRANSLATION_TERMS).collect();

    LaneSetup { general, text, terms, translation_terms, ..setup }
}
