//! The runtime: recovery, and what every layer is built on. It knows nothing of the app.
//! Budget: none of its own; it runs inside the layer that calls it, on that layer's budget.
//! Must not use any other module of this crate.

pub mod supervise;
