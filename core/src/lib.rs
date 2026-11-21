//! ZuTalk core: everything the app knows and does. No UI, no Tauri.
//!
//! The directories are layers, and a layer uses only those below it
//! (`scripts/build/rules/layers.sh` holds the table).

pub mod ai;
pub mod app;
pub mod capture;
pub mod id;
pub mod knowledge;
pub mod live;
pub mod out;
pub mod record;
pub mod rt;
