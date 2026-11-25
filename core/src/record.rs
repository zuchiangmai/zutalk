//! The facts: the person's signed records, and what is kept or moved in. Written on the
//! record path (L2, seconds), which must never hold the live path back.

pub mod adopt;
pub mod migrate;
pub mod notes;
pub mod retain;
pub mod shelf;
pub mod takes;
