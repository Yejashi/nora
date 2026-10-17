//! Native clipboard backend (N15, ADR-016).
//!
//! The window's display decides the backend ([`clipboard_kind`]): a native
//! Wayland window uses `smithay-clipboard` on the window's own
//! `wl_display`, anything else uses `arboard` (X11 on Linux). A Wayland
//! session never starts the X11 path behind the user's back: a build
//! without the `wayland` feature cannot produce a Wayland window, and a
//! Wayland display without the Wayland backend gets no clipboard.
//!
//! The clipboard is optional: if none is available, [`create_clipboard`]
//! returns `None` and callers fall back to no-ops.
//!
//! Copies run on the UI thread and never block. A read (paste) can wait on
//! the selection owner, so the window runs it on a worker thread.

use std::sync::{Arc, Mutex, TryLockError};

use winit::raw_window_handle::RawDisplayHandle;

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

/// Which native clipboard serves a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardKind {
    /// `smithay-clipboard` on the window's `wl_display` (native Wayland).
    Wayland,
    /// `arboard`: X11 on Linux (Xlib/Xcb displays).
    Arboard,
}

/// The backend for a window's display: Wayland for a Wayland display,
/// `arboard` for every other kind (X11 on Linux).
pub fn clipboard_kind(display: &RawDisplayHandle) -> ClipboardKind {
    match display {
        RawDisplayHandle::Wayland(_) => ClipboardKind::Wayland,
        _ => ClipboardKind::Arboard,
    }
}

/// Create the native clipboard for a window's `display`. Returns `None` if
/// no clipboard is available (headless, no display server, or a Wayland
/// display in a build without the Wayland backend). Never panics.
///
/// # Safety
///
/// When `display` is a Wayland handle, its `wl_display` must stay
/// connected for as long as the returned backend is alive, including any
/// clone of the `Arc` a worker thread still holds: the backend's threads
/// use the display until the last clone is dropped. Other handle kinds
/// carry no such requirement (`arboard` opens its own connection).
pub unsafe fn create_clipboard(display: RawDisplayHandle) -> Option<Arc<dyn ClipboardBackend>> {
    match (clipboard_kind(&display), display) {
        #[cfg(all(feature = "wayland", unix, not(target_os = "macos")))]
        (ClipboardKind::Wayland, RawDisplayHandle::Wayland(handle)) => {
            // SAFETY: the caller keeps this `wl_display` connected for the
            // backend's whole lifetime (this function's contract).
            Some(Arc::new(unsafe {
                wayland::WaylandBackend::new(handle.display.as_ptr())
            }))
        }
        (ClipboardKind::Wayland, _) => {
            eprintln!("nora: native clipboard unavailable: no Wayland clipboard in this build");
            None
        }
        (ClipboardKind::Arboard, _) => create_arboard(),
    }
}

/// An `arboard`-backed [`ClipboardBackend`].
///
/// `arboard::Clipboard`'s methods take `&mut self`, so the backend wraps
/// one instance in a [`Mutex`] for copies, which are brief. A read (paste)
/// can wait on the selection owner, so it opens its own handle instead of
/// holding that lock.
struct ArboardBackend {
    /// `arboard` opens the native clipboard only for each transfer, so the
    /// lock is held briefly.
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

fn create_arboard() -> Option<Arc<dyn ClipboardBackend>> {
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

/// One reader whose reads may block indefinitely, used by one read at a
/// time: a read that finds an earlier one still waiting fails at once
/// instead of queueing behind it, so a stuck selection owner costs one
/// blocked thread, not one per paste. Reads are serialized on the reader,
/// so each read receives its own reply.
#[cfg_attr(
    not(all(feature = "wayland", unix, not(target_os = "macos"))),
    allow(dead_code)
)]
struct SerialReader<R> {
    inner: Mutex<R>,
}

#[cfg_attr(
    not(all(feature = "wayland", unix, not(target_os = "macos"))),
    allow(dead_code)
)]
impl<R> SerialReader<R> {
    fn new(reader: R) -> Self {
        Self {
            inner: Mutex::new(reader),
        }
    }

    /// Run `read` on the reader unless another read holds it.
    fn read<T>(
        &self,
        read: impl FnOnce(&R) -> Result<T, ClipboardError>,
    ) -> Result<T, ClipboardError> {
        match self.inner.try_lock() {
            Ok(reader) => read(&reader),
            Err(TryLockError::WouldBlock) => Err(ClipboardError::Get(
                "an earlier clipboard read is still waiting".to_string(),
            )),
            // A read that panicked left no partial state behind: the next
            // request starts a fresh exchange.
            Err(TryLockError::Poisoned(poisoned)) => read(&poisoned.into_inner()),
        }
    }
}

