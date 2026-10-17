//! Kitty graphics file media through the vendored PTY event loop (ADR-014,
//! "Media reads"): the file is read with the terminal lock and its lease
//! released, so a slow filesystem cannot freeze the window, and the reply
//! still comes in stream order.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Filter, MarkerContext, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollEvent, PollMode, Poller};

use nora::engine::Engine;
use nora::stream::StreamFilter;

struct Reader(UnixStream);

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

struct FakePty {
    reader: Reader,
    writer: UnixStream,
}

impl EventedReadWrite for FakePty {
    type Reader = Reader;
    type Writer = UnixStream;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the socket lives in this fake PTY, which the event loop
        // owns until after it deregisters it on exit.
        unsafe { poll.add_with_mode(&self.reader.0, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        poll.modify_with_mode(&self.reader.0, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        poll.delete(&self.reader.0)
    }

    fn reader(&mut self) -> &mut Reader {
        &mut self.reader
    }

    fn writer(&mut self) -> &mut UnixStream {
        &mut self.writer
    }
}

impl OnResize for FakePty {
    fn on_resize(&mut self, _: WindowSize) {}
}

impl EventedPty for FakePty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        None
    }
}

/// The real stream filter, checking at each unlocked run that another
/// thread can take the terminal's fair lock (which also needs the lease).
struct Probe<T: Send + 'static> {
    inner: StreamFilter,
    term: Arc<FairMutex<T>>,
    runs: Arc<AtomicUsize>,
    lock_was_free: Arc<AtomicBool>,
}

impl<T: Send + 'static> Filter for Probe<T> {
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        Filter::process(&mut self.inner, bytes)
    }

    fn on_marker(&mut self, position: usize, context: &MarkerContext) -> Option<Vec<u8>> {
        Filter::on_marker(&mut self.inner, position, context)
    }

    fn needs_unlocked(&self) -> bool {
        self.inner.needs_unlocked()
    }

    fn run_unlocked(&mut self) {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let term = Arc::clone(&self.term);
        std::thread::spawn(move || {
            drop(term.lock());
            let _ = tx.send(());
        });
        if rx.recv_timeout(Duration::from_secs(5)).is_ok() {
            self.lock_was_free.store(true, Ordering::SeqCst);
        }
        self.inner.run_unlocked();
    }
}

#[test]
fn a_file_is_read_without_the_terminal_lock() {
    use base64::Engine as _;
    let dir = std::env::temp_dir().join(format!("nora-media-unlocked-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("pixels.rgba");
    std::fs::write(&file, [255u8, 0, 0, 255].repeat(2)).expect("write pixels");
    let path = base64::engine::general_purpose::STANDARD.encode(file.to_string_lossy().as_bytes());

    let mut engine = Engine::with_history(24, 80, 100);
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    theirs.set_nonblocking(true).expect("nonblocking");
    let writer = theirs.try_clone().expect("clone");
    let pty = FakePty {
        reader: Reader(theirs),
        writer,
    };
    let runs = Arc::new(AtomicUsize::new(0));
    let lock_was_free = Arc::new(AtomicBool::new(false));
    let probe = Probe {
        inner: StreamFilter::new(engine.stream_targets()),
        term: engine.term_handle(),
        runs: Arc::clone(&runs),
        lock_was_free: Arc::clone(&lock_was_free),
    };
    let event_loop = EventLoop::new(engine.term_handle(), engine.recorder(), pty, false, false)
        .expect("event loop")
        .with_filter(Box::new(probe));
    let sender = event_loop.channel();
    let handle = event_loop.spawn();

    let stream = format!("ab\x1b_Ga=T,t=f,s=2,v=1,i=7;{path}\x1b\\cd\x1b[c");
    (&ours).write_all(stream.as_bytes()).expect("write");
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.replies().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = sender.send(Msg::Shutdown);
    handle.join().expect("event loop thread");
    let _ = std::fs::remove_dir_all(&dir);

    let replies = engine.replies();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0], "\x1b_Gi=7;OK\x1b\\", "the image reply first");
    assert!(replies[1].starts_with("\x1b[?"), "then DA1: {replies:?}");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "one unlocked read");
    assert!(
        lock_was_free.load(Ordering::SeqCst),
        "another thread took the terminal lock during the read"
    );
    assert!(engine.graphics().image(7).is_some());
    // The image was placed after "ab" and the text after it follows the
    // cursor movement (1 column: 2 px at 8 px cells).
    let snap = engine.snapshot();
    assert_eq!(snap.images.len(), 1);
    assert_eq!((snap.images[0].row, snap.images[0].col), (0, 2));
    assert_eq!(snap.cursor, Some((0, 5)));
}
