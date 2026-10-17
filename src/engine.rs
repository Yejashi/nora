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
use std::sync::{Arc, Mutex, PoisonError};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
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
    /// Scrollback lines this engine was created with (re-applied when
    /// terminal options change).
    history: usize,
    recorder: Recorder,
    /// Shared selection state (N14b): the window thread and pump thread
    /// both touch it through `&self` accessors, so it lives behind a
    /// mutex. `None` when nothing is selected.
    selection: Arc<Mutex<Option<AnchoredSelection>>>,
    /// The prompt-marker and graphics filters for [`Engine::feed`] (the PTY
    /// path runs its own [`crate::stream::StreamFilter`] on the same state).
    stream: crate::stream::StreamFilter,
    /// Kitty graphics (ADR-014), shared with the PTY path's filter.
    graphics: Arc<Mutex<crate::graphics_image::GraphicsState>>,
    /// The graphics version the last snapshot showed.
    graphics_seen: u64,
    /// The cursor state the last snapshot showed (see [`CursorCells`]).
    cursor_seen: Option<CursorCells>,
    /// Byte-bounded queue of prompt-marker events, shared between the
    /// headless `Engine::feed` path and the PTY `EventLoop` filter (N35a).
    marker_queue: Arc<Mutex<crate::prompt_marker::MarkerQueue>>,
    /// Bounded store of detected prompt markers (N35b): drained from
    /// [`marker_queue`] and consulted by prompt navigation.
    marker_store: crate::prompt_marker::MarkerStore,
    /// The child's current working directory (N35d): the most recent OSC 7
    /// `file://` path, shared between the headless `Engine::feed` path and
    /// the PTY `EventLoop` filter. `None` until the child reports one.
    cwd: Arc<Mutex<Option<String>>>,
    /// The shell's reported command line state (ADR-013), shared between
    /// the headless `Engine::feed` path and the PTY `EventLoop` filter.
    shell_input: Arc<Mutex<crate::shell_input::ShellInput>>,
    /// Byte-bounded queue of hyperlink events, shared between the
    /// headless `Engine::feed` path and the PTY `EventLoop` filter (N35d).
    hyperlink_queue: Arc<Mutex<crate::prompt_marker::HyperlinkQueue>>,
    /// Bounded store of detected hyperlinks (N35d): drained from
    /// [`hyperlink_queue`] and consulted by hyperlink activation.
    hyperlink_store: crate::prompt_marker::HyperlinkStore,
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
    /// Records evicted under the caps since creation (diagnostics).
    dropped: u64,
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

/// Whether a record becomes bytes written back to the application (the
/// session resolves the request records into replies).
fn is_reply(record: &EventRecord) -> bool {
    matches!(
        record,
        EventRecord::PtyWrite(_)
            | EventRecord::ColorRequest(..)
            | EventRecord::TextAreaSizeRequest(_)
            | EventRecord::ClipboardLoad(..)
    )
}

/// Ordered event sink shared between the engine, its owner, and any PTY
/// event loop driven against the engine's term.
///
/// Pending state is bounded: at most [`MAX_RECORDS`] records and
/// [`MAX_RECORD_BYTES`] String-owned bytes (drop-oldest FIFO, replies to
/// the application last). Replaceable presentation state coalesces before
/// eviction: a new title or title reset replaces the pending one (latest
/// wins, appended),
/// and a bell, wakeup or mouse-cursor update while one is pending is
/// skipped.
#[derive(Clone, Default)]
pub struct Recorder {
    events: Arc<Mutex<RecorderState>>,
    /// Called after every recorded event (outside the record lock) so the
    /// consumer can block instead of polling. It runs on the PTY reader
    /// thread, under the terminal lock: it must be cheap and non-blocking.
    waker: Arc<Mutex<Option<Waker>>>,
}

/// A wake callback for [`Recorder`]; see [`Engine::set_waker`].
pub type Waker = Arc<dyn Fn() + Send + Sync>;

impl EventListener for Recorder {
    fn send_event(&self, event: Event) {
        self.record(event);
        let waker = self.waker.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(wake) = waker {
            wake();
        }
    }
}

