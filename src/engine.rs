//! Headless adapter around the pinned `alacritty_terminal` engine.
//!
//! [`Engine`] owns one long-lived `Term` and one long-lived `vte` parser, so
//! PTY bytes may be fed in arbitrary chunks without losing escape-sequence
//! state. Engine-specific types (`Term`, `Processor`, `Colors`, points,
//! cursors) stay inside this module; only the owned, engine-free types
//! ([`Snapshot`], [`CellText`], [`Damage`], [`LineDamage`],
//! [`EventRecord`]) cross the boundary.
//!
//! Contract:
//! - [`Engine::new`] runs with **zero scrollback** (`total_lines == rows`),
//!   so grid lines equal viewport rows and `display_offset` is always 0;
//!   [`Engine::with_history`] admits `scrollback` lines of history, and
//!   `display_offset` tracks the viewport's position within it.
//! - [`Engine::snapshot`] consumes one damage report: the first snapshot of a
//!   fresh engine is [`Damage::Full`] (the engine starts fully damaged); each
//!   later snapshot reports only what changed since the previous snapshot.
//! - [`Engine::feed`] preserves parser state across calls; partial escape
//!   sequences are completed by later feeds.
//! - PTY reply bytes (DSR and similar queries) arrive as
//!   [`EventRecord::PtyWrite`] and are exposed via [`Engine::replies`].

use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::term::{
    ClipboardType, Config, Osc52, Term, TermDamage, TermMode, point_to_viewport,
};
use alacritty_terminal::vte;
use alacritty_terminal::vte::ansi::{Color, CursorShape, Rgb};

/// Headless terminal engine with ordered event capture.
pub struct Engine {
    /// Shared term: owned by the engine and, for PTY sessions, also driven
    /// by the pinned event loop on its own thread (the deliberately
    /// encapsulated synchronization boundary).
    term: Arc<FairMutex<Term<Recorder>>>,
    processor: vte::ansi::Processor<vte::ansi::StdSyncHandler>,
    rows: usize,
    cols: usize,
    /// Per-capture monotonic snapshot epoch (starts at 0; the first
    /// [`Engine::snapshot`] is epoch 1).
    snapshot_epoch: u64,
    recorder: Recorder,
}

/// Viewport size for a zero-scrollback [`Term`].
struct Size {
    rows: usize,
    cols: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Owned record of one engine event, in arrival order.
///
/// `Event::ClipboardLoad`, `Event::ColorRequest` and
/// `Event::TextAreaSizeRequest` carry engine-internal `Arc<dyn Fn>`
/// formatters; the records keep those formatters so the session layer
/// can resolve the requests into replies. Formatters are neither
/// comparable nor printable, so [`PartialEq`]/[`Eq`] compare variant and
/// scalar fields only (closures compare equal within a variant) and
/// `Debug` prints the request identity.
#[derive(Clone)]
pub enum EventRecord {
    MouseCursorDirty,
    Title(String),
    ResetTitle,
    ClipboardStore(ClipboardType, String),
    ClipboardLoad(ClipboardType, Arc<dyn Fn(&str) -> String + Sync + Send>),
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
    PtyWrite(String),
    TextAreaSizeRequest(Arc<dyn Fn(WindowSize) -> String + Sync + Send>),
    CursorBlinkingChange,
    Wakeup,
    Bell,
    Exit,
    ChildExit(ExitStatus),
}

impl std::fmt::Debug for EventRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventRecord::MouseCursorDirty => write!(f, "MouseCursorDirty"),
            EventRecord::Title(title) => write!(f, "Title({title:?})"),
            EventRecord::ResetTitle => write!(f, "ResetTitle"),
            EventRecord::ClipboardStore(clipboard, text) => {
                write!(f, "ClipboardStore({clipboard:?}, {text:?})")
            }
            EventRecord::ClipboardLoad(clipboard, _) => write!(f, "ClipboardLoad({clipboard:?})"),
            EventRecord::ColorRequest(index, _) => write!(f, "ColorRequest({index})"),
            EventRecord::PtyWrite(text) => write!(f, "PtyWrite({text})"),
            EventRecord::TextAreaSizeRequest(_) => write!(f, "TextAreaSizeRequest"),
            EventRecord::CursorBlinkingChange => write!(f, "CursorBlinkingChange"),
            EventRecord::Wakeup => write!(f, "Wakeup"),
            EventRecord::Bell => write!(f, "Bell"),
            EventRecord::Exit => write!(f, "Exit"),
            EventRecord::ChildExit(status) => write!(f, "ChildExit({status:?})"),
        }
    }
}

