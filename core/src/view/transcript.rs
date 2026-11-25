//! Fixed-size transcript chunking and bounded pure-function take comparison (`P2.md` §3.5).
//!
//! Every transcript or comparison reply is capped at [`TRANSCRIPT_CHUNK_LINES`] (64) lines so
//! per-request cost stays independent of recording length (I8).

use crate::live::lang::profile;
use crate::live::lines::Line;
use crate::record::shelf::Said;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Lines in one transcript or compare chunk (`PLAN.md` §2.5, `P2.md` §3.5).
pub const TRANSCRIPT_CHUNK_LINES: usize = 64;
/// Byte ceiling for character-level LCS inside [`diff_text`]; longer lines degrade to prefix/suffix
/// stripping plus a single delete/insert pair so runtime stays strictly O(1) (`P2.md` §3.5).
pub const DIFF_LCS_MAX_BYTES: usize = 1_024;

/// One bounded 64-line slice of a recording take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct TranscriptChunk {
    pub id: String,
    pub take: u32,
    pub chunk: u32,
    pub total_lines: u32,
    pub total_chunks: u32,
    pub lines: Vec<(String, Said)>,
}

/// One segment of a character-level text diff between two takes of a line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "kind")]
pub enum DiffOp {
    Equal { text: String },
    Delete { text: String },
    Insert { text: String },
}

/// One aligned row comparing `take_a` (typically live draft) against `take_b` (refined).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompareRow {
    pub key_a: Option<String>,
    pub key_b: Option<String>,
    pub t0: u32,
    pub t1: u32,
    pub speaker: String,
    pub language: String,
    pub changed: bool,
    pub text_a: String,
    pub text_b: String,
    pub ops: Vec<DiffOp>,
}

/// One bounded chunk of aligned comparison rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CompareChunk {
    pub id: String,
    pub take_a: u32,
    pub take_b: u32,
    pub chunk: u32,
    pub total_chunks: u32,
    pub rows: Vec<CompareRow>,
}

/// Total number of chunks required for `total_lines` at [`TRANSCRIPT_CHUNK_LINES`] per chunk.
pub fn total_chunks(total_lines: u32) -> u32 {
    total_lines.div_ceil(TRANSCRIPT_CHUNK_LINES as u32)
}

/// Slices chunk `chunk` (0-indexed) out of an in-memory take slice. Out-of-range chunks return
/// empty `lines` while preserving accurate `total_lines` and `total_chunks`.
pub fn slice_chunk(id: &str, take: u32, all: &[(String, Said)], chunk: u32) -> TranscriptChunk {
    let total_lines = all.len() as u32;
    let chunks = total_chunks(total_lines);
    let start = (chunk as usize).saturating_mul(TRANSCRIPT_CHUNK_LINES);
    let lines = if start < all.len() {
        let end = start.saturating_add(TRANSCRIPT_CHUNK_LINES).min(all.len());
        all[start..end].to_vec()
    } else {
        Vec::new()
    };
    TranscriptChunk { id: id.to_owned(), take, chunk, total_lines, total_chunks: chunks, lines }
}

/// Computes a bounded character-level diff between `a` and `b`.
///
/// Guarantees round-trip invariants for all UTF-8 inputs:
/// - concatenating `Equal` + `Delete` yields `a`;
/// - concatenating `Equal` + `Insert` yields `b`.
pub fn diff_text(a: &str, b: &str) -> Vec<DiffOp> {
    if a == b {
        return if a.is_empty() { Vec::new() } else { vec![DiffOp::Equal { text: a.to_owned() }] };
    }

    let ac: Vec<char> = a.chars().collect();
    let bc: Vec<char> = b.chars().collect();
    let prefix = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    let max_suffix = (ac.len() - prefix).min(bc.len() - prefix);
    let suffix =
        (0..max_suffix).take_while(|&k| ac[ac.len() - 1 - k] == bc[bc.len() - 1 - k]).count();

    let a_mid = &ac[prefix..ac.len() - suffix];
    let b_mid = &bc[prefix..bc.len() - suffix];
    let mid_bytes: usize = a_mid.iter().map(|c| c.len_utf8()).sum::<usize>()
        + b_mid.iter().map(|c| c.len_utf8()).sum::<usize>();

    let mut ops = Vec::new();
    if prefix > 0 {
        push_op(&mut ops, DiffOp::Equal { text: ac[..prefix].iter().collect() });
    }

    if a_mid.is_empty() {
        if !b_mid.is_empty() {
            push_op(&mut ops, DiffOp::Insert { text: b_mid.iter().collect() });
        }
    } else if b_mid.is_empty() {
        push_op(&mut ops, DiffOp::Delete { text: a_mid.iter().collect() });
    } else if mid_bytes <= DIFF_LCS_MAX_BYTES {
        append_lcs_diff(&mut ops, a_mid, b_mid);
    } else {
        push_op(&mut ops, DiffOp::Delete { text: a_mid.iter().collect() });
        push_op(&mut ops, DiffOp::Insert { text: b_mid.iter().collect() });
    }

    if suffix > 0 {
        push_op(&mut ops, DiffOp::Equal { text: ac[ac.len() - suffix..].iter().collect() });
    }
    ops
}

