//! The byte-level hooks between the child's output and the parser
//! (N32b, N35, ADR-013, ADR-014), shared by the PTY reader thread (as the
//! vendored event loop's `Filter`) and [`crate::engine::Engine::feed`].
//!
//! [`StreamFilter::process`] runs the prompt-marker filter, then the
//! graphics filter on its output (which forwards bytes unchanged), so the
//! positions of both kinds of event are offsets into the same forwarded
//! bytes. The parser processes those bytes in segments and calls
//! [`StreamFilter::on_marker`] at each position with the terminal there:
//! shell events are recorded at the cursor's line on the screen shown
//! (main or alternate, each with its own counters), graphics commands are
//! handled at the cursor, their replies are recorded in order with the
//! parser's own, and the bytes it returns (a cursor movement after an
//! image) are parsed right there. A command that names a file is read
//! without the terminal lock first (ADR-014, "Media reads"): by
//! [`StreamFilter::process`] for `Engine::feed`, and through the event
//! loop's `run_unlocked` hook on the PTY path.

use std::sync::{Arc, Mutex, PoisonError};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::MarkerContext;

use crate::engine::Recorder;
use crate::graphics_filter::{GraphicsEvent, GraphicsFilter, GraphicsSignal};
use crate::graphics_image::{
    Cursor, GraphicsState, LoadedMedia, MAX_IMAGE_BYTES, load_media, track_file_read,
};
use crate::prompt_marker::{
    HyperlinkAtLine, HyperlinkQueue, MarkerAtLine, MarkerQueue, PromptMarkerFilter, ShellEvent,
};
use crate::shell_input::ShellInput;

/// The engine state the stream hooks write to.
#[derive(Clone)]
pub struct StreamTargets {
    pub markers: Arc<Mutex<MarkerQueue>>,
    pub cwd: Arc<Mutex<Option<String>>>,
    pub shell_input: Arc<Mutex<ShellInput>>,
    pub hyperlinks: Arc<Mutex<HyperlinkQueue>>,
    pub graphics: Arc<Mutex<GraphicsState>>,
    /// Where replies go, in order with the parser's.
    pub recorder: Recorder,
}

enum Pending {
    Shell(ShellEvent),
    Graphics(GraphicsSignal),
    /// A graphics command that reads a file, with the file once loaded
    /// off the terminal lock (ADR-014, "Media reads").
    Media(GraphicsEvent, Option<LoadedMedia>),
}

/// Files [`StreamFilter::process`] loads ahead for `Engine::feed` (which
/// calls it before locking the terminal); past this many decoded bytes
/// in one chunk, the rest are loaded as they are handled.
const PRELOAD_BYTES: usize = MAX_IMAGE_BYTES;

/// The prompt-marker and graphics filters with the state they feed.
pub struct StreamFilter {
    markers: PromptMarkerFilter,
    graphics: GraphicsFilter,
    targets: StreamTargets,
    /// Events from the last `process`, in stream order; `on_marker` is
    /// called once per reported position, in order.
    pending: Vec<Pending>,
    pending_index: usize,
    /// The alternate screen session a prompt marker or hyperlink was
    /// queued on since the last shell event seen on another screen or
    /// session.
    alt_queued: Option<u64>,
}

impl StreamFilter {
    pub fn new(targets: StreamTargets) -> Self {
        Self {
            markers: PromptMarkerFilter::new(),
            graphics: GraphicsFilter::new(),
            targets,
            pending: Vec::new(),
            pending_index: 0,
            alt_queued: None,
        }
    }

