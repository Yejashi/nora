//! A synchronized update (`CSI ? 2026 h … l`) split across PTY reads is
//! drawn once, when it ends: a stream marker inside it (here `CSI 2 J`,
//! a graphics-filter position) parses the buffered bytes and resumes the
//! update, but the vendored loop sends no redraw and reports the update
//! as pending until the end marker arrives or the update times out.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Filter, Msg};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollEvent, PollMode, Poller};

const BSU: &[u8] = b"\x1b[?2026h";
const ESU: &[u8] = b"\x1b[?2026l";

struct FakePty {
    reader: UnixStream,
    writer: UnixStream,
}

impl EventedReadWrite for FakePty {
    type Reader = UnixStream;
    type Writer = UnixStream;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the socket lives in this fake PTY, which the event loop
        // owns until after it deregisters it on exit.
        unsafe { poll.add_with_mode(&self.reader, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        poll.modify_with_mode(&self.reader, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        poll.delete(&self.reader)
    }

    fn reader(&mut self) -> &mut UnixStream {
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

/// Reports a marker position after every `CSI 2 J`, as the graphics
/// filter does.
struct ClearMarks;

impl Filter for ClearMarks {
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        let positions = bytes
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == b"\x1b[2J")
            .map(|(i, _)| i + 4)
            .collect();
        (bytes.to_vec(), positions)
    }
}

#[derive(Clone)]
struct Proxy(Arc<Mutex<Sender<Event>>>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

fn wakeups(rx: &Receiver<Event>) -> usize {
    rx.try_iter().filter(|e| matches!(e, Event::Wakeup)).count()
}

fn wait_for_wakeup(rx: &Receiver<Event>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Wakeup) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

fn wait_until(flag: &AtomicBool, value: bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while flag.load(Ordering::Acquire) != value {
        assert!(Instant::now() < deadline, "sync flag never became {value}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn row(term: &Term<Proxy>, line: i32) -> String {
    (0..10)
        .map(|col| term.grid()[Line(line)][Column(col)].c)
        .collect()
}

type LoopHandle = std::thread::JoinHandle<(
    EventLoop<FakePty, Proxy>,
    alacritty_terminal::event_loop::State,
)>;

/// The loop over a fake PTY: our end of it, the events, the terminal,
/// the sync flag, the loop's sender and its thread.
#[allow(clippy::type_complexity)]
fn start() -> (
    UnixStream,
    Receiver<Event>,
    Arc<FairMutex<Term<Proxy>>>,
    Arc<AtomicBool>,
    alacritty_terminal::event_loop::EventLoopSender,
    LoopHandle,
) {
    start_with(Box::new(ClearMarks))
}

/// [`start`] with another filter.
#[allow(clippy::type_complexity)]
fn start_with(
    filter: Box<dyn Filter + Send>,
) -> (
    UnixStream,
    Receiver<Event>,
    Arc<FairMutex<Term<Proxy>>>,
    Arc<AtomicBool>,
    alacritty_terminal::event_loop::EventLoopSender,
    LoopHandle,
) {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    theirs.set_nonblocking(true).expect("nonblocking");
    let writer = theirs.try_clone().expect("clone");
    let pty = FakePty {
        reader: theirs,
        writer,
    };
    let (tx, rx) = mpsc::channel();
    let proxy = Proxy(Arc::new(Mutex::new(tx)));
    let size = TermSize::new(10, 4);
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        proxy.clone(),
    )));
    let pending = Arc::new(AtomicBool::new(false));
    let event_loop = EventLoop::new(Arc::clone(&term), proxy, pty, false, false)
        .expect("event loop")
        .with_filter(filter)
        .with_sync_pending(pending.clone());
    let sender = event_loop.channel();
    let handle = event_loop.spawn();
    (ours, rx, term, pending, sender, handle)
}

#[test]
fn split_update_is_drawn_once_at_its_end() {
    let (ours, rx, term, pending, sender, handle) = start();

    // First read: the update begins, clears the screen (a marker
    // position) and draws half its frame.
    let mut first = BSU.to_vec();
    first.extend_from_slice(b"\x1b[2J\x1b[Hhalf");
    (&ours).write_all(&first).expect("write");
    wait_until(&pending, true);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(wakeups(&rx), 0, "no redraw of a half-applied frame");

    // Second read: the rest of the frame and the end of the update.
    let mut second = b" done".to_vec();
    second.extend_from_slice(ESU);
    (&ours).write_all(&second).expect("write");
    assert!(wait_for_wakeup(&rx), "the end of the update redraws");
    assert!(!pending.load(Ordering::Acquire));
    assert_eq!(row(&term.lock(), 0), "half done ");

    let _ = sender.send(Msg::Shutdown);
    let _ = handle.join().expect("event loop thread");
}

#[test]
fn unfinished_update_redraws_when_it_times_out() {
    let (ours, rx, term, pending, sender, handle) = start();

    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"\x1b[2J\x1b[Hstuck");
    (&ours).write_all(&bytes).expect("write");
    wait_until(&pending, true);
    assert!(wait_for_wakeup(&rx), "the timeout redraws");
    assert!(!pending.load(Ordering::Acquire));
    assert_eq!(row(&term.lock(), 0), "stuck     ");

    let _ = sender.send(Msg::Shutdown);
    let _ = handle.join().expect("event loop thread");
}

/// An update that never ends, followed by continuous output with markers:
/// each marker resumes the update, which used to push its deadline back,
/// and the loop's timeout branch runs only when a poll finds nothing. The
/// update must still end at its own deadline while the output goes on.
#[test]
fn endless_update_with_markers_redraws_while_output_continues() {
    let (ours, rx, term, pending, sender, handle) = start();

    let mut first = BSU.to_vec();
    first.extend_from_slice(b"\x1b[2J\x1b[Hdead");
    (&ours).write_all(&first).expect("write");
    wait_until(&pending, true);
    let began = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let ours = ours.try_clone().expect("clone");
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                if (&ours).write_all(b"\x1b[2J\x1b[Hmore").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };
    let redrawn = wait_for_wakeup(&rx);
    let waited = began.elapsed();
    stop.store(true, Ordering::Release);
    writer.join().expect("writer");
    assert!(redrawn, "the update ended");
    assert!(
        waited < Duration::from_millis(400),
        "redrawn after {waited:?}, while output continued"
    );
    assert_eq!(row(&term.lock(), 0), "more      ");