impl PartialEq for EventRecord {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (EventRecord::MouseCursorDirty, EventRecord::MouseCursorDirty) => true,
            (EventRecord::Title(a), EventRecord::Title(b)) => a == b,
            (EventRecord::ResetTitle, EventRecord::ResetTitle) => true,
            (EventRecord::ClipboardStore(a, x), EventRecord::ClipboardStore(b, y)) => {
                a == b && x == y
            }
            (EventRecord::ClipboardLoad(a, _), EventRecord::ClipboardLoad(b, _)) => a == b,
            (EventRecord::ColorRequest(a, _), EventRecord::ColorRequest(b, _)) => a == b,
            (EventRecord::PtyWrite(a), EventRecord::PtyWrite(b)) => a == b,
            (EventRecord::TextAreaSizeRequest(_), EventRecord::TextAreaSizeRequest(_)) => true,
            (EventRecord::CursorBlinkingChange, EventRecord::CursorBlinkingChange) => true,
            (EventRecord::Wakeup, EventRecord::Wakeup) => true,
            (EventRecord::Bell, EventRecord::Bell) => true,
            (EventRecord::Exit, EventRecord::Exit) => true,
            (EventRecord::ChildExit(a), EventRecord::ChildExit(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for EventRecord {}

/// Hard entry cap for the recorder (drop-oldest FIFO).
const MAX_RECORDS: usize = 256;
/// Hard byte cap for the recorder (drop-oldest FIFO): the sum of the
/// String-owned field lengths of the pending records.
const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// The recorder's shared state: the ordered pending records and their
/// total String-owned byte count, bounded in entries and bytes.
#[derive(Clone, Debug, Default)]
struct RecorderState {
    events: Vec<EventRecord>,
    bytes: usize,
}

/// Per-record byte cost for the recorder cap: the sum of that variant's
/// String-owned field lengths (unit and Arc-only variants cost nothing).
/// `PtyWrite` holds owned reply bytes (String) and counts toward the
/// byte cap exactly like the other String-owned variants.
fn event_record_cost(record: &EventRecord) -> usize {
    match record {
        EventRecord::Title(text) => text.len(),
        EventRecord::ClipboardStore(_, text) => text.len(),
        EventRecord::PtyWrite(text) => text.len(),
        _ => 0,
    }
}

/// Ordered event sink shared between the engine, its owner, and any PTY
/// event loop driven against the engine's term.
///
/// Pending state is bounded: at most [`MAX_RECORDS`] records and
/// [`MAX_RECORD_BYTES`] String-owned bytes (drop-oldest FIFO). Replaceable
/// presentation state coalesces before eviction: a new title replaces a
/// pending undrained title (latest wins), and a bell while one is pending
/// is skipped.
#[derive(Clone, Default)]
pub struct Recorder {
    events: Arc<Mutex<RecorderState>>,
}

impl EventListener for Recorder {
    fn send_event(&self, event: Event) {
        let mut state = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let record = to_record(event);

        // Coalesce replaceable presentation state before eviction.
        match &record {
            EventRecord::Title(_) => {
                // Latest title wins: replace the pending undrained
                // title. The pending record's cost is part of
                // `state.bytes` (the sum over all pending records), so
                // the delta is exact.
                let cost = event_record_cost(&record);
                if let Some(pos) = state
                    .events
                    .iter()
                    .rev()
                    .position(|r| matches!(r, EventRecord::Title(_)))
                {
                    let pos = state.events.len() - 1 - pos;
                    let pending = std::mem::replace(&mut state.events[pos], record);
                    state.bytes = state.bytes - event_record_cost(&pending) + cost;
                    return;
                }
            }
            EventRecord::Bell if state.events.iter().any(|r| matches!(r, EventRecord::Bell)) => {
                return;
            }
            _ => {}
        }

        // Bounded retention: drop oldest first. A single record over the
        // byte cap evicts everything else and remains alone.
        state.bytes += event_record_cost(&record);
        state.events.push(record);
        // Never evict the last remaining record: a single record over the
        // byte cap evicts everything else and remains alone.
        while state.events.len() > 1
            && (state.events.len() > MAX_RECORDS || state.bytes > MAX_RECORD_BYTES)
        {
            let oldest = state.events.remove(0);
            state.bytes -= event_record_cost(&oldest);
        }
    }
}

fn to_record(event: Event) -> EventRecord {
    match event {
        Event::MouseCursorDirty => EventRecord::MouseCursorDirty,
        Event::Title(title) => EventRecord::Title(title),
        Event::ResetTitle => EventRecord::ResetTitle,
        Event::ClipboardStore(clipboard, text) => EventRecord::ClipboardStore(clipboard, text),
        Event::ClipboardLoad(clipboard, format) => EventRecord::ClipboardLoad(clipboard, format),
        Event::ColorRequest(index, format) => EventRecord::ColorRequest(index, format),
        Event::PtyWrite(bytes) => EventRecord::PtyWrite(bytes),
        Event::TextAreaSizeRequest(format) => EventRecord::TextAreaSizeRequest(format),
        Event::CursorBlinkingChange => EventRecord::CursorBlinkingChange,
        Event::Wakeup => EventRecord::Wakeup,
        Event::Bell => EventRecord::Bell,
        Event::Exit => EventRecord::Exit,
        Event::ChildExit(status) => EventRecord::ChildExit(status),
    }
}

/// One screen cell, owned and engine-free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellText {
    /// Viewport row (0 = top).
    pub row: usize,
    /// Viewport column (0 = left).
    pub col: usize,
    /// Character in the cell (engine-internal for wide-char spacers).
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    /// Zerowidth (combining) characters stacked on this cell, in arrival
    /// order.
    pub zerowidth: Vec<char>,
    /// SGR 58 underline color, if set.
    pub underline_color: Option<Color>,
    /// Hyperlink identity `(id, uri)`, if set (owned, engine-free).
    pub hyperlink: Option<(String, String)>,
}

impl CellText {
    /// Display width in columns, derived from the pinned wide-char flags:
    /// a full wide char spans 2, a wide-char spacer spans 0, everything
    /// else spans 1.
    pub fn width(&self) -> u16 {
        if self.flags.contains(Flags::WIDE_CHAR) {
            2
        } else if self
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            0
        } else {
            1
        }
    }
}

/// One owned screen snapshot with viewport metadata.
#[derive(Clone)]
pub struct Snapshot {
    pub rows: usize,
    pub cols: usize,
    /// Per-capture monotonic identity (the first snapshot is epoch 1).
    pub epoch: u64,
    /// Row-major: `cells[row * cols + col]`.
    pub cells: Vec<CellText>,
    /// Cursor in viewport coordinates.
    pub cursor: Option<(usize, usize)>,
    pub cursor_shape: CursorShape,
    pub modes: TermMode,
    /// Engine color table; compared implicitly through [`CellText::fg`]/`bg`.
    pub colors: Colors,
    /// Damage for this snapshot (consumed on read, see module docs).
    pub damage: Damage,
    /// Viewport offset into the scrollback history (0 = bottom).
    pub display_offset: usize,
}

/// Content identity only: [`Snapshot::epoch`] is deliberately not compared,
/// so two captures of the same screen are equal.
impl PartialEq for Snapshot {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.cols == other.cols
            && self.cells == other.cells
            && self.cursor == other.cursor
            && self.cursor_shape == other.cursor_shape
            && self.modes == other.modes
            && self.damage == other.damage
    }
}

