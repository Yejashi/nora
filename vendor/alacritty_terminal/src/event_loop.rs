//! The main event loop which performs I/O on the pseudoterminal.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::JoinHandle;
use std::time::Instant;

use log::error;
use polling::{Event as PollingEvent, Events, PollMode, Poller};

use crate::event::{self, Event, EventListener, WindowSize};
use crate::sync::FairMutex;
use crate::term::Term;
use crate::{thread, tty};
use vte::ansi;

/// Max bytes to read from the PTY before forced terminal synchronization.
pub(crate) const READ_BUFFER_SIZE: usize = 0x10_0000;

/// Max bytes to read from the PTY while the terminal is locked.
const MAX_LOCKED_READ: usize = u16::MAX as usize;

/// nora: most bytes the exit drain reads after the child exited (output
/// still queued in the PTY), and how long it may take.
pub const MAX_EXIT_DRAIN: usize = 4 * 1024 * 1024;
pub const EXIT_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

/// Messages that may be sent to the `EventLoop`.
#[derive(Debug)]
pub enum Msg {
    /// Data that should be written to the PTY.
    Input(Cow<'static, [u8]>),

    /// Indicates that the `EventLoop` should shut down, as Alacritty is shutting down.
    Shutdown,

    /// Instruction to resize the PTY.
    Resize(WindowSize),
}

/// A byte-level pre-filter over raw PTY bytes, run before the parser.
///
/// Implementations observe the bytes (e.g. to extract kitty-graphics APCs
/// that the vte parser drops) and return the bytes to feed to the parser.
/// The returned bytes must keep the parser in sync with the stream (a
/// prefix, or a transformation that mirrors the parser's own transitions).
///
/// The filter runs on the PTY reader thread, in order, over every chunk
/// read from the PTY. It must not block for long and must not allocate
/// without bound.
pub trait Filter {
    /// Process a chunk of bytes.
    ///
    /// Returns `(forwarded, marker_positions)`. `forwarded` is the bytes
    /// to feed to the parser. `marker_positions` are offsets into
    /// `forwarded` at which the filter detected prompt markers (N35b);
    /// the event loop processes the forwarded bytes in segments around
    /// those offsets and calls [`Filter::on_marker`] after each.
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>);

    /// Called by the event loop after processing the segment ending at
    /// `position`, with the terminal at that point (N35b/N35d, ADR-014).
    /// Implementors use it to record each event's grid position; storing
    /// both scroll counters lets them recompute the event's current line
    /// later, which stays correct across scrolls and across vertical
    /// resizes that pull lines from history. Bytes returned are parsed
    /// right there, before the next segment (kitty graphics move the
    /// cursor this way).
    fn on_marker(&mut self, _position: usize, _context: &MarkerContext) -> Option<Vec<u8>> {
        None
    }
}

/// The terminal at a position a [`Filter`] reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkerContext {
    pub cursor_line: i32,
    pub cursor_col: usize,
    pub total_scrolled: u64,
    pub total_pulled: u64,
    pub columns: usize,
    pub screen_lines: usize,
    pub alt_screen: bool,
}

impl MarkerContext {
    /// The context of `term` now.
    pub fn of<T>(term: &Term<T>) -> Self {
        use crate::grid::Dimensions;
        let grid = term.grid();
        Self {
            cursor_line: grid.cursor.point.line.0,
            cursor_col: grid.cursor.point.column.0,
            total_scrolled: grid.total_scrolled(),
            total_pulled: grid.total_pulled(),
            columns: grid.columns(),
            screen_lines: grid.screen_lines(),
            alt_screen: term.mode().contains(crate::term::TermMode::ALT_SCREEN),
        }
    }
}

/// nora: the `CSI ? 2026 h` that begins a synchronized update.
const BSU: &[u8] = b"\x1b[?2026h";

/// nora: run `at_marker` on the terminal as of a filter position.
///
/// Inside a synchronized update the parser only buffers bytes, so the
/// terminal (and [`MarkerContext::of`]) would still show the state from
/// before the update. When bytes are buffered they are parsed first
/// (ending the update, which also emits their replies before any reply
/// `at_marker` sends), and the update is resumed afterwards so the rest of
/// it stays synchronized. `at_marker` gets the parser to parse injected
/// bytes with.
pub fn at_marker_position<T, U, R>(
    parser: &mut ansi::Processor<T>,
    terminal: &mut Term<U>,
    at_marker: impl FnOnce(&mut ansi::Processor<T>, &mut Term<U>) -> R,
) -> R
where
    T: ansi::Timeout,
    U: EventListener,
{
    let flushed = parser.sync_bytes_count() > 0;
    if flushed {
        parser.stop_sync(terminal);
    }
    let result = at_marker(parser, terminal);
    if flushed {
        parser.advance(terminal, BSU);
    }
    result
}

