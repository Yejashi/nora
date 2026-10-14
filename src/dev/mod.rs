//! Development tooling (N26): deterministic corpora and headless
//! benchmarks that distinguish processing from presentation.
//!
//! These are development instruments: no production code path depends
//! on this module, and diagnostics omit user input/output by default.

pub mod bench;
pub mod corpus;