fn push_op(ops: &mut Vec<DiffOp>, next: DiffOp) {
    match (ops.last_mut(), next) {
        (Some(DiffOp::Equal { text: prev }), DiffOp::Equal { text }) => prev.push_str(&text),
        (Some(DiffOp::Delete { text: prev }), DiffOp::Delete { text }) => prev.push_str(&text),
        (Some(DiffOp::Insert { text: prev }), DiffOp::Insert { text }) => prev.push_str(&text),
        (_, op) => ops.push(op),
    }
}

fn append_lcs_diff(ops: &mut Vec<DiffOp>, a: &[char], b: &[char]) {
    let m = a.len();
    let n = b.len();
    let cols = n + 1;
    let mut dp = vec![0u32; (m + 1) * cols];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * cols + j] = if a[i] == b[j] {
                dp[(i + 1) * cols + (j + 1)] + 1
            } else {
                dp[(i + 1) * cols + j].max(dp[i * cols + (j + 1)])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if a[i] == b[j] {
            push_char(ops, 0, a[i]);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)] {
            push_char(ops, 1, a[i]);
            i += 1;
        } else {
            push_char(ops, 2, b[j]);
            j += 1;
        }
    }
    while i < m {
        push_char(ops, 1, a[i]);
        i += 1;
    }
    while j < n {
        push_char(ops, 2, b[j]);
        j += 1;
    }
}

fn push_char(ops: &mut Vec<DiffOp>, kind: u8, c: char) {
    match (ops.last_mut(), kind) {
        (Some(DiffOp::Equal { text }), 0) => text.push(c),
        (Some(DiffOp::Delete { text }), 1) => text.push(c),
        (Some(DiffOp::Insert { text }), 2) => text.push(c),
        (_, 0) => ops.push(DiffOp::Equal { text: c.to_string() }),
        (_, 1) => ops.push(DiffOp::Delete { text: c.to_string() }),
        _ => ops.push(DiffOp::Insert { text: c.to_string() }),
    }
}

fn has_timestamps(lines: &[(String, Said)]) -> bool {
    lines.iter().any(|(_, s)| s.line.t0 > 0 || s.line.t1 > 0)
}

fn overlaps(a: &Line, b: &Line) -> bool {
    (a.t0 < b.t1 && b.t0 < a.t1) || (a.t0 == b.t0 && (a.t0 == a.t1 || b.t0 == b.t1))
}

fn join_texts(pieces: &[&str], language: &str) -> String {
    let sep = if profile(language).spaced { " " } else { "" };
    let mut out = String::new();
    for piece in pieces {
        if piece.is_empty() {
            continue;
        }
        if !out.is_empty()
            && !sep.is_empty()
            && !out.ends_with(char::is_whitespace)
            && !piece.starts_with(char::is_whitespace)
        {
            out.push_str(sep);
        }
        out.push_str(piece);
    }
    out
}

