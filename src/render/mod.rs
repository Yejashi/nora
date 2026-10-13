//! Owned presentation state shared between the engine and render layers.
//!
//! Everything in this module holds owned data only: no engine lifetimes,
//! borrowed renderable iterators or terminal locks survive into these types.

pub mod policy;
pub mod snapshot;
pub mod state;
pub mod term;