impl Recorder {
    fn record(&self, event: Event) {
        let mut state = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let record = to_record(event);

        // Coalesce replaceable presentation state before eviction.
        match &record {
            EventRecord::Title(_) | EventRecord::ResetTitle => {
                // A title and a title reset share one slot, latest wins:
                // drop the pending one and append the new one. (Replacing
                // a pending title in place lost a later reset's order:
                // [Title(A), ResetTitle] + Title(B) became
                // [Title(B), ResetTitle], no title.) The pending record's
                // cost is part of `state.bytes`, so the delta is exact.
                if let Some(pos) = state
                    .events
                    .iter()
                    .position(|r| matches!(r, EventRecord::Title(_) | EventRecord::ResetTitle))
                {
                    let pending = state.events.remove(pos);
                    state.bytes -= event_record_cost(&pending);
                }
            }
            // A pending bell, redraw request or mouse-cursor update already
            // says everything a second one would.
            EventRecord::Bell | EventRecord::Wakeup | EventRecord::MouseCursorDirty
                if state
                    .events
                    .iter()
                    .any(|r| std::mem::discriminant(r) == std::mem::discriminant(&record)) =>
            {
                return;
            }
            _ => {}
        }

        state.bytes += event_record_cost(&record);
        state.events.push(record);
        // Bounded retention, oldest first. Never evict the last remaining
        // record: a single record over the byte cap evicts everything else
        // and remains alone. The child's exit record is never evicted:
        // losing it loses the exit status (audit P9). Replies to the
        // application are evicted only when nothing else is left to evict
        // (a reply flood), so presentation records can never push out a
        // pending reply.
        while state.events.len() > 1
            && (state.events.len() > MAX_RECORDS || state.bytes > MAX_RECORD_BYTES)
        {
            let Some(index) = state
                .events
                .iter()
                .position(|r| !is_reply(r) && !matches!(r, EventRecord::ChildExit(_)))
                .or_else(|| state.events.iter().position(is_reply))
            else {
                break;
            };
            let oldest = state.events.remove(index);
            state.bytes -= event_record_cost(&oldest);
            state.dropped += 1;
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

/// A text selection in viewport coordinates: `(row, col)` pairs, 0-based.
///
/// The selection is a **linear row-major range**: the cell at
/// `row * cols + col` is selected iff its index lies between
/// `start.0 * cols + start.1` and `end.0 * cols + end.1` (inclusive, in
/// either order). This matches how the window thread drags across cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

/// The stored selection: `(line id, col)` bounds in row-major order, where
/// a line id is the grid line plus [`line_shift`] (stable while output
/// scrolls lines into history and while the viewport scrolls), so the
/// highlight and the copied text stay on the text that was selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnchoredSelection {
    /// Where the selection was started (a drag extends from here).
    anchor: (i64, usize),
    lo: (i64, usize),
    hi: (i64, usize),
    /// Whether it was made on the alternate screen. Line ids are per
    /// screen, so on the other screen the selection is absent (no
    /// highlight, nothing to copy) until that screen is shown again.
    alt: bool,
}

/// Whether `term` shows its alternate screen.
fn alt_screen<T>(term: &Term<T>) -> bool {
    term.mode().contains(TermMode::ALT_SCREEN)
}

impl AnchoredSelection {
    /// The selection from `start` to `end` (`(line id, col)`, either
    /// order), started at `start`, on the screen `term` shows.
    fn new<T>(term: &Term<T>, start: (i64, usize), end: (i64, usize)) -> Self {
        Self {
            anchor: start,
            lo: start.min(end),
            hi: start.max(end),
            alt: alt_screen(term),
        }
    }

    /// Whether the selection belongs to the screen `term` shows.
    fn on_screen<T>(&self, term: &Term<T>) -> bool {
        self.alt == alt_screen(term)
    }

    /// Whether the cell at (`line_id`, `col`) lies in the range.
    fn contains(&self, line_id: i64, col: usize) -> bool {
        (line_id, col) >= self.lo && (line_id, col) <= self.hi
    }
}

/// The line id of viewport row `row` (see [`AnchoredSelection`]).
fn row_line_id(
    grid: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
    row: usize,
) -> i64 {
    row as i64 - grid.display_offset() as i64 + line_shift(grid)
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
    /// Per-cell selection state (N14b), same length as `cells`:
    /// `selected[row * cols + col]` is `true` when that cell is within
    /// the current selection.
    pub selected: Vec<bool>,
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
    /// Kitty graphics placements in the viewport (ADR-014), lowest z
    /// first.
    pub images: Vec<crate::graphics_image::VisibleImage>,
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
            && self.images.len() == other.images.len()
            && self.images.iter().zip(&other.images).all(|(a, b)| {
                (a.image_id, a.generation, a.row, a.col, a.z)
                    == (b.image_id, b.generation, b.row, b.col, b.z)
            })
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
            .field("images", &self.images.len())
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

/// Build the opener process argv for a hyperlink URI (N35d).
///
/// Only allowed schemes are opened: `http`, `https`, `mailto`, `file`,
/// and `ftp`. The URL is passed as a single argv element to the platform
/// opener (`xdg-open` on Linux) — never interpolated into a shell command.
/// Returns `None` for an empty URI or a disallowed/missing scheme.
pub fn opener_argv(uri: &str) -> Option<Vec<String>> {
    let uri = uri.trim();
    if uri.is_empty() {
        return None;
    }
    // The scheme is the part before the first `:`, `/`, or `?`. It must be
    // ASCII alphabetic (RFC 3986 schemes are `alpha *( alpha / digit /
    // "+" / "-" / "." )`; the allow-list below is alphabetic only).
    let scheme_end = uri.find([':', '/', '?'])?;
    let scheme = &uri[..scheme_end];
    if !scheme.chars().all(|c| c.is_ascii_alphabetic()) || scheme.is_empty() {
        return None;
    }
    let allowed = matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto" | "file" | "ftp"
    );
    if !allowed {
        return None;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    Some(vec![opener.to_string(), uri.to_string()])
}

/// Most matches [`Engine::search_text`] returns.
pub const MAX_SEARCH_HITS: usize = 10_000;

/// Lines [`Engine::search_text`] copies per terminal-lock hold.
const SEARCH_CHUNK_LINES: usize = 256;

/// One search match: its line and its first and last column.
///
/// `line_id` is stable while output arrives: the grid line at scan time
/// plus the lines scrolled into history minus the lines pulled back by
/// resizes since the terminal started. [`Engine::search_hit_line`] maps it
/// to the current grid line (0 = top screen row, negative = scrollback).
/// A match that continues past a soft wrap ends on a later row:
/// `end` is a column of the row `end_line_id` (`line_id` otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHit {
    pub line_id: i64,
    pub start: usize,
    pub end: usize,
    pub end_line_id: i64,
}

/// `total_scrolled - total_pulled`: adding it to a grid line gives the
/// line's stable id (see [`SearchHit`]).
fn line_shift(grid: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>) -> i64 {
    grid.total_scrolled() as i64 - grid.total_pulled() as i64
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
            // The engine keeps the kitty keyboard mode stack, answers
            // `CSI ? u` and exposes the flags as `TermMode` bits; the
            // window encodes keys under them (input::kitty).
            kitty_keyboard: true,
            ..Default::default()
        };
        let (rows, cols) = (rows.max(1), cols.max(1));
        let size = Size { rows, cols };
        let recorder = Recorder::default();
        let term = Arc::new(FairMutex::new(Term::new(config, &size, recorder.clone())));
        let targets = crate::stream::StreamTargets {
            markers: Arc::new(Mutex::new(crate::prompt_marker::MarkerQueue::new())),
            cwd: Arc::new(Mutex::new(None)),
            shell_input: Arc::new(Mutex::new(crate::shell_input::ShellInput::new())),
            hyperlinks: Arc::new(Mutex::new(crate::prompt_marker::HyperlinkQueue::new())),
            graphics: Arc::new(Mutex::new(crate::graphics_image::GraphicsState::new())),
            recorder: recorder.clone(),
        };
        let stream = crate::stream::StreamFilter::new(targets.clone());
        Self {
            term,
            processor: vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new(),
            rows,
            cols,
            snapshot_epoch: 0,
            history: scrollback,
            recorder,
            selection: Arc::new(Mutex::new(None)),
            stream,
            graphics: targets.graphics,
            graphics_seen: 0,
            cursor_seen: None,
            marker_queue: targets.markers,
            marker_store: crate::prompt_marker::MarkerStore::new(),
            cwd: targets.cwd,
            shell_input: targets.shell_input,
            hyperlink_queue: targets.hyperlinks,
            hyperlink_store: crate::prompt_marker::HyperlinkStore::new(),
        }
    }