/// The main event loop.
///
/// Handles all the PTY I/O and runs the PTY parser which updates terminal
/// state.
pub struct EventLoop<T: tty::EventedPty, U: EventListener> {
    poll: Arc<Poller>,
    pty: T,
    rx: PeekableReceiver<Msg>,
    tx: Sender<Msg>,
    terminal: Arc<FairMutex<Term<U>>>,
    event_proxy: U,
    drain_on_exit: bool,
    ref_test: bool,
    /// Optional byte-level pre-filter (e.g. a graphics APC extractor).
    /// Runs on the PTY reader thread before the parser advances.
    filter: Option<Box<dyn Filter + Send>>,
    /// nora: bytes handed to this loop as `Msg::Input` and not yet written
    /// to the PTY. The sender adds, `pty_write` subtracts what the PTY
    /// accepted, so the owner can bound input end to end (the channel and
    /// the write list are otherwise unbounded).
    in_flight: Option<Arc<AtomicUsize>>,
    /// nora: whether a synchronized update is still pending after the
    /// last read (set by `pty_read`, cleared when the update times out),
    /// so the owner can hold its frame until the update ends.
    sync_pending: Option<Arc<AtomicBool>>,
}

impl<T, U> EventLoop<T, U>
where
    T: tty::EventedPty + event::OnResize + Send + 'static,
    U: EventListener + Send + 'static,
{
    /// Create a new event loop.
    pub fn new(
        terminal: Arc<FairMutex<Term<U>>>,
        event_proxy: U,
        pty: T,
        drain_on_exit: bool,
        ref_test: bool,
    ) -> io::Result<EventLoop<T, U>> {
        let (tx, rx) = mpsc::channel();
        let poll = Poller::new()?.into();
        Ok(EventLoop {
            poll,
            pty,
            tx,
            rx: PeekableReceiver::new(rx),
            terminal,
            event_proxy,
            drain_on_exit,
            ref_test,
            filter: None,
            in_flight: None,
            sync_pending: None,
        })
    }

    /// Set the byte-level pre-filter (e.g. a graphics APC extractor).
    ///
    /// The filter runs on the PTY reader thread, over every chunk read
    /// from the PTY, before the parser advances. Builder-style: consumes
    /// `self` and returns it.
    pub fn with_filter(mut self, filter: Box<dyn Filter + Send>) -> Self {
        self.filter = Some(filter);
        self
    }

    /// nora: install the shared in-flight byte counter (see the field).
    pub fn with_in_flight(mut self, counter: Arc<AtomicUsize>) -> Self {
        self.in_flight = Some(counter);
        self
    }

    /// nora: install the shared synchronized-update flag (see the field).
    pub fn with_sync_pending(mut self, flag: Arc<AtomicBool>) -> Self {
        self.sync_pending = Some(flag);
        self
    }

    pub fn channel(&self) -> EventLoopSender {
        EventLoopSender { sender: self.tx.clone(), poller: self.poll.clone() }
    }

    /// Drain the channel.
    ///
    /// Returns `false` when a shutdown message was received.
    fn drain_recv_channel(&mut self, state: &mut State) -> bool {
        while let Some(msg) = self.rx.recv() {
            match msg {
                Msg::Input(input) => state.write_list.push_back(input),
                Msg::Resize(window_size) => self.pty.on_resize(window_size),
                Msg::Shutdown => return false,
            }
        }

        true
    }

    /// nora: returns the bytes parsed this pass (upstream returns `()`).
    #[inline]
    fn pty_read<X>(
        &mut self,
        state: &mut State,
        buf: &mut [u8],
        mut writer: Option<&mut X>,
    ) -> io::Result<usize>
    where
        X: Write,
    {
        let mut unprocessed = 0;
        let mut processed = 0;
        // nora: a read error after bytes were already read (typically EIO
        // when the child exits and the slave closes) is deferred until
        // those bytes are parsed. Upstream returns immediately and drops
        // them whenever the lock was busy on the previous iteration, which
        // loses a fast-exiting child's final output.
        let mut deferred_err: Option<io::Error> = None;

        // Reserve the next terminal lock for PTY reading.
        let _terminal_lease = Some(self.terminal.lease());
        let mut terminal = None;

        loop {
            // Read from the PTY.
            match self.pty.reader().read(&mut buf[unprocessed..]) {
                // This is received on Windows/macOS when no more data is readable from the PTY.
                Ok(0) if unprocessed == 0 => break,
                Ok(got) => unprocessed += got,
                Err(err) => match err.kind() {
                    ErrorKind::Interrupted | ErrorKind::WouldBlock => {
                        // Go back to mio if we're caught up on parsing and the PTY would block.
                        if unprocessed == 0 {
                            break;
                        }
                    },
                    _ if unprocessed > 0 => deferred_err = Some(err),
                    // nora: bytes parsed on an earlier pass still need the
                    // redraw below before the error is returned.
                    _ if processed > 0 => {
                        deferred_err = Some(err);
                        break;
                    },
                    _ => return Err(err),
                },
            }

            // Attempt to lock the terminal.
            let terminal = match &mut terminal {
                Some(terminal) => terminal,
                None => terminal.insert(match self.terminal.try_lock_unfair() {
                    // Force block if we are at the buffer size limit.
                    None if unprocessed >= READ_BUFFER_SIZE || deferred_err.is_some() => {
                        self.terminal.lock_unfair()
                    },
                    None => continue,
                    Some(terminal) => terminal,
                }),
            };

            // Write a copy of the bytes to the ref test file.
            if let Some(writer) = &mut writer {
                writer.write_all(&buf[..unprocessed]).unwrap();
            }

            // Run the byte-level pre-filter (if present) before parsing.
            // The filter observes the bytes (e.g. to extract graphics
            // APCs) and returns the bytes to feed to the parser plus any
            // marker positions (N35b).
            let (to_parse, marker_positions) = match self.filter.as_mut() {
                Some(filter) => {
                    let (forwarded, positions) = filter.process(&buf[..unprocessed]);
                    (forwarded, positions)
                },
                None => (buf[..unprocessed].to_vec(), Vec::new()),
            };

            // Parse the incoming bytes. When the filter reported marker
            // positions (N35b), process in segments: markers are grid
            // no-ops, so the cursor line after each marker's segment is
            // the marker's own line. Report it to the filter.
            if marker_positions.is_empty() {
                state.parser.advance(&mut **terminal, &to_parse);
            } else {
                let mut pos = 0;
                for &mpos in &marker_positions {
                    state.parser.advance(&mut **terminal, &to_parse[pos..mpos]);
                    let filter = &mut self.filter;
                    at_marker_position(&mut state.parser, &mut **terminal, |parser, term| {
                        let context = MarkerContext::of(term);
                        if let Some(filter) = filter.as_mut()
                            && let Some(inject) = filter.on_marker(mpos, &context)
                        {
                            parser.advance(term, &inject);
                        }
                    });
                    pos = mpos;
                }
                state.parser.advance(&mut **terminal, &to_parse[pos..]);
            }

            processed += unprocessed;
            unprocessed = 0;

            // Assure we're not blocking the terminal too long unnecessarily.
            if processed >= MAX_LOCKED_READ || deferred_err.is_some() {
                break;
            }
        }

        // nora: queue a redraw only when no synchronized update is pending.
        // Upstream compared the buffered byte count with the bytes read,
        // but `at_marker_position` parses the buffered part of an update
        // and resumes it, so the count no longer says whether the
        // terminal holds a half-applied frame. The update's end (ESU on a
        // later read) or its timeout sends the redraw instead.
        let pending = state.parser.sync_timeout().sync_timeout().is_some();
        if let Some(flag) = &self.sync_pending {
            flag.store(pending, Ordering::Release);
        }
        if processed > 0 && !pending {
            self.event_proxy.send_event(Event::Wakeup);
        }

        match deferred_err {
            Some(err) => Err(err),
            None => Ok(processed),
        }
    }

    #[inline]
    fn pty_write(&mut self, state: &mut State) -> io::Result<()> {
        state.ensure_next();

        'write_many: while let Some(mut current) = state.take_current() {
            'write_one: loop {
                match self.pty.writer().write(current.remaining_bytes()) {
                    Ok(0) => {
                        state.set_current(Some(current));
                        break 'write_many;
                    },
                    Ok(n) => {
                        if let Some(counter) = &self.in_flight {
                            // Every byte written was counted when nora sent
                            // it, so the counter never drops below `n`.
                            counter.fetch_sub(n, Ordering::AcqRel);
                        }
                        current.advance(n);
                        if current.finished() {
                            state.goto_next();
                            break 'write_one;
                        }
                    },
                    Err(err) => {
                        state.set_current(Some(current));
                        match err.kind() {
                            ErrorKind::Interrupted | ErrorKind::WouldBlock => break 'write_many,
                            _ => return Err(err),
                        }
                    },
                }
            }
        }

        Ok(())
    }

    pub fn spawn(mut self) -> JoinHandle<(Self, State)> {
        thread::spawn_named("PTY reader", move || {
            let mut state = State::default();
            let mut buf = [0u8; READ_BUFFER_SIZE];

            let poll_opts = PollMode::Level;
            let mut interest = PollingEvent::readable(0);

            // Register TTY through EventedRW interface.
            if let Err(err) = unsafe { self.pty.register(&self.poll, interest, poll_opts) } {
                error!("Event loop registration error: {err}");
                return (self, state);
            }

            let mut events = Events::with_capacity(NonZeroUsize::new(1024).unwrap());

            let mut pipe = if self.ref_test {
                Some(File::create("./alacritty.recording").expect("create alacritty recording"))
            } else {
                None
            };

            'event_loop: loop {
                // Wakeup the event loop when a synchronized update timeout was reached.
                let handler = state.parser.sync_timeout();
                let timeout =
                    handler.sync_timeout().map(|st| st.saturating_duration_since(Instant::now()));

                events.clear();
                if let Err(err) = self.poll.wait(&mut events, timeout) {
                    match err.kind() {
                        ErrorKind::Interrupted => continue,
                        _ => {
                            error!("Event loop polling error: {err}");
                            break 'event_loop;
                        },
                    }
                }

                // Handle synchronized update timeout.
                if events.is_empty() && self.rx.peek().is_none() {
                    state.parser.stop_sync(&mut *self.terminal.lock());
                    // nora: the update ended (see `sync_pending`).
                    if let Some(flag) = &self.sync_pending {
                        flag.store(false, Ordering::Release);
                    }
                    self.event_proxy.send_event(Event::Wakeup);
                    continue;
                }

                // Handle channel events, if there are any.
                if !self.drain_recv_channel(&mut state) {
                    break;
                }

                for event in events.iter() {
                    match event.key {
                        tty::PTY_CHILD_EVENT_TOKEN => {
                            if let Some(tty::ChildEvent::Exited(status)) =
                                self.pty.next_child_event()
                            {
                                if let Some(status) = status {
                                    self.event_proxy.send_event(Event::ChildExit(status));
                                }
                                if self.drain_on_exit {
                                    // nora: one pass reads at most
                                    // `MAX_LOCKED_READ`; upstream read once
                                    // and dropped the rest of the child's
                                    // final output. Read until a pass reads
                                    // nothing or fails (EIO once drained),
                                    // within a byte and time bound.
                                    let deadline = Instant::now() + EXIT_DRAIN_TIMEOUT;
                                    let mut drained = 0;
                                    while let Ok(read @ 1..) =
                                        self.pty_read(&mut state, &mut buf, pipe.as_mut())
                                    {
                                        drained += read;
                                        if drained >= MAX_EXIT_DRAIN || Instant::now() >= deadline
                                        {
                                            break;
                                        }
                                    }
                                }
                                self.terminal.lock().exit();
                                self.event_proxy.send_event(Event::Wakeup);
                                break 'event_loop;
                            }
                        },

                        tty::PTY_READ_WRITE_TOKEN => {
                            if event.is_interrupt() {
                                // Don't try to do I/O on a dead PTY.
                                continue;
                            }

                            if event.readable {
                                if let Err(err) = self.pty_read(&mut state, &mut buf, pipe.as_mut())
                                {
                                    // On Linux, a `read` on the master side of a PTY can fail
                                    // with `EIO` if the client side hangs up.  In that case,
                                    // just loop back round for the inevitable `Exited` event.
                                    // This sucks, but checking the process is either racy or
                                    // blocking.
                                    #[cfg(target_os = "linux")]
                                    if err.raw_os_error() == Some(libc::EIO) {
                                        continue;
                                    }

                                    error!("Error reading from PTY in event loop: {err}");
                                    break 'event_loop;
                                }
                            }

                            if event.writable {
                                if let Err(err) = self.pty_write(&mut state) {
                                    error!("Error writing to PTY in event loop: {err}");
                                    break 'event_loop;
                                }
                            }
                        },
                        _ => (),
                    }
                }

                // Register write interest if necessary.
                let needs_write = state.needs_write();
                if needs_write != interest.writable {
                    interest.writable = needs_write;

                    // Re-register with new interest.
                    self.pty.reregister(&self.poll, interest, poll_opts).unwrap();
                }
            }

            // The evented instances are not dropped here so deregister them explicitly.
            let _ = self.pty.deregister(&self.poll);

            (self, state)
        })
    }
}

