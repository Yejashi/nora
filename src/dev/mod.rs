//! Development tooling (N26): deterministic corpora, headless
//! benchmarks and real-PTY / renderer / input instruments that
//! distinguish processing from presentation.
//!
//! These are development instruments: no production code path depends
//! on this module, and diagnostics omit user input/output by default.

pub mod bench;
pub mod corpus;
pub mod input_bench;
pub mod pty_bench;
pub mod render_bench;
