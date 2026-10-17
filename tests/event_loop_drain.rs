//! The vendored PTY event loop drains the child's final output on exit:
//! with more queued than one locked read pass (64 KiB), every byte up to
//! the drain bound is parsed, so the last lines of `seq 1 200000` reach
//! the screen.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::EventLoop;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollEvent, PollMode, Poller};

/// Output already queued in the PTY: served in 4 KiB reads, then EIO (a
/// Linux PTY master once the child side hung up and the queue is empty).
struct Queued {
    data: Vec<u8>,
    pos: usize,
}

impl Read for Queued {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.data.len() {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let n = buf.len().min(4096).min(self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// A PTY whose read side never polls readable (only the exit drain reads
/// it) and whose child has already exited (the child source is readable).
struct FakePty {
    reader: Queued,
    idle: UnixStream,
    child: UnixStream,
    writer: UnixStream,
    exited: bool,
}

impl EventedReadWrite for FakePty {
    type Reader = Queued;
    type Writer = UnixStream;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: both sockets live in this fake PTY, which the event loop
        // owns until after it deregisters them on exit. Key 1 is the
        // loop's child-event token on Unix.
        unsafe {
            poll.add_with_mode(&self.idle, interest, mode)?;
            poll.add_with_mode(&self.child, PollEvent::readable(1), mode)
        }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        poll.modify_with_mode(&self.idle, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        poll.delete(&self.idle)?;
        poll.delete(&self.child)
    }

    fn reader(&mut self) -> &mut Queued {
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
        (!std::mem::replace(&mut self.exited, true)).then_some(ChildEvent::Exited(None))
    }
}

#[derive(Clone)]
struct Proxy(Arc<Mutex<Vec<Event>>>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

fn row_text<T>(term: &Term<T>, line: usize) -> String {
    (0..term.grid().columns())
        .map(|col| term.grid()[Line(line as i32)][Column(col)].c)
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[test]
fn exit_drain_reads_past_one_pass() {
    let data: Vec<u8> = (1..=200_000)
        .flat_map(|n| format!("{n}\r\n").into_bytes())
        .collect();
    assert!(data.len() > 1024 * 1024, "well past one 64 KiB pass");

    let (idle, _idle_peer) = UnixStream::pair().expect("socket pair");
    let (child, mut child_peer) = UnixStream::pair().expect("socket pair");
    child_peer.write_all(b"x").expect("child event");
    let (writer, _writer_peer) = UnixStream::pair().expect("socket pair");
    let pty = FakePty {
        reader: Queued { data, pos: 0 },
        idle,
        child,
        writer,
        exited: false,
    };

    let proxy = Proxy(Arc::new(Mutex::new(Vec::new())));
    let size = TermSize::new(80, 24);
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        proxy.clone(),
    )));
    let event_loop =
        EventLoop::new(Arc::clone(&term), proxy, pty, true, false).expect("event loop");
    let handle = event_loop.spawn();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !handle.is_finished() {
        assert!(
            std::time::Instant::now() < deadline,
            "the loop ends on exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = handle.join().expect("event loop thread");

    let term = term.lock();
    assert_eq!(row_text(&term, 22), "200000", "the last line was drained");
    assert_eq!(row_text(&term, 21), "199999");
}
