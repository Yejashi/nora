//! Session layer: bounded, ordered outbound I/O per terminal session.
//!
//! N04a introduces [`io::BoundedWriter`], a byte-budgeted pending-write
//! queue that caps queued application and parser-reply bytes and returns
//! capacity as writes complete. The headless `Session` (engine + writer)
//! arrives in N04b.

pub mod io;
