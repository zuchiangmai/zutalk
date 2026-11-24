//! Live, L1: from the pump's blocks to captions on the screen, speech to screen p99 ≤ 1 s.
//! Must not read or write the disk, nor wait on another layer.

pub mod lane;
pub mod lang;
pub mod lines;
pub mod wire;