/// Aligns `a` (typically live draft) to `b` (typically refined take) by overlapping time intervals,
/// falling back to positional alignment when all timestamps are zero (`P2.md` §3.5).
pub fn compare_lines(a: &[(String, Said)], b: &[(String, Said)]) -> Vec<CompareRow> {
    if !has_timestamps(a) && !has_timestamps(b) {
        let len = a.len().max(b.len());
        let mut rows = Vec::with_capacity(len);
        for i in 0..len {
            match (a.get(i), b.get(i)) {
                (Some((ka, sa)), Some((kb, sb))) => {
                    let text_a = sa.line.text.clone();
                    let text_b = sb.line.text.clone();
                    let changed = text_a != text_b;
                    let ops = diff_text(&text_a, &text_b);
                    rows.push(CompareRow {
                        key_a: Some(ka.clone()),
                        key_b: Some(kb.clone()),
                        t0: sb.line.t0,
                        t1: sb.line.t1,
                        speaker: sb.line.speaker.clone(),
                        language: sb.line.language.clone(),
                        changed,
                        text_a,
                        text_b,
                        ops,
                    });
                }
                (Some((ka, sa)), None) => {
                    let text_a = sa.line.text.clone();
                    let ops = diff_text(&text_a, "");
                    rows.push(CompareRow {
                        key_a: Some(ka.clone()),
                        key_b: None,
                        t0: sa.line.t0,
                        t1: sa.line.t1,
                        speaker: sa.line.speaker.clone(),
                        language: sa.line.language.clone(),
                        changed: !text_a.is_empty(),
                        text_a,
                        text_b: String::new(),
                        ops,
                    });
                }
                (None, Some((kb, sb))) => {
                    let text_b = sb.line.text.clone();
                    let ops = diff_text("", &text_b);
                    rows.push(CompareRow {
                        key_a: None,
                        key_b: Some(kb.clone()),
                        t0: sb.line.t0,
                        t1: sb.line.t1,
                        speaker: sb.line.speaker.clone(),
                        language: sb.line.language.clone(),
                        changed: !text_b.is_empty(),
                        text_a: String::new(),
                        text_b,
                        ops,
                    });
                }
                (None, None) => {}
            }
        }
        return rows;
    }

    let mut matched_a = vec![false; a.len()];
    // Tag each row with `(t0, kind_rank, orig_idx)` so unmatched `a` rows slot cleanly by `t0`.
    let mut tagged: Vec<(u32, u8, usize, CompareRow)> = Vec::with_capacity(b.len() + a.len());

    for (bi, (kb, sb)) in b.iter().enumerate() {
        let mut key_a: Option<String> = None;
        let mut pieces: Vec<&str> = Vec::new();
        for (ai, (ka, sa)) in a.iter().enumerate() {
            if overlaps(&sa.line, &sb.line) {
                matched_a[ai] = true;
                if key_a.is_none() {
                    key_a = Some(ka.clone());
                }
                pieces.push(sa.line.text.as_str());
            }
        }
        let text_a = join_texts(&pieces, &sb.line.language);
        let text_b = sb.line.text.clone();
        let changed = text_a != text_b;
        let ops = diff_text(&text_a, &text_b);
        tagged.push((
            sb.line.t0,
            1,
            bi,
            CompareRow {
                key_a,
                key_b: Some(kb.clone()),
                t0: sb.line.t0,
                t1: sb.line.t1,
                speaker: sb.line.speaker.clone(),
                language: sb.line.language.clone(),
                changed,
                text_a,
                text_b,
                ops,
            },
        ));
    }

    for (ai, (ka, sa)) in a.iter().enumerate() {
        if matched_a[ai] {
            continue;
        }
        let text_a = sa.line.text.clone();
        let ops = diff_text(&text_a, "");
        tagged.push((
            sa.line.t0,
            0,
            ai,
            CompareRow {
                key_a: Some(ka.clone()),
                key_b: None,
                t0: sa.line.t0,
                t1: sa.line.t1,
                speaker: sa.line.speaker.clone(),
                language: sa.line.language.clone(),
                changed: !text_a.is_empty(),
                text_a,
                text_b: String::new(),
                ops,
            },
        ));
    }

    tagged.sort_by_key(|(t0, rank, idx, _)| (*t0, *rank, *idx));
    tagged.into_iter().map(|(_, _, _, row)| row).collect()
}