impl Eq for Snapshot {}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("epoch", &self.epoch)
            .field("cells", &self.cells)
            .field("cursor", &self.cursor)
            .field("cursor_shape", &self.cursor_shape)
            .field("modes", &self.modes)
            .field("damage", &self.damage)
            .field("display_offset", &self.display_offset)
            .finish()
    }
}

/// What changed since the previous snapshot (or since creation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damage {
    /// The whole screen is damaged.
    Full,
    /// Only the listed line ranges are damaged.
    Partial(Vec<LineDamage>),
}

/// One damaged line range, in viewport coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineDamage {
    pub line: usize,
    /// First damaged column (inclusive).
    pub left: usize,
    /// Last damaged column (inclusive).
    pub right: usize,
}

impl Engine {
    /// Create a headless engine with a `rows` × `cols` viewport and no
    /// scrollback.
    pub fn new(rows: usize, cols: usize) -> Self {
        Self::with_history(rows, cols, 0)
    }

    /// Create a headless engine with a `rows` × `cols` viewport and
    /// `scrollback` lines of history above the viewport.
    pub fn with_history(rows: usize, cols: usize, scrollback: usize) -> Self {
        // `scrollback` lines of history: with zero history, grid lines
        // equal viewport rows, so display_offset is always 0 and
        // cursor/viewport math is the identity.
        // OSC 52 store/load are denied: the pinned term emits no events and
        // no replies for clipboard queries while `Osc52::Disabled`.
        let config = Config {
            scrolling_history: scrollback,
            osc52: Osc52::Disabled,
            ..Default::default()
        };
        let size = Size { rows, cols };
        let recorder = Recorder::default();
        let term = Arc::new(FairMutex::new(Term::new(config, &size, recorder.clone())));
        Self {
            term,
            processor: vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new(),
            rows,
            cols,
            snapshot_epoch: 0,
            recorder,
        }
    }

