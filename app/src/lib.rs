//! The desktop shell library: lock-free event fan-out pump, non-blocking main-thread heartbeat,
//! non-blocking window helpers, and supervised update checking (`P1.md` §3.9, §3.13.7, `P1-23`,
//! `P1-24`).

pub mod beat;
pub mod pump;
pub mod updates;
pub mod windows;
