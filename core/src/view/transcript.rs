//! Fixed-size transcript chunking (`P2.md` §3.5).
//!
//! Every transcript reply is capped at [`TRANSCRIPT_CHUNK_LINES`] (64) lines so per-request cost
//! stays independent of recording length (I8).

use crate::record::shelf::Said;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Lines in one transcript chunk (`PLAN.md` §2.5, `P2.md` §3.5).
pub const TRANSCRIPT_CHUNK_LINES: usize = 64;

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
