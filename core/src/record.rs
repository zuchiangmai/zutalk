//! The facts: the person's signed records, and what is kept or moved in. Written on the
//! record path (L2, seconds), which must never hold the live path back.

pub mod migrate;
pub mod retain;
pub mod shelf;