/// Helper type which tracks how much of a buffer has been written.
struct Writing {
    source: Cow<'static, [u8]>,
    written: usize,
}

pub struct Notifier(pub EventLoopSender);

impl event::Notify for Notifier {
    fn notify<B>(&self, bytes: B)
    where
        B: Into<Cow<'static, [u8]>>,
    {
        let bytes = bytes.into();
        // Terminal hangs if we send 0 bytes through.
        if bytes.is_empty() {
            return;
        }

        let _ = self.0.send(Msg::Input(bytes));
    }
}

impl event::OnResize for Notifier {
    fn on_resize(&mut self, window_size: WindowSize) {
        let _ = self.0.send(Msg::Resize(window_size));
    }
}

#[derive(Debug)]
pub enum EventLoopSendError {
    /// Error polling the event loop.
    Io(io::Error),

    /// Error sending a message to the event loop.
    Send(mpsc::SendError<Msg>),
}

impl Display for EventLoopSendError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            EventLoopSendError::Io(err) => err.fmt(f),
            EventLoopSendError::Send(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for EventLoopSendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            EventLoopSendError::Io(err) => err.source(),
            EventLoopSendError::Send(err) => err.source(),
        }
    }
}

#[derive(Clone)]
pub struct EventLoopSender {
    sender: Sender<Msg>,
    poller: Arc<Poller>,
}

