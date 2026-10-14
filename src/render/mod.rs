//! Owned presentation state shared between the engine and render layers.
//!
//! Everything in this module holds owned data only: no engine lifetimes,
//! borrowed renderable iterators or terminal locks survive into these types.

pub mod fallback;
pub mod glyph;
pub mod policy;
pub mod snapshot;
pub mod state;
pub mod style;
pub mod term;
