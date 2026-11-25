//! The desktop shell library: lock-free event fan-out pump, non-blocking window helpers, and
//! supervised update checking (`P1.md` §3.9, `P1-23`).

pub mod pump;
pub mod updates;
pub mod windows;