impl EventLoopSender {
    pub fn send(&self, msg: Msg) -> Result<(), EventLoopSendError> {
        self.sender.send(msg).map_err(EventLoopSendError::Send)?;
        // nora: once the message is queued it is accepted, whatever the
        // wake-up does. Returning the notify error here made callers treat
        // queued input as unsent and resend it (duplicate bytes, in-flight
        // counted twice). The loop still drains the channel on its next
        // wake-up (the next PTY event or a later successful notify).
        if let Err(err) = self.poller.notify() {
            error!("Event loop notify failed after queueing a message: {err}");
        }
        Ok(())
    }
}

/// All of the mutable state needed to run the event loop.
///
/// Contains list of items to write, current write state, etc. Anything that
/// would otherwise be mutated on the `EventLoop` goes here.
#[derive(Default)]
pub struct State {
    write_list: VecDeque<Cow<'static, [u8]>>,
    writing: Option<Writing>,
    parser: ansi::Processor,
}

impl State {
    #[inline]
    fn ensure_next(&mut self) {
        if self.writing.is_none() {
            self.goto_next();
        }
    }

    #[inline]
    fn goto_next(&mut self) {
        self.writing = self.write_list.pop_front().map(Writing::new);
    }

    #[inline]
    fn take_current(&mut self) -> Option<Writing> {
        self.writing.take()
    }

