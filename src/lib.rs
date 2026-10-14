//! nora terminal core (library).
//!
//! N03: headless terminal engine adapter ([`engine::Engine`]) around the
//! pinned `alacritty_terminal` 0.26.0, with owned, engine-free state
//! extraction and ordered event capture, validated by the replay fixtures in
//! `tests/engine_adapter.rs`.

pub mod clipboard;
pub mod config;
pub mod engine;
pub mod input;
pub mod render;
pub mod session;
