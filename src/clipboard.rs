//! Native clipboard backend (N15).
//!
//! Uses `arboard` for cross-platform clipboard access (Wayland + X11).
//! The clipboard is optional: if no display server is available,
//! [`create_clipboard`] returns `None` and callers fall back to no-ops.
//!
//! `arboard::Clipboard`'s methods take `&mut self`, so the backend wraps
//! one instance in a [`Mutex`] to expose the shared (`&self`)
//! [`ClipboardBackend`] interface for copies, which are brief. A read
//! (paste) can wait on the selection owner, so it opens its own handle
//! instead of holding that lock; the window runs it on a worker thread.

use std::sync::{Arc, Mutex};

/// Error from a clipboard operation.
#[derive(Debug)]
pub enum ClipboardError {
    /// Failed to connect to the clipboard backend.
    Connect(String),
    /// Failed to set clipboard contents.
    Set(String),
    /// Failed to get clipboard contents.
    Get(String),
}

impl std::fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClipboardError::Connect(msg) => write!(f, "clipboard connect failed: {msg}"),
            ClipboardError::Set(msg) => write!(f, "clipboard set failed: {msg}"),
            ClipboardError::Get(msg) => write!(f, "clipboard get failed: {msg}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// A native clipboard backend.
///
/// `Send + Sync` so an `Arc` can cross the UI/worker boundary.
/// `set_text` is bounded and non-blocking (the backend serves text lazily).
/// `get_text` may block while the selection owner sends its data (a slow
/// or stuck owner, a large clipboard): call it off the UI thread.
pub trait ClipboardBackend: Send + Sync {
    /// Place `text` on the clipboard, replacing any previous contents.
    fn set_text(&self, text: &str) -> Result<(), ClipboardError>;
    /// Fetch the current clipboard text.
    fn get_text(&self) -> Result<String, ClipboardError>;
}

/// An `arboard`-backed [`ClipboardBackend`].
struct ArboardBackend {
    /// `arboard::Clipboard`'s methods take `&mut self`; the `Mutex` lets the
    /// shared backend serialize access. `arboard` opens the native
    /// clipboard only for each transfer, so the lock is held briefly.
    inner: Mutex<arboard::Clipboard>,
}

impl ClipboardBackend for ArboardBackend {
    fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
        let mut cb = self
            .inner
            .lock()
            .expect("clipboard mutex is never poisoned");
        cb.set_text(text)
            .map_err(|err| ClipboardError::Set(err.to_string()))
    }

    fn get_text(&self) -> Result<String, ClipboardError> {
        // A read can wait on the selection owner: it uses its own handle
        // (arboard shares one native connection per process), so it never
        // holds the lock a copy on the UI thread needs.
        let mut cb =
            arboard::Clipboard::new().map_err(|err| ClipboardError::Connect(err.to_string()))?;
        cb.get_text()
            .map_err(|err| ClipboardError::Get(err.to_string()))
    }
}

/// Create a native clipboard. Returns `None` if no clipboard is available
/// (headless, no display server). Never panics.
pub fn create_clipboard() -> Option<Arc<dyn ClipboardBackend>> {
    match arboard::Clipboard::new() {
        Ok(clipboard) => Some(Arc::new(ArboardBackend {
            inner: Mutex::new(clipboard),
        })),
        Err(err) => {
            eprintln!("nora: native clipboard unavailable: {err}");
            None
        }
    }
}