#[cfg(all(feature = "wayland", unix, not(target_os = "macos")))]
mod wayland {
    //! The native Wayland backend: `smithay-clipboard` 0.7.2.
    //!
    //! Each `smithay_clipboard::Clipboard` runs a worker thread with its
    //! own event queue on the shared `wl_display` and binds its own seat
    //! devices, so it tracks keyboard focus and input serials itself and
    //! needs no events from winit. It offers and accepts
    //! `text/plain;charset=utf-8`, `UTF8_STRING` and `text/plain` on the
    //! last seat that delivered a key or button event.

    use std::ffi::c_void;
    use std::sync::Mutex;

    use super::{ClipboardBackend, ClipboardError, SerialReader};

    /// Two clipboards on one display: `load` blocks on a reply with no
    /// timeout (until the selection owner closes its pipe), and replies
    /// come back on one channel per instance, so reads get their own
    /// instance, serialized by a [`SerialReader`], and copies never wait
    /// behind a stuck read.
    pub(super) struct WaylandBackend {
        /// Copies: `store` only queues a request for the worker and never
        /// blocks. The `Mutex` makes the `!Sync` handle shareable; it is
        /// held only for that send.
        copy: Mutex<smithay_clipboard::Clipboard>,
        /// Pastes: one `load` at a time.
        paste: SerialReader<smithay_clipboard::Clipboard>,
    }

    impl WaylandBackend {
        /// # Safety
        ///
        /// `display` is a valid `*mut wl_display` that stays connected
        /// for as long as the backend is alive (see
        /// [`super::create_clipboard`]).
        pub(super) unsafe fn new(display: *mut c_void) -> Self {
            // SAFETY: both instances share the caller's guarantee that
            // `display` outlives the backend, which owns them.
            let (copy, paste) = unsafe {
                (
                    smithay_clipboard::Clipboard::new(display),
                    smithay_clipboard::Clipboard::new(display),
                )
            };
            Self {
                copy: Mutex::new(copy),
                paste: SerialReader::new(paste),
            }
        }
    }

    impl ClipboardBackend for WaylandBackend {
        /// Queues the copy for the worker. Without keyboard focus or any
        /// input event on a seat the compositor accepts no selection and
        /// the copy is dropped by the worker (no error reaches here).
        fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
            let copy = self
                .copy
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            copy.store(text);
            Ok(())
        }

        fn get_text(&self) -> Result<String, ClipboardError> {
            self.paste.read(|paste| {
                paste
                    .load()
                    .map_err(|err| ClipboardError::Get(err.to_string()))
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;
    use std::sync::mpsc;
    use std::time::Duration;

    use winit::raw_window_handle::{WaylandDisplayHandle, XcbDisplayHandle, XlibDisplayHandle};

    use super::*;

    #[test]
    fn wayland_display_gets_the_wayland_backend() {
        let wayland = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull::dangling()));
        assert_eq!(clipboard_kind(&wayland), ClipboardKind::Wayland);
    }

    #[test]
    fn x11_displays_get_arboard() {
        let xlib = RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0));
        let xcb = RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0));
        assert_eq!(clipboard_kind(&xlib), ClipboardKind::Arboard);
        assert_eq!(clipboard_kind(&xcb), ClipboardKind::Arboard);
    }

    /// A fake reader whose read blocks until the test answers it.
    struct BlockingReader {
        replies: Mutex<mpsc::Receiver<String>>,
    }

    impl BlockingReader {
        fn load(&self) -> Result<String, ClipboardError> {
            self.replies
                .lock()
                .unwrap()
                .recv()
                .map_err(|err| ClipboardError::Get(err.to_string()))
        }
    }

    #[test]
    fn a_read_behind_a_stuck_read_fails_at_once_and_replies_stay_matched() {
        let (answer, replies) = mpsc::channel();
        let reader = Arc::new(SerialReader::new(BlockingReader {
            replies: Mutex::new(replies),
        }));
        let (started_tx, started) = mpsc::channel();
        let first = {
            let reader = reader.clone();
            std::thread::spawn(move || {
                reader.read(|r| {
                    started_tx.send(()).unwrap();
                    r.load()
                })
            })
        };
        started.recv_timeout(Duration::from_secs(5)).unwrap();

        // The first read is still waiting: the second fails without
        // touching the reader.
        let second = reader.read(|r| r.load());
        assert!(matches!(second, Err(ClipboardError::Get(_))), "{second:?}");

        // The late answer goes to the read that asked for it, and the
        // reader is free again afterwards.
        answer.send("first".to_string()).unwrap();
        assert_eq!(first.join().unwrap().unwrap(), "first");
        answer.send("third".to_string()).unwrap();
        assert_eq!(reader.read(|r| r.load()).unwrap(), "third");
    }

    #[test]
    fn a_panicked_read_leaves_the_reader_usable() {
        let reader = Arc::new(SerialReader::new(()));
        let panicking = reader.clone();
        let _ = std::thread::spawn(move || {
            panicking.read(|_| -> Result<(), ClipboardError> { panic!("read failed") })
        })
        .join();
        assert_eq!(reader.read(|_| Ok(7)).unwrap(), 7);
    }
}