    #[inline]
    fn needs_write(&self) -> bool {
        self.writing.is_some() || !self.write_list.is_empty()
    }

    #[inline]
    fn set_current(&mut self, new: Option<Writing>) {
        self.writing = new;
    }
}

impl Writing {
    #[inline]
    fn new(c: Cow<'static, [u8]>) -> Writing {
        Writing { source: c, written: 0 }
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.written += n;
    }

    #[inline]
    fn remaining_bytes(&self) -> &[u8] {
        &self.source[self.written..]
    }

    #[inline]
    fn finished(&self) -> bool {
        self.written >= self.source.len()
    }
}

struct PeekableReceiver<T> {
    rx: Receiver<T>,
    peeked: Option<T>,
}

impl<T> PeekableReceiver<T> {
    fn new(rx: Receiver<T>) -> Self {
        Self { rx, peeked: None }
    }

    fn peek(&mut self) -> Option<&T> {
        if self.peeked.is_none() {
            self.peeked = self.rx.try_recv().ok();
        }

        self.peeked.as_ref()
    }

    fn recv(&mut self) -> Option<T> {
        if self.peeked.is_some() {
            self.peeked.take()
        } else {
            match self.rx.try_recv() {
                Err(TryRecvError::Disconnected) => panic!("event loop channel closed"),
                res => res.ok(),
            }
        }
    }
}
