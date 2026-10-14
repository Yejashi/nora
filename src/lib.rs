//! nora terminal core (library).
//!
//! N03: headless terminal engine adapter ([`engine::Engine`]) around the
//! pinned `alacritty_terminal` 0.26.0, with owned, engine-free state
//! extraction and ordered event capture, validated by the replay fixtures in
//! `tests/engine_adapter.rs`.

pub mod clipboard;
pub mod config;
pub mod dev;
pub mod engine;
pub mod graphics_filter;
pub mod ime;
pub mod input;
pub mod layout;
pub mod render;
pub mod search;
pub mod session;
pub mod workspace;
