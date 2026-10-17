//! The vendored PTY event loop with a fake PTY whose read fails (as a
//! Linux PTY master does with EIO once the child side hangs up) right
//! after a pass that already parsed bytes: the parsed output must still
//! be announced with a `Wakeup`.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::tty::{ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event as PollEvent, PollMode, Poller};

/// Reads from a socket once, then fails with EIO.
struct EioAfterData {
    stream: UnixStream,
    delivered: bool,
}

impl Read for EioAfterData {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.delivered {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        let got = self.stream.read(buf)?;
        self.delivered = true;
        Ok(got)
    }
}

struct FakePty {
    reader: EioAfterData,
    writer: UnixStream,
}

impl EventedReadWrite for FakePty {
    type Reader = EioAfterData;
    type Writer = UnixStream;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        // The loop passes the PTY read/write key (0) in `interest`.
        // SAFETY: the socket lives in this fake PTY, which the event loop
        // owns until after it deregisters it on exit.
        unsafe { poll.add_with_mode(&self.reader.stream, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: PollEvent,
        mode: PollMode,
    ) -> io::Result<()> {
        // The loop passes the PTY read/write key (0) in `interest`.
        poll.modify_with_mode(&self.reader.stream, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        poll.delete(&self.reader.stream)
    }

    fn reader(&mut self) -> &mut EioAfterData {
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

#[derive(Clone)]
struct Proxy(Arc<Mutex<Sender<Event>>>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let _ = self.0.lock().unwrap().send(event);
    }
}

fn wait_for_wakeup(rx: &Receiver<Event>) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(Event::Wakeup) => return true,
            Ok(_) => continue,
            Err(_) => return false,
        }
    }
    false
}

#[test]
fn eio_after_parsed_bytes_still_wakes() {
    let (ours, theirs) = UnixStream::pair().expect("socket pair");
    theirs.set_nonblocking(true).expect("nonblocking");
    let writer = theirs.try_clone().expect("clone");
    let pty = FakePty {
        reader: EioAfterData {
            stream: theirs,
            delivered: false,
        },
        writer,
    };

    let (tx, rx) = mpsc::channel();
    let proxy = Proxy(Arc::new(Mutex::new(tx)));
    let size = TermSize::new(80, 24);
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        proxy.clone(),
    )));
    let event_loop =
        EventLoop::new(Arc::clone(&term), proxy, pty, false, false).expect("event loop");
    let sender = event_loop.channel();
    let handle = event_loop.spawn();

    (&ours).write_all(b"hello").expect("write");
    let woke = wait_for_wakeup(&rx);

    let _ = sender.send(Msg::Shutdown);
    handle.join().expect("event loop thread");

    assert!(woke, "parsed output before the EIO must send a Wakeup");
    let term = term.lock();
    let first: String = (0..5)
        .map(|col| {
            term.grid()[alacritty_terminal::index::Line(0)][alacritty_terminal::index::Column(col)]
                .c
        })
        .collect();
    assert_eq!(first, "hello");
}