    /// Resize the viewport to `rows` × `cols`.
    ///
    /// The pinned `Term::resize` marks the terminal fully damaged, so the
    /// next [`Engine::snapshot`] reports [`Damage::Full`].
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let size = Size { rows, cols };
        let mut term = self.term.lock();
        term.resize(size);
        drop(term);
        self.rows = rows;
        self.cols = cols;
    }

    /// Viewport height in rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Viewport width in columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// The shared term handle: for PTY sessions the pinned event loop
    /// drives the same [`Term`] on its own thread (the deliberately
    /// encapsulated synchronization boundary).
    pub fn term_handle(&self) -> Arc<FairMutex<Term<Recorder>>> {
        Arc::clone(&self.term)
    }

    /// A clone of the shared recorder, so an event loop driven against
    /// this engine's term appends its events to the same ordered stream
    /// as the engine's own events.
    pub fn recorder(&self) -> Recorder {
        self.recorder.clone()
    }

    /// Feed raw PTY bytes into the engine, preserving parser state.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut term = self.term.lock();
        self.processor.advance(&mut *term, bytes);
    }

    /// Current terminal modes.
    pub fn modes(&self) -> TermMode {
        *self.term.lock().mode()
    }

    /// Raw color-table read under the shared term guard (pinned
    /// `Term::colors()`): `Some` when the slot is set, `None` when it is
    /// unset (no seeding here — unset-slot defaults are applied at
    /// resolution time by the session layer). Indices beyond the table
    /// are reported as unset.
    pub fn color(&self, index: usize) -> Option<Rgb> {
        if index >= COUNT {
            return None;
        }
        self.term.lock().colors()[index]
    }

    /// Cursor position in viewport coordinates (0-based row, column).
    /// Returns `None` when the cursor is outside the visible viewport
    /// (scrolled above or below the current display window).
    pub fn cursor(&self) -> Option<(usize, usize)> {
        let term = self.term.lock();
        let point = term.grid().cursor.point;
        let display_offset = term.grid().display_offset();
        point_to_viewport(display_offset, point)
            .filter(|p| p.line < self.rows)
            .map(|p| (p.line, p.column.0))
    }

    /// Scroll the viewport by `scroll` (N14a): `Delta` moves the display
    /// offset by lines (positive = into history), clamped to the history
    /// bounds; `PageUp`/`PageDown`/`Top`/`Bottom` jump within it.
    pub fn scroll(&self, scroll: Scroll) {
        let mut term = self.term.lock();
        term.scroll_display(scroll);
    }

    /// The viewport's offset into the scrollback history (0 = bottom).
    pub fn display_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// Number of scrollback lines currently stored above the viewport.
    pub fn history_size(&self) -> usize {
        let term = self.term.lock();
        let grid = term.grid();
        grid.total_lines().saturating_sub(grid.screen_lines())
    }

    /// Take one owned snapshot of the screen, consuming one damage report.
    pub fn snapshot(&mut self) -> Snapshot {
        let mut term = self.term.lock();
        let damage = term.damage();
        let damage = match damage {
            TermDamage::Full => Damage::Full,
            TermDamage::Partial(lines) => Damage::Partial(
                lines
                    .map(|bounds| LineDamage {
                        line: bounds.line,
                        left: bounds.left,
                        right: bounds.right,
                    })
                    .collect(),
            ),
        };

        let content = term.renderable_content();
        // Cell extras are copied to owned, engine-free data under the term
        // guard, via the pinned `Cell` accessors. Grid points are mapped
        // through `point_to_viewport` so history lines (negative grid
        // lines) land in viewport rows; out-of-viewport cells are skipped.
        let display_offset = content.display_offset;
        let cells: Vec<CellText> = content
            .display_iter
            .filter_map(|cell| {
                let point = point_to_viewport(display_offset, cell.point)?;
                Some(CellText {
                    row: point.line,
                    col: point.column.0,
                    ch: cell.cell.c,
                    fg: cell.cell.fg,
                    bg: cell.cell.bg,
                    flags: cell.cell.flags,
                    zerowidth: cell
                        .cell
                        .zerowidth()
                        .map(|s| s.to_vec())
                        .unwrap_or_default(),
                    underline_color: cell.cell.underline_color(),
                    hyperlink: cell
                        .cell
                        .hyperlink()
                        .map(|h| (h.id().to_string(), h.uri().to_string())),
                })
            })
            .collect();

        let cursor = point_to_viewport(content.display_offset, content.cursor.point)
            .map(|p| (p.line, p.column.0));

        // Increment before assignment: the first snapshot is epoch 1.
        self.snapshot_epoch += 1;

        let snapshot = Snapshot {
            rows: self.rows,
            cols: self.cols,
            epoch: self.snapshot_epoch,
            cells,
            cursor,
            cursor_shape: content.cursor.shape,
            modes: content.mode,
            colors: *content.colors,
            damage,
            display_offset,
        };
        term.reset_damage();
        snapshot
    }

    /// Inspect pending damage without consuming it: the same damage
    /// conversion and cursor read as [`Engine::snapshot`], but no
    /// `reset_damage` — the following `snapshot()` reads and resets
    /// (engine.rs snapshot tail). The pinned `Term::damage` re-damages
    /// the cursor cell on every call (alacritty_terminal-0.26.0
    /// term/mod.rs:477-485), which is race-free here: the PTY loop
    /// thread is the sole feeder, and this probe runs on the owner
    /// thread under the same term guard as `snapshot()`.
    pub fn probe(&self) -> (Damage, Option<(usize, usize)>) {
        let mut term = self.term.lock();
        let damage = term.damage();
        let damage = match damage {
            TermDamage::Full => Damage::Full,
            TermDamage::Partial(lines) => Damage::Partial(
                lines
                    .map(|bounds| LineDamage {
                        line: bounds.line,
                        left: bounds.left,
                        right: bounds.right,
                    })
                    .collect(),
            ),
        };

        let content = term.renderable_content();
        let cursor = point_to_viewport(content.display_offset, content.cursor.point)
            .map(|p| (p.line, p.column.0));

        (damage, cursor)
    }

    /// All events captured so far, in arrival order.
    pub fn events(&self) -> Vec<EventRecord> {
        let state = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.events.clone()
    }

    /// Atomically take all events captured so far, in arrival order,
    /// leaving the recorder empty.
    pub fn drain_events(&self) -> Vec<EventRecord> {
        let mut state = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let events = std::mem::take(&mut state.events);
        state.bytes = 0;
        events
    }

    /// PTY reply bytes (DSR and similar queries) in arrival order.
    pub fn replies(&self) -> Vec<String> {
        let state = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state
            .events
            .iter()
            .filter_map(|event| match event {
                EventRecord::PtyWrite(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    /// Drop all captured events and replies.
    pub fn clear_events(&self) {
        let mut state = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.events.clear();
        state.bytes = 0;
    }
}
