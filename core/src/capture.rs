//! Capture, L0–L2: the device callback (a few ms, hard real time: no allocation, no lock, no
//! disk), the pump (100 ms a block: no disk) and the tape (seconds: never holds the pump back).
//! Audio leaves the device only through `tape::Pass`.

pub mod audio;
pub mod clock;
pub mod tape;