    /// Filter a chunk: the bytes to parse and the positions at which
    /// [`StreamFilter::on_marker`] must be called. Files named by graphics
    /// commands are read here, up to [`PRELOAD_BYTES`]: the caller
    /// (`Engine::feed`) has not locked the terminal yet.
    pub fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        let filtered = self.filter(bytes);
        let mut loaded = 0;
        for index in 0..self.pending.len() {
            if loaded >= PRELOAD_BYTES {
                break;
            }
            loaded += self.load_media(index);
        }
        filtered
    }

    /// Whether the next reported position is a command whose file is not
    /// loaded yet.
    fn next_needs_file(&self) -> bool {
        matches!(
            self.pending.get(self.pending_index),
            Some(Pending::Media(_, None))
        )
    }

    /// Load the file of the pending command at `index`, if it is one that
    /// is not loaded yet; returns the decoded bytes now held for it.
    fn load_media(&mut self, index: usize) -> usize {
        match self.pending.get_mut(index) {
            Some(Pending::Media(event, media @ None)) => {
                let loaded = load_media(event);
                let bytes = loaded.bytes();
                *media = Some(loaded);
                bytes
            }
            _ => 0,
        }
    }

    /// The filters over a chunk, without reading files.
    fn filter(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        let (forward, shell) = self.markers.process(bytes);
        let (_, graphics) = self.graphics.process(&forward);
        self.pending.clear();
        self.pending_index = 0;
        if graphics.is_empty() {
            let positions = shell.iter().map(|(pos, _)| *pos).collect();
            self.pending = shell.into_iter().map(|(_, e)| Pending::Shell(e)).collect();
            return (forward, positions);
        }
        // Which commands read a file follows from the chunking state the
        // graphics state will have at each (every earlier command has been
        // handled; this batch's are followed in order).
        let mut in_chunk = self
            .targets
            .graphics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_chunk();
        let graphics = graphics.into_iter().map(|(pos, signal)| {
            let pending = match signal {
                GraphicsSignal::Command(event) if track_file_read(&event, &mut in_chunk) => {
                    Pending::Media(event, None)
                }
                GraphicsSignal::Command(event) => Pending::Graphics(GraphicsSignal::Command(event)),
                GraphicsSignal::Reset => {
                    in_chunk = false;
                    Pending::Graphics(GraphicsSignal::Reset)
                }
                other => Pending::Graphics(other),
            };
            (pos, pending)
        });
        // Merge by position. A shell event at offset p is a removed frame
        // that sat just before byte p, and a graphics command at p ended at
        // p, so at the same offset the graphics command came first.
        let mut merged: Vec<(usize, Pending)> = shell
            .into_iter()
            .map(|(pos, e)| (pos, Pending::Shell(e)))
            .chain(graphics)
            .collect();
        merged.sort_by_key(|(pos, event)| (*pos, matches!(event, Pending::Shell(_))));
        let positions = merged.iter().map(|(pos, _)| *pos).collect();
        self.pending = merged.into_iter().map(|(_, e)| e).collect();
        (forward, positions)
    }

    /// The parser reached the next reported position; returns bytes to
    /// parse there.
    pub fn on_marker(&mut self, context: &MarkerContext) -> Option<Vec<u8>> {
        let index = self.pending_index;
        self.pending_index += 1;
        match self.pending.get_mut(index)? {
            Pending::Shell(event) => {
                let event = event.clone();
                self.shell_event(event, context);
                None
            }
            Pending::Graphics(signal) => {
                let signal = std::mem::replace(signal, GraphicsSignal::Reset);
                self.graphics_signal(signal, None, context)
            }
            Pending::Media(event, media) => {
                // Loaded off the lock by both callers (`process` up to its
                // budget, the event loop through `run_unlocked`); past the
                // preload budget of `Engine::feed` it is loaded here.
                let event = GraphicsEvent {
                    command: event.command,
                    keys: std::mem::take(&mut event.keys),
                    data: std::mem::take(&mut event.data),
                };
                let media = media.take().unwrap_or_else(|| load_media(&event));
                self.graphics_signal(GraphicsSignal::Command(event), Some(media), context)
            }
        }
    }

    fn shell_event(&mut self, event: ShellEvent, at: &MarkerContext) {
        let t = &self.targets;
        let screen = at.alt_screen.then_some(at.alt_session);
        if self
            .alt_queued
            .is_some_and(|session| screen != Some(session))
        {
            // An event on the main screen, or on a later alternate screen
            // session, after alternate-screen entries were queued: their
            // session ended, and its markers and hyperlinks mean nothing
            // on the next one.
            self.alt_queued = None;
            t.markers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retire_alternate(screen);
            t.hyperlinks
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retire_alternate(screen);
        }
        if screen.is_some() && matches!(event, ShellEvent::Marker(_) | ShellEvent::Hyperlink(_)) {
            self.alt_queued = screen;
        }
        {
            let cwd = t.cwd.lock().unwrap_or_else(PoisonError::into_inner);
            t.shell_input
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .apply(&event, cwd.as_deref());
        }
        match event {
            ShellEvent::Marker(m) => {
                t.markers
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(MarkerAtLine {
                        line: at.cursor_line,
                        scrolled: at.total_scrolled,
                        pulled: at.total_pulled,
                        alt: at.alt_screen,
                        alt_session: at.alt_session,
                        event: m,
                    });
            }
            ShellEvent::Cwd(path) => {
                *t.cwd.lock().unwrap_or_else(PoisonError::into_inner) = Some(path);
            }
            ShellEvent::Hyperlink(uri) => {
                t.hyperlinks
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(HyperlinkAtLine {
                        line: at.cursor_line,
                        col: at.cursor_col,
                        scrolled: at.total_scrolled,
                        pulled: at.total_pulled,
                        alt: at.alt_screen,
                        alt_session: at.alt_session,
                        uri,
                    });
            }
            // Folded into the shell input state above.
            ShellEvent::Buffer(_)
            | ShellEvent::BufferEnd
            | ShellEvent::CommandLine(_)
            | ShellEvent::CommandExit(_) => {}
        }
    }

    fn graphics_signal(
        &mut self,
        signal: GraphicsSignal,
        media: Option<LoadedMedia>,
        at: &MarkerContext,
    ) -> Option<Vec<u8>> {
        let mut state = self
            .targets
            .graphics
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (cell_width, cell_height) = state.cell_size();
        let cursor = Cursor {
            line: at.cursor_line,
            col: at.cursor_col,
            scrolled: at.total_scrolled,
            pulled: at.total_pulled,
            columns: at.columns,
            lines: at.screen_lines,
            alt: at.alt_screen,
            alt_session: at.alt_session,
            cell_width,
            cell_height,
        };
        match signal {
            GraphicsSignal::Command(event) => {
                let outcome = state.handle_loaded(&event, &cursor, media);
                drop(state);
                if let Some(reply) = outcome.reply {
                    let reply = String::from_utf8_lossy(&reply).into_owned();
                    self.targets.recorder.send_event(Event::PtyWrite(reply));
                }
                outcome
                    .move_cursor
                    .map(|(columns, rows)| cursor_movement(columns, rows, at))
            }
            GraphicsSignal::ClearScreen => {
                state.clear_screen(&cursor);
                None
            }
            GraphicsSignal::ClearScrollback => {
                state.clear_scrollback(&cursor);
                None
            }
            GraphicsSignal::Reset => {
                state.reset();
                None
            }
        }
    }
}

