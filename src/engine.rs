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
//! - `Engine` runs with **zero scrollback** (`total_lines == rows`), so grid
//!   lines equal viewport rows and `display_offset` is always 0.
//! - [`Engine::snapshot`] consumes one damage report: the first snapshot of a
//!   fresh engine is [`Damage::Full`] (the engine starts fully damaged); each
//!   later snapshot reports only what changed since the previous snapshot.
//! - [`Engine::feed`] preserves parser state across calls; partial escape
//!   sequences are completed by later feeds.
//! - PTY reply bytes (DSR and similar queries) arrive as
//!   [`EventRecord::PtyWrite`] and are exposed via [`Engine::replies`].

use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{
    ClipboardType, Config, Term, TermDamage, TermMode, point_to_viewport,
};
use alacritty_terminal::vte;
use alacritty_terminal::vte::ansi::{Color, CursorShape};

/// Headless terminal engine with ordered event capture.
pub struct Engine {
    term: Term<Recorder>,
    processor: vte::ansi::Processor<vte::ansi::StdSyncHandler>,
    rows: usize,
    cols: usize,
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
/// formatters; the records keep only the request identity (lossy by design).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventRecord {
    MouseCursorDirty,
    Title(String),
    ResetTitle,
    ClipboardStore(ClipboardType, String),
    ClipboardLoad(ClipboardType),
    ColorRequest(usize),
    PtyWrite(String),
    TextAreaSizeRequest,
    CursorBlinkingChange,
    Wakeup,
    Bell,
    Exit,
    ChildExit(ExitStatus),
}

/// Ordered event sink shared between the engine and its owner.
#[derive(Clone, Default)]
struct Recorder {
    events: Arc<Mutex<Vec<EventRecord>>>,
}

impl EventListener for Recorder {
    fn send_event(&self, event: Event) {
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events.push(to_record(event));
    }
}

fn to_record(event: Event) -> EventRecord {
    match event {
        Event::MouseCursorDirty => EventRecord::MouseCursorDirty,
        Event::Title(title) => EventRecord::Title(title),
        Event::ResetTitle => EventRecord::ResetTitle,
        Event::ClipboardStore(clipboard, text) => EventRecord::ClipboardStore(clipboard, text),
        Event::ClipboardLoad(clipboard, _) => EventRecord::ClipboardLoad(clipboard),
        Event::ColorRequest(index, _) => EventRecord::ColorRequest(index),
        Event::PtyWrite(bytes) => EventRecord::PtyWrite(bytes),
        Event::TextAreaSizeRequest(_) => EventRecord::TextAreaSizeRequest,
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
}

/// One owned screen snapshot with viewport metadata.
#[derive(Clone)]
pub struct Snapshot {
    pub rows: usize,
    pub cols: usize,
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
}

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
            .field("cells", &self.cells)
            .field("cursor", &self.cursor)
            .field("cursor_shape", &self.cursor_shape)
            .field("modes", &self.modes)
            .field("damage", &self.damage)
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
        // Zero history: grid lines equal viewport rows, so display_offset is
        // always 0 and cursor/viewport math is the identity.
        let config = Config {
            scrolling_history: 0,
            ..Default::default()
        };
        let size = Size { rows, cols };
        let recorder = Recorder::default();
        Self {
            term: Term::new(config, &size, recorder.clone()),
            processor: vte::ansi::Processor::<vte::ansi::StdSyncHandler>::new(),
            rows,
            cols,
            recorder,
        }
    }

    /// Viewport height in rows.
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Viewport width in columns.
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Feed raw PTY bytes into the engine, preserving parser state.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.processor.advance(&mut self.term, bytes);
    }

    /// Current terminal modes.
    pub fn modes(&self) -> TermMode {
        *self.term.mode()
    }

    /// Cursor position in viewport coordinates (0-based row, column).
    pub fn cursor(&self) -> Option<(usize, usize)> {
        let point = self.term.grid().cursor.point;
        // Zero scrollback: display_offset is always 0.
        point_to_viewport(0, point).map(|p| (p.line, p.column.0))
    }

    /// Take one owned snapshot of the screen, consuming one damage report.
    pub fn snapshot(&mut self) -> Snapshot {
        let damage = self.term.damage();
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

        let content = self.term.renderable_content();
        let cells: Vec<CellText> = content
            .display_iter
            .map(|cell| {
                let row = cell.point.line.0 as usize;
                CellText {
                    row,
                    col: cell.point.column.0,
                    ch: cell.cell.c,
                    fg: cell.cell.fg,
                    bg: cell.cell.bg,
                    flags: cell.cell.flags,
                }
            })
            .collect();

        let cursor = point_to_viewport(content.display_offset, content.cursor.point)
            .map(|p| (p.line, p.column.0));

        let snapshot = Snapshot {
            rows: self.rows,
            cols: self.cols,
            cells,
            cursor,
            cursor_shape: content.cursor.shape,
            modes: content.mode,
            colors: *content.colors,
            damage,
        };
        self.term.reset_damage();
        snapshot
    }

    /// All events captured so far, in arrival order.
    pub fn events(&self) -> Vec<EventRecord> {
        let events = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        events.clone()
    }

    /// PTY reply bytes (DSR and similar queries) in arrival order.
    pub fn replies(&self) -> Vec<String> {
        let events = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        events
            .iter()
            .filter_map(|event| match event {
                EventRecord::PtyWrite(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .collect()
    }

    /// Drop all captured events and replies.
    pub fn clear_events(&self) {
        let mut events = self
            .recorder
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        events.clear();
    }
}