    let _ = sender.send(Msg::Shutdown);
    let _ = handle.join().expect("event loop thread");
}

/// [`ClearMarks`] that, before its second marker, runs unlocked work (as
/// a kitty graphics file read does) and waits there until released.
struct UnlockedAtSecond {
    markers: usize,
    entered: Sender<()>,
    release: Receiver<()>,
    done: bool,
}

impl Filter for UnlockedAtSecond {
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        ClearMarks.process(bytes)
    }

    fn on_marker(
        &mut self,
        _: usize,
        _: &alacritty_terminal::event_loop::MarkerContext,
    ) -> Option<Vec<u8>> {
        self.markers += 1;
        None
    }

    fn needs_unlocked(&self) -> bool {
        self.markers == 1 && !self.done
    }

    fn run_unlocked(&mut self) {
        self.done = true;
        let _ = self.entered.send(());
        let _ = self.release.recv_timeout(Duration::from_secs(5));
    }
}

/// While the loop runs unlocked work inside a pending update (the lock is
/// free, and an earlier marker parsed part of the frame), the update is
/// already reported pending, so the owner draws no half-applied frame.
#[test]
fn unlocked_work_inside_an_update_reports_it_pending() {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (ours, rx, term, pending, sender, handle) = start_with(Box::new(UnlockedAtSecond {
        markers: 0,
        entered: entered_tx,
        release: release_rx,
        done: false,
    }));

    let mut bytes = BSU.to_vec();
    bytes.extend_from_slice(b"\x1b[2J\x1b[Hhalf\x1b[2J\x1b[Hdone");
    (&ours).write_all(&bytes).expect("write");
    entered
        .recv_timeout(Duration::from_secs(5))
        .expect("the loop ran the unlocked work");
    assert!(
        pending.load(Ordering::Acquire),
        "pending while the lock is free inside the update"
    );
    drop(term.lock());
    release.send(()).expect("release");
    (&ours).write_all(ESU).expect("write");
    assert!(wait_for_wakeup(&rx), "the end of the update redraws");
    wait_until(&pending, false);
    assert_eq!(row(&term.lock(), 0), "done      ");

    let _ = sender.send(Msg::Shutdown);
    let _ = handle.join().expect("event loop thread");
}
