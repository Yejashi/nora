//! N15 native clipboard regression suite for `nora::clipboard`.
//!
//! Headless-safe: no PTY, no threads, no GPU, no window. Verifies the
//! optional-clipboard contract (`create_clipboard` returns an `Option` and
//! never panics) and the copy/paste flow through a mock
//! [`ClipboardBackend`].

use std::sync::{Arc, Mutex};

use nora::clipboard::{ClipboardBackend, ClipboardError, create_clipboard};
use winit::raw_window_handle::{RawDisplayHandle, XlibDisplayHandle};

/// N15 (1): `create_clipboard` returns an `Option` — `Some` when a display
/// server is available, `None` when headless — and never panics. In the
/// current (headless CI) environment it is `None`; on a desktop it is
/// `Some`. Either way, calling it must not panic.
#[test]
fn create_clipboard_returns_option_without_panicking() {
    // Assert only that it returned (an `Option`, no panic). The
    // environment decides `Some` vs `None`; both are valid.
    let x11 = RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0));
    // SAFETY: an X11 handle carries no lifetime requirement (arboard opens
    // its own connection); only a Wayland handle does.
    let _ = unsafe { create_clipboard(x11) };
}

/// A mock [`ClipboardBackend`] that records `set_text` calls and returns the
/// last set string from `get_text`.
#[derive(Default)]
struct MockClipboard {
    set_calls: Mutex<Vec<String>>,
    contents: Mutex<String>,
}

impl MockClipboard {
    fn set_calls(&self) -> Vec<String> {
        self.set_calls.lock().unwrap().clone()
    }
}

impl ClipboardBackend for MockClipboard {
    fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
        self.set_calls.lock().unwrap().push(text.to_owned());
        *self.contents.lock().unwrap() = text.to_owned();
        Ok(())
    }

    fn get_text(&self) -> Result<String, ClipboardError> {
        Ok(self.contents.lock().unwrap().clone())
    }
}

/// N15 (2): the copy/paste flow through a [`ClipboardBackend`]: a
/// `set_text` (copy) followed by a `get_text` (paste) round-trips the text,
/// and the copy is recorded exactly once.
#[test]
fn copy_paste_flow_round_trips() {
    let mock = Arc::new(MockClipboard::default());
    let backend: Arc<dyn ClipboardBackend> = mock.clone();

    // Copy.
    backend.set_text("hello clipboard").expect("copy succeeds");
    // Paste.
    let pasted = backend.get_text().expect("paste succeeds");

    assert_eq!(pasted, "hello clipboard", "paste returns the copied text");
    assert_eq!(
        mock.set_calls(),
        vec!["hello clipboard".to_owned()],
        "the copy is recorded exactly once"
    );
}

/// N15 (3): a second `set_text` replaces the previous contents (the
/// clipboard holds only the latest copy), and both copies are recorded in
/// order.
#[test]
fn second_copy_replaces_contents() {
    let mock = Arc::new(MockClipboard::default());
    let backend: Arc<dyn ClipboardBackend> = mock.clone();

    backend.set_text("first").expect("first copy succeeds");
    backend.set_text("second").expect("second copy succeeds");

    assert_eq!(
        backend.get_text().expect("paste succeeds"),
        "second",
        "the clipboard holds only the latest copy"
    );
    assert_eq!(
        mock.set_calls(),
        vec!["first".to_owned(), "second".to_owned()],
        "copies are recorded in order"
    );
}
