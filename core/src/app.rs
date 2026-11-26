//! The hub, and the protocol to the shell. Budget: ≤ 1 ms a command. Must not read or write
//! the disk nor make a network request: that is handed to the layers that do IO (P1-19 and
//! P1-20 move out what the hub still does itself).

pub mod core;
pub mod hub;
pub mod proto;
pub mod recording;
pub mod work;