/// The bytes that move the cursor past a displayed image, as kitty does:
/// down `rows - 1` lines (scrolling at the bottom), then right by
/// `columns`, to the next line's start when that reaches the right edge.
/// The line feeds stop at the screen height: past it, more only add blank
/// history, and an image's rows come from the client. They are IND
/// (`ESC D`), not LF: IND never returns the carriage, whatever line
/// feed/new line mode (`CSI 20 h`) says, so the column is kept.
fn cursor_movement(columns: u32, rows: u32, at: &MarkerContext) -> Vec<u8> {
    let feeds = (rows.saturating_sub(1) as usize).min(at.screen_lines);
    let mut out = b"\x1bD".repeat(feeds);
    if at.cursor_col + columns as usize >= at.columns {
        out.extend_from_slice(b"\r\n");
    } else if columns > 0 {
        out.extend_from_slice(format!("\x1b[{columns}C").as_bytes());
    }
    out
}

/// The PTY reader's hooks. `process` runs with the terminal locked, so it
/// reads no file; the event loop calls `run_unlocked` with the lock (and
/// its lease) released before a position whose command names one.
impl alacritty_terminal::event_loop::Filter for StreamFilter {
    fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<usize>) {
        self.filter(bytes)
    }

    fn on_marker(&mut self, _position: usize, context: &MarkerContext) -> Option<Vec<u8>> {
        StreamFilter::on_marker(self, context)
    }

    fn needs_unlocked(&self) -> bool {
        self.next_needs_file()
    }

    fn run_unlocked(&mut self) {
        self.load_media(self.pending_index);
    }
}