    /// The cursor shape used until the application sets one (DECSCUSR).
    pub fn set_default_cursor_shape(&mut self, shape: CursorShape) {
        let config = Config {
            scrolling_history: self.history,
            osc52: Osc52::Disabled,
            kitty_keyboard: true,
            default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle {
                shape,
                blinking: false,
            },
            ..Default::default()
        };
        self.term.lock().set_options(config);
    }

    /// Resize the viewport to `rows` × `cols`.
    ///
    /// The pinned `Term::resize` marks the terminal fully damaged, so the
    /// next [`Engine::snapshot`] reports [`Damage::Full`]. Each axis is at
    /// least 1 (the pinned grid panics on an empty axis), and the stored
    /// geometry is the grid's, so snapshots stay consistent.
    pub fn resize(&mut self, rows: usize, cols: usize) {
        let (rows, cols) = (rows.max(1), cols.max(1));
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

    /// Install a wake callback, called after every event the PTY loop or
    /// the parser records (new output, child exit, replies). It runs under
    /// the terminal lock on the reader thread, so it must not block.
    pub fn set_waker(&self, waker: Waker) {
        *self
            .recorder
            .waker
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(waker);
    }

    /// Feed raw PTY bytes into the engine, preserving parser state.
    ///
    /// The same [`crate::stream::StreamFilter`] the PTY path uses runs
    /// first: prompt markers (N35a) and kitty graphics (ADR-014) are
    /// extracted, and the parser processes the bytes in segments around
    /// their positions, so each is handled with the cursor where it was
    /// (and a displayed image's cursor movement is parsed in place).
    pub fn feed(&mut self, bytes: &[u8]) {
        let (forward, positions) = self.stream.process(bytes);
        let mut term = self.term.lock();
        let mut pos = 0;
        for at in positions {
            self.processor.advance(&mut *term, &forward[pos..at]);
            let stream = &mut self.stream;
            alacritty_terminal::event_loop::at_marker_position(
                &mut self.processor,
                &mut *term,
                |parser, term| {
                    let context = alacritty_terminal::event_loop::MarkerContext::of(term);
                    if let Some(inject) = stream.on_marker(&context) {
                        parser.advance(term, &inject);
                    }
                },
            );
            pos = at;
        }
        self.processor.advance(&mut *term, &forward[pos..]);
    }

    /// The prompt-marker, shell and graphics state the stream hooks write
    /// to, for the PTY path's own [`crate::stream::StreamFilter`].
    pub fn stream_targets(&self) -> crate::stream::StreamTargets {
        crate::stream::StreamTargets {
            markers: Arc::clone(&self.marker_queue),
            cwd: Arc::clone(&self.cwd),
            shell_input: Arc::clone(&self.shell_input),
            hyperlinks: Arc::clone(&self.hyperlink_queue),
            graphics: Arc::clone(&self.graphics),
            recorder: self.recorder.clone(),
        }
    }

    /// The kitty graphics state (ADR-014).
    pub fn graphics(&self) -> std::sync::MutexGuard<'_, crate::graphics_image::GraphicsState> {
        self.graphics.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the images changed since the last snapshot, so a frame is
    /// due although the grid has no damage.
    pub fn graphics_changed(&self) -> bool {
        self.graphics().version() != self.graphics_seen
    }

    /// Whether the cursor, or the cells next to it, changed since the last
    /// snapshot in a way the term's damage does not report. The pinned
    /// `Term` damages no cell on `input` and relies on the old and new
    /// cursor positions, so a change that leaves the cursor in place (a
    /// character written into the last column with a wrap pending, a
    /// combining mark on the cell before the cursor, EL/ECH that clears
    /// only the cursor cell, DECTCEM, DECSCUSR) has the same damage shape
    /// as "nothing happened" (`is_noop_damage`).
    pub fn cursor_changed(&self) -> bool {
        let term = self.term.lock();
        self.cursor_seen.as_ref() != Some(&CursorCells::of(&term))
    }

    /// Drain the prompt-marker queue (N35a), returning all pending
    /// prompt-marker entries (with their grid lines, N35b) in order.
    ///
    /// Each drained entry is also inserted into the engine's
    /// [`MarkerStore`](crate::prompt_marker::MarkerStore) (N35b), so it
    /// can be consulted by prompt navigation. Entries that exceed the
    /// store's cap are dropped (the store returns `None` for them).
    pub fn drain_markers(&mut self) -> Vec<crate::prompt_marker::MarkerAtLine> {
        let entries = self
            .marker_queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain();
        for entry in &entries {
            self.marker_store.insert(entry.clone());
        }
        entries
    }

    /// The number of prompt markers in the engine's store (N35b).
    pub fn marker_count(&self) -> usize {
        self.marker_store.len()
    }

    /// Jump to the previous prompt (N35b).
    ///
    /// Finds the newest prompt marker (an `A` marker, which marks the
    /// start of a prompt) above the top of the view, computing each
    /// marker's current grid line (accounting for the grid's scrolls and
    /// vertical resizes since it was stored), and scrolls so that prompt
    /// is the top row, with its command's output below it. Each call goes
    /// one prompt further back; a prompt already in view (the shell's
    /// current one) is not a target.
    ///
    /// Returns the marker's ID, or `None` without scrolling when no prompt
    /// is above the view (no integration, or the oldest one is already
    /// shown). A prompt beyond the history scrolls to its top.
    pub fn jump_to_previous_prompt(&mut self) -> Option<usize> {
        let (scrolled, pulled, top, history) = {
            let term = self.term.lock();
            let grid = term.grid();
            (
                grid.total_scrolled(),
                grid.total_pulled(),
                -(grid.display_offset() as i32),
                grid.total_lines().saturating_sub(grid.screen_lines()),
            )
        };
        let (id, line) = self
            .marker_store
            .iter()
            .filter(|(_, entry)| entry.event.kind == crate::prompt_marker::MarkerKind::A)
            .filter_map(|(id, entry)| Some((id, entry.current_line(scrolled, pulled)?)))
            .filter(|&(_, line)| line < top)
            .max_by_key(|&(id, _)| id)?;
        let target = (line.unsigned_abs() as usize).min(history);
        let delta = target as i64 - self.display_offset() as i64;
        if delta != 0 {
            self.scroll(Scroll::Delta(delta as i32));
        }
        Some(id)
    }

    /// Select the previous command's text (N35c).
    ///
    /// A "command" is the range between a `C` marker (command start, emitted
    /// by the shell integration before the command's output) and the `D`
    /// marker that follows it (command end, emitted after the output). This
    /// selects the most recent command: from its `C` line to its last output
    /// line. If a command is still running (its `C` has been emitted but not
    /// yet its `D`), the selection runs from that `C` line to the bottom of
    /// the screen.
    ///
    /// The `D` marker lands on the line after the last output line when the
    /// output ends with a newline (the common case), so the selection ends
    /// at `D - 1`; when `D` shares the `C` line (no trailing newline) the
    /// whole command is that one line.
    ///
    /// The viewport is scrolled so the selection's end is at the bottom of
    /// the viewport. Returns the created [`Selection`], or `None` when no
    /// `C` marker is stored (no integration) or the command has scrolled out
    /// of the history (clean fallback).
    pub fn select_previous_command(&self) -> Option<Selection> {
        if self.rows == 0 || self.cols == 0 {
            return None;
        }
        use crate::prompt_marker::MarkerKind;
        // Find the latest `C` and the latest `D` marker (by ID).
        let mut latest_c: Option<(usize, &crate::prompt_marker::MarkerAtLine)> = None;
        let mut latest_d: Option<(usize, &crate::prompt_marker::MarkerAtLine)> = None;
        for (id, entry) in self.marker_store.iter() {
            match entry.event.kind {
                MarkerKind::C if latest_c.as_ref().is_none_or(|(cid, _)| id > *cid) => {
                    latest_c = Some((id, entry));
                }
                MarkerKind::D if latest_d.as_ref().is_none_or(|(did, _)| id > *did) => {
                    latest_d = Some((id, entry));
                }
                _ => {}
            }
        }
        let (c_id, c_entry) = latest_c?;
        // The command's end line: the latest `D` that follows this `C` (a
        // completed command), or the bottom row when the command is still
        // running (no `D` yet).
        let end_entry = latest_d.filter(|(d_id, _)| *d_id > c_id).map(|(_, e)| e);
        let bottom = self.rows as i32 - 1;
        let (scrolled, pulled, topmost) = {
            let term = self.term.lock();
            let grid = term.grid();
            (
                grid.total_scrolled(),
                grid.total_pulled(),
                grid.topmost_line().0,
            )
        };
        let end_line = match end_entry {
            Some(e) => e.current_line(scrolled, pulled)?,
            None => bottom,
        };
        let c_line = c_entry.current_line(scrolled, pulled)?;
        // The command's end must still be in the grid (its start is at or
        // above it, so one check suffices).
        if end_line < topmost {
            return None;
        }
        // The selection's end line. For a completed command, `D` lands on the
        // line after the last output line when the output ends with a newline
        // (the common case), so the selection ends at `D - 1`; when `D`
        // shares the `C` line (no trailing newline) the whole command is
        // that one line. For a command still running, select up to the
        // bottom of the screen.
        let end_grid = if end_entry.is_some() {
            if end_line > c_line {
                end_line - 1
            } else {
                end_line
            }
        } else {
            end_line
        };
        // Scroll so the selection's end is at the bottom of the viewport,
        // clamped to the history (a no-op when it is already visible).
        let target = (bottom - end_grid).max(0) as usize;
        let target = target.min(self.history_size());
        if target != self.display_offset() {
            let delta = target as i64 - self.display_offset() as i64;
            self.scroll(Scroll::Delta(delta as i32));
        }
        let offset = self.display_offset();
        let c_row = (c_line as i64 + offset as i64).max(0) as usize;
        let end_row = (end_grid as i64 + offset as i64).max(0) as usize;
        let start = (c_row.min(self.rows - 1), 0);
        let end = (end_row.min(self.rows - 1), self.cols - 1);
        self.select(start, end);
        Some(Selection { start, end })
    }

    /// A clone of the shared prompt-marker-queue handle (N35a): the PTY
    /// `EventLoop` filter pushes marker events into the same queue the
    /// headless [`Engine::feed`] path uses, so both paths feed one
    /// byte-bounded queue.
    pub fn markers_handle(&self) -> Arc<Mutex<crate::prompt_marker::MarkerQueue>> {
        Arc::clone(&self.marker_queue)
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

    /// Scroll the viewport so that grid line `line` (a
    /// [`alacritty_terminal::index::Line`], where `screen_lines - 1` is
    /// the bottom row and negative values are scrollback) is on the bottom
    /// row of the viewport (N35b). Lines at or below the bottom scroll to
    /// the bottom; lines above the scrollback are clamped to the top.
    pub fn scroll_to_line(&self, line: i32) {
        let target = {
            let term = self.term.lock();
            let bottom = term.grid().screen_lines() as i32 - 1;
            (bottom - line).max(0) as usize
        };
        let current = self.display_offset();
        let delta = target as i64 - current as i64;
        if delta != 0 {
            self.scroll(Scroll::Delta(delta as i32));
        }
    }

    /// The child's current working directory (N35d): the most recent OSC 7
    /// `file://` path, or `None` when the child has not reported one.
    pub fn cwd(&self) -> Option<String> {
        self.cwd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A clone of the shared hyperlink-queue handle (N35d): the PTY
    /// `EventLoop` filter pushes hyperlink events into the same queue the
    /// headless [`Engine::feed`] path uses.
    pub fn hyperlink_handle(&self) -> Arc<Mutex<crate::prompt_marker::HyperlinkQueue>> {
        Arc::clone(&self.hyperlink_queue)
    }

    /// The shell line editor's buffer while it is current (ADR-013): the
    /// last `OSC 6973` report after the prompt's input marker.
    pub fn shell_buffer(&self) -> Option<crate::shell_input::LineBuffer> {
        self.shell_input
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .current_buffer()
            .cloned()
    }

    /// Take the command lines the shell reported executing (ADR-013),
    /// oldest first, each with its cwd and exit status.
    pub fn take_executed(&self) -> Vec<crate::shell_input::ExecutedCommand> {
        self.shell_input
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take_executed()
    }

    /// A clone of the shared shell-input handle (ADR-013) for the PTY
    /// `EventLoop` filter.
    pub fn shell_input_handle(&self) -> Arc<Mutex<crate::shell_input::ShellInput>> {
        Arc::clone(&self.shell_input)
    }

    /// A clone of the shared cwd handle (N35d): the PTY `EventLoop` filter
    /// writes the child's current directory into the same cell the
    /// headless `Engine::feed` path writes and [`Engine::cwd`] reads.
    pub fn cwd_handle(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.cwd)
    }

    /// Drain the hyperlink queue (N35d), returning all pending hyperlink
    /// entries in order.
    ///
    /// Each drained entry is also inserted into the engine's
    /// [`HyperlinkStore`](crate::prompt_marker::HyperlinkStore), so it can
    /// be consulted by hyperlink activation.
    pub fn drain_hyperlinks(&mut self) -> Vec<crate::prompt_marker::HyperlinkAtLine> {
        let entries = self
            .hyperlink_queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain();
        for entry in &entries {
            self.hyperlink_store.insert(entry.clone());
        }
        entries
    }

    /// The number of hyperlinks in the engine's store (N35d).
    pub fn hyperlink_count(&self) -> usize {
        self.hyperlink_store.len()
    }

    /// Find the hyperlink at the viewport position `(row, col)` (N35d).
    ///
    /// Returns the URI of the hyperlink whose current grid line matches
    /// the clicked viewport row and whose start column is at or before
    /// `col`. When several hyperlinks match, the one with the greatest
    /// start column (the most recent link start on that line) is returned.
    /// Returns `None` when no hyperlink matches.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<String> {
        let (target_line, scrolled_now, pulled_now) = {
            let term = self.term.lock();
            let display_offset = term.grid().display_offset() as i32;
            let line = row as i32 - display_offset;
            (
                line,
                term.grid().total_scrolled(),
                term.grid().total_pulled(),
            )
        };
        let mut best: Option<(usize, String)> = None;
        for (_id, entry) in self.hyperlink_store.iter() {
            if entry.col > col {
                continue;
            }
            if entry.current_line(scrolled_now, pulled_now) != Some(target_line) {
                continue;
            }
            if best.as_ref().is_none_or(|(bc, _)| entry.col >= *bc) {
                best = Some((entry.col, entry.uri.clone()));
            }
        }
        best.map(|(_, uri)| uri)
    }

    /// Activate the hyperlink at the viewport position `(row, col)`
    /// (N35d).
    ///
    /// Returns the opener process argv for the hyperlink at that
    /// position, or `None` when there is no hyperlink there or its scheme
    /// is not allowed. The URL is passed as a single argv element (never
    /// interpolated into a shell command). The matched hyperlink is
    /// consumed (removed from the queue) whether or not its scheme is
    /// allowed.
    pub fn activate_hyperlink(&mut self, row: usize, col: usize) -> Option<Vec<String>> {
        let mut queue = self
            .hyperlink_queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let idx = queue
            .iter()
            .find(|(_, h)| h.line == row as i32 && h.col == col)
            .map(|(i, _)| i);
        match idx {
            Some(i) => queue.remove_at(i).and_then(|h| opener_argv(&h.uri)),
            None => None,
        }
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

    /// Every occurrence of `query` in the scrollback and the screen, top
    /// to bottom: the newest [`MAX_SEARCH_HITS`] when there are more.
    /// Matching follows soft wraps (a match may continue onto the next
    /// row; it is reported on the row it starts on) and includes combining
    /// marks; it ignores case unless the query has an uppercase letter.
    ///
    /// The scan runs bottom-up and copies [`SEARCH_CHUNK_LINES`] lines per
    /// hold of the terminal lock, so a long history never stalls the PTY
    /// reader for the whole scan (audit RV3). Lines are tracked by stable
    /// id, so output arriving between chunks does not skip or repeat any.
    pub fn search_text(&self, query: &str) -> Vec<SearchHit> {
        use alacritty_terminal::index::{Column, Line};
        let mut hits = Vec::new();
        if query.is_empty() {
            return hits;
        }
        let fold = !query.chars().any(char::is_uppercase);
        let norm = |c: char| {
            if fold {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                c
            }
        };
        let needle: Vec<char> = query.chars().map(norm).collect();
        // One searchable character: (char, stable line id, first column,
        // last column). A wide char spans two columns; combining marks
        // share their base cell's columns.
        type ScanChar = (char, i64, usize, usize);
        // The row below the one being matched, when that row soft-wraps
        // into it: its id and the first `needle.len() - 1` characters from
        // it on (all a match starting above can reach).
        let mut below: Option<(i64, Vec<ScanChar>)> = None;
        // Stable id of the next line to scan (moving up), once started.
        let mut next: Option<i64> = None;
        'scan: loop {
            // (stable line id, characters, soft-wraps into the next row).
            type ScanLine = (i64, Vec<ScanChar>, bool);
            let mut lines: Vec<ScanLine> = Vec::new();
            {
                let term = self.term.lock();
                let grid = term.grid();
                let shift = line_shift(grid);
                let top = -(grid.total_lines().saturating_sub(grid.screen_lines()) as i64);
                let bottom = grid.screen_lines() as i64 - 1;
                let cols = grid.columns();
                let mut line = next.map_or(bottom, |id| (id - shift).min(bottom));
                while line >= top && lines.len() < SEARCH_CHUNK_LINES {
                    let row = &grid[Line(line as i32)];
                    let id = line + shift;
                    let mut chars = Vec::with_capacity(cols);
                    for c in 0..cols {
                        let cell = &row[Column(c)];
                        // Skip wide-char spacers, including the one that
                        // pads a row whose wide char wrapped to the next.
                        if cell
                            .flags
                            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                        {
                            continue;
                        }
                        let last =
                            (c + usize::from(cell.flags.contains(Flags::WIDE_CHAR))).min(cols - 1);
                        chars.push((norm(cell.c), id, c, last));
                        for &mark in cell.zerowidth().unwrap_or_default() {
                            chars.push((norm(mark), id, c, last));
                        }
                    }
                    let wraps = row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
                    lines.push((id, chars, wraps));
                    line -= 1;
                }
                next = Some(line + shift);
            }
            if lines.is_empty() {
                break;
            }
            for (line_id, chars, wraps) in lines {
                // The characters a match starting on this row may run into.
                let tail: &[ScanChar] = match &below {
                    Some((id, tail)) if wraps && *id == line_id + 1 => tail,
                    _ => &[],
                };
                let total = chars.len() + tail.len();
                let at = |i: usize| {
                    if i < chars.len() {
                        &chars[i]
                    } else {
                        &tail[i - chars.len()]
                    }
                };
                if total >= needle.len() {
                    // Right to left, so a full result keeps the newest.
                    let last_start = (total - needle.len()).min(chars.len().saturating_sub(1));
                    for start in (0..=last_start).rev() {
                        if start >= chars.len() {
                            continue;
                        }
                        if (0..needle.len()).all(|k| at(start + k).0 == needle[k]) {
                            // `end` covers a final wide char's right half
                            // (audit RV10).
                            let end = at(start + needle.len() - 1);
                            hits.push(SearchHit {
                                line_id,
                                start: chars[start].2,
                                end: end.3,
                                end_line_id: end.1,
                            });
                            if hits.len() >= MAX_SEARCH_HITS {
                                break 'scan;
                            }
                        }
                    }
                }
                // This row (and what it wraps into) is the tail of the row
                // above, if that one wraps.
                let keep = needle.len() - 1;
                let mut tail_above: Vec<ScanChar> = chars.into_iter().take(keep).collect();
                if tail_above.len() < keep {
                    let more = keep - tail_above.len();
                    tail_above.extend(tail.iter().take(more).copied());
                }
                below = Some((line_id, tail_above));
            }
        }
        hits.reverse();
        hits
    }

    /// The current grid line of `hit` (0 = top screen row, negative =
    /// scrollback), or `None` once it has left the history.
    pub fn search_hit_line(&self, hit: SearchHit) -> Option<i32> {
        let term = self.term.lock();
        let grid = term.grid();
        let line = hit.line_id - line_shift(grid);
        let top = -(grid.total_lines().saturating_sub(grid.screen_lines()) as i64);
        let bottom = grid.screen_lines() as i64 - 1;
        (top..=bottom)
            .contains(&line)
            .then(|| i32::try_from(line).ok())
            .flatten()
    }

    /// Scroll `hit` into view and select it, so the selection highlight
    /// marks the match. The search bar covers the bottom row, so a match
    /// there or off screen is scrolled to the row above it (the live
    /// screen's bottom row cannot scroll higher).
    pub fn show_search_hit(&self, hit: SearchHit) {
        let Some(line) = self.search_hit_line(hit) else {
            self.clear_selection();
            return;
        };
        let rows = self.rows as i32;
        // Rows not under the search bar (all of them in a one-row view).
        let usable = if rows > 1 { rows - 1 } else { rows };
        let visible = |offset: usize, limit: i32| {
            let row = line + offset as i32;
            (0..limit).contains(&row).then_some(row as usize)
        };
        let row = match visible(self.display_offset(), usable) {
            Some(row) => row,
            None => {
                self.scroll_to_line(line + (rows - usable));
                match visible(self.display_offset(), rows) {
                    Some(row) => row,
                    None => return,
                }
            }
        };
        // A wrapped match ends on a later row; past the bottom of the view
        // the highlight stops at the last cell.
        let end_row = row + hit.end_line_id.saturating_sub(hit.line_id).max(0) as usize;
        let end = if end_row < self.rows {
            (end_row, hit.end)
        } else {
            (self.rows - 1, self.cols - 1)
        };
        self.select((row, hit.start), end);
    }

    /// Set the selection to the range `start..=end` in viewport
    /// `(row, col)` coordinates (N14b). The range is linear row-major;
    /// `start` and `end` may be given in either order.
    ///
    /// The range is anchored to the text under it when it is set: output
    /// that scrolls lines into history and viewport scrolling move the
    /// highlight with that text, and copying takes it even once it is off
    /// screen (lines that left the history are skipped).
    pub fn select(&self, start: (usize, usize), end: (usize, usize)) {
        let anchored = {
            let term = self.term.lock();
            let grid = term.grid();
            let anchor = |(row, col): (usize, usize)| (row_line_id(grid, row), col);
            AnchoredSelection::new(&term, anchor(start), anchor(end))
        };
        *self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(anchored);
    }

    /// Move the end of the current selection to viewport `end`, keeping
    /// the start where [`Engine::select`] anchored it: a drag that scrolls
    /// the view (or runs while output scrolls) keeps its start on the text
    /// it began on. Does nothing without a selection.
    pub fn extend_selection(&self, end: (usize, usize)) {
        let b = (row_line_id(self.term.lock().grid(), end.0), end.1);
        let mut selection = self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(sel) = selection.as_mut() {
            sel.lo = sel.anchor.min(b);
            sel.hi = sel.anchor.max(b);
        }
    }

    /// Clear the selection (N14b).
    pub fn clear_selection(&self) {
        *self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The current selection in viewport coordinates, `start` before `end`
    /// (N14b), clipped to the viewport: a part above the view starts at
    /// the top-left cell, a part below ends at the bottom-right one.
    /// `None` when nothing is selected or the selection is entirely off
    /// screen (it still exists for [`Engine::copy_selection`]).
    pub fn selection(&self) -> Option<Selection> {
        let sel = (*self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner))?;
        if self.rows == 0 || self.cols == 0 {
            return None;
        }
        let term = self.term.lock();
        if !sel.on_screen(&term) {
            return None;
        }
        let top = row_line_id(term.grid(), 0);
        let bottom = top + self.rows as i64 - 1;
        if sel.hi.0 < top || sel.lo.0 > bottom {
            return None;
        }
        let start = if sel.lo.0 < top {
            (0, 0)
        } else {
            ((sel.lo.0 - top) as usize, sel.lo.1)
        };
        let end = if sel.hi.0 > bottom {
            (self.rows - 1, self.cols - 1)
        } else {
            ((sel.hi.0 - top) as usize, sel.hi.1)
        };
        Some(Selection { start, end })
    }

    /// Copy the selected text (N14b).
    ///
    /// Returns the selected cells' characters joined row-major with `\n`
    /// between rows, or `None` when there is no selection, none of its
    /// lines is still in the grid, or it was made on the other screen
    /// (main or alternate) than the one shown. Cells are read
    /// from the same grid the snapshot renders (`Term::grid`), so the
    /// copied text always matches the on-screen selection.
    pub fn copy_selection(&self) -> Option<String> {
        let sel = (*self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner))?;
        let term = self.term.lock();
        if !sel.on_screen(&term) {
            return None;
        }
        let grid = term.grid();
        let shift = line_shift(grid);
        let cols = grid.columns();
        let topmost = grid.topmost_line().0 as i64;
        let bottommost = grid.bottommost_line().0 as i64;
        let mut out = String::new();
        // Whether any selected line is still in the grid: when none is
        // (cleared with its history, scrolled past the scrollback limit),
        // there is nothing to copy and the clipboard is left alone.
        let mut any = false;
        // A newline is due before the next copied row: the previous one
        // ended a line (not soft-wrapped into the next row).
        let mut newline = false;
        for line_id in sel.lo.0..=sel.hi.0 {
            // Lines that left the history (or a grid that a resize shrank
            // below the range) have nothing to copy.
            let line = line_id - shift;
            if line < topmost || line > bottommost || cols == 0 {
                continue;
            }
            any = true;
            let first = if line_id == sel.lo.0 { sel.lo.1 } else { 0 };
            let last = if line_id == sel.hi.0 {
                sel.hi.1.min(cols - 1)
            } else {
                cols - 1
            };
            let row = &grid[Line(line as i32)];
            if newline {
                out.push('\n');
            }
            let start = out.len();
            for col in first..=last {
                let cell = &row[Column(col)];
                // Skip wide-char spacers: a wide char occupies two cells;
                // the second (spacer) cell carries a placeholder space and
                // must not be duplicated in the copied text.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                out.push(cell.c);
                // Combining marks belong to their base character.
                if let Some(marks) = cell.zerowidth() {
                    out.extend(marks.iter());
                }
            }
            // A row that soft-wraps continues in the next one: no newline
            // and its trailing cells are text. A row that ends a line drops
            // the blanks after its text, as other terminals copy.
            let to_end = last == cols - 1;
            let wrapped = to_end && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            if to_end && !wrapped {
                let text_end = start + out[start..].trim_end_matches(' ').len();
                out.truncate(text_end);
            }
            newline = !wrapped;
        }
        any.then_some(out)
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

        // Per-cell selection state (N14b): a cell is selected iff its
        // anchored `(line id, col)` lies within the current selection range.
        let selected = {
            let sel = self
                .selection
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let mut selected = vec![false; self.rows * self.cols];
            if let Some(sel) = sel.as_ref().filter(|sel| sel.on_screen(&term)) {
                let top = row_line_id(term.grid(), 0);
                for (row, cells) in selected.chunks_mut(self.cols.max(1)).enumerate() {
                    let line_id = top + row as i64;
                    if line_id < sel.lo.0 || line_id > sel.hi.0 {
                        continue;
                    }
                    for (col, cell) in cells.iter_mut().enumerate() {
                        *cell = sel.contains(line_id, col);
                    }
                }
            }
            selected
        };

        // Kitty graphics in the viewport; leaving the alternate screen drops
        // its placements.
        let images = {
            let alt = content.mode.contains(TermMode::ALT_SCREEN);
            let grid = term.grid();
            let history = grid.total_lines().saturating_sub(grid.screen_lines());
            let mut graphics = self.graphics.lock().unwrap_or_else(PoisonError::into_inner);
            if !alt {
                graphics.leave_alternate_screen();
            }
            self.graphics_seen = graphics.version();
            let mut images = graphics.visible(
                grid.total_scrolled(),
                grid.total_pulled(),
                display_offset,
                self.rows,
                history,
                alt,
            );
            images.extend(placeholder_tiles(&cells, &graphics));
            images.sort_by_key(|image| image.z);
            images
        };

        // Increment before assignment: the first snapshot is epoch 1.
        self.snapshot_epoch += 1;

        let snapshot = Snapshot {
            rows: self.rows,
            cols: self.cols,
            epoch: self.snapshot_epoch,
            cells,
            selected,
            cursor,
            cursor_shape: content.cursor.shape,
            modes: content.mode,
            colors: *content.colors,
            damage,
            display_offset,
            images,
        };
        self.cursor_seen = Some(CursorCells::of(&term));
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

        // The cursor in viewport coordinates, the space the damage
        // iterator reports lines in (screen line + display offset, lines
        // pushed off the bottom dropped). Screen coordinates never matched
        // the always-damaged cursor cell while partly scrolled back, so
        // every probe counted as real damage: a redraw loop (audit RV1).
        // Fully off screen, the cursor's damage is dropped too and the
        // empty report is a no-op (`is_noop_damage`).
        let point = term.grid().cursor.point;
        let display_offset = term.grid().display_offset();
        let cursor = point_to_viewport(display_offset, point)
            .filter(|p| p.line < self.rows)
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

/// Unicode placeholder cells (kitty graphics, ADR-014): each shows the tile
/// of a virtual placement given by its marks (row, column, image id high
/// byte), its foreground color (image id) and underline color (placement
/// id). Missing marks continue the cell to the left when it has the same
/// colors: the same row, the next column, the same high byte.
fn placeholder_tiles(
    cells: &[CellText],
    graphics: &crate::graphics_image::GraphicsState,
) -> Vec<crate::graphics_image::VisibleImage> {
    use crate::graphics_image::{PLACEHOLDER, placeholder_mark};
    struct Previous {
        row: usize,
        col: usize,
        fg: Color,
        underline: Option<Color>,
        image_row: u32,
        image_col: u32,
        high: u32,
    }
    let id_of = |color: Color| match color {
        Color::Spec(rgb) => Some(u32::from(rgb.r) << 16 | u32::from(rgb.g) << 8 | u32::from(rgb.b)),
        Color::Indexed(index) => Some(u32::from(index)),
        Color::Named(_) => None,
    };
    let cell = graphics.cell_size();
    let mut tiles = Vec::new();
    let mut previous: Option<Previous> = None;
    for c in cells.iter().filter(|c| c.ch == PLACEHOLDER) {
        let Some(low) = id_of(c.fg) else {
            previous = None;
            continue;
        };
        let marks: Vec<u32> = c
            .zerowidth
            .iter()
            .filter_map(|&m| placeholder_mark(m))
            .collect();
        let left = previous.as_ref().filter(|p| {
            p.row == c.row && p.col + 1 == c.col && p.fg == c.fg && p.underline == c.underline_color
        });
        let (image_row, image_col, high) = match (marks.as_slice(), left) {
            ([], Some(p)) => (p.image_row, p.image_col + 1, p.high),
            ([r], Some(p)) if *r == p.image_row => (*r, p.image_col + 1, p.high),
            ([r, col], Some(p)) if *r == p.image_row && *col == p.image_col + 1 => {
                (*r, *col, p.high)
            }
            ([r, col, high, ..], _) => (*r, *col, *high),
            ([r, col], _) => (*r, *col, 0),
            ([r], _) => (*r, 0, 0),
            ([], _) => (0, 0, 0),
        };
        previous = Some(Previous {
            row: c.row,
            col: c.col,
            fg: c.fg,
            underline: c.underline_color,
            image_row,
            image_col,
            high,
        });
        let id = high << 24 | low;
        let placement = c.underline_color.and_then(id_of).unwrap_or(0);
        if let Some(mut tile) = graphics.placeholder_tile(id, placement, image_row, image_col, cell)
        {
            tile.row = c.row as i32;
            tile.col = c.col as u32;
            tiles.push(tile);
        }
    }
    tiles
}

/// What a frame shows at and next to the cursor that the pinned `Term`'s
/// damage can miss: the cursor's position, visibility, style and pending
/// wrap, and the cells from two columns left of it (where a combining
/// mark lands after a wide character) to the cursor cell.
#[derive(Clone, Debug, PartialEq)]
struct CursorCells {
    point: (i32, usize),
    visible: bool,
    style: alacritty_terminal::vte::ansi::CursorStyle,
    wrap_pending: bool,
    cells: [Option<CursorCell>; 3],
}

/// One cell's visible content for [`CursorCells`].
#[derive(Clone, Debug, PartialEq)]
struct CursorCell {
    ch: char,
    zerowidth: Vec<char>,
    flags: Flags,
    fg: Color,
    bg: Color,
}

impl CursorCells {
    fn of(term: &Term<Recorder>) -> Self {
        let grid = term.grid();
        let cursor = &grid.cursor;
        let line = cursor.point.line;
        let col = cursor.point.column.0;
        let row = &grid[line];
        let cell = |c: Option<usize>| {
            c.filter(|&c| c < grid.columns()).map(|c| {
                let cell = &row[Column(c)];
                CursorCell {
                    ch: cell.c,
                    zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
                    flags: cell.flags,
                    fg: cell.fg,
                    bg: cell.bg,
                }
            })
        };
        Self {
            point: (line.0, col),
            visible: term.mode().contains(TermMode::SHOW_CURSOR),
            style: term.cursor_style(),
            wrap_pending: cursor.input_needs_wrap,
            cells: [
                cell(col.checked_sub(2)),
                cell(col.checked_sub(1)),
                cell(Some(col)),
            ],
        }
    }
}
