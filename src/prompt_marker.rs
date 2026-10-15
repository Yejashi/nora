//! Bounded OSC 133 prompt-marker filter, queue, and store (N35a).
//!
//! [`PromptMarkerFilter`] is a byte-level state machine that runs in
//! parallel with the vte parser. It sees the same bytes, extracts OSC 133
//! prompt markers (`ESC ] 133 ; <kind> ST`), and either forwards or
//! suppresses each frame:
//! - A well-formed OSC 133 marker (`133 ; <kind>` with `<kind>` one of
//!   `A`/`B`/`C`/`D`) is parsed into a [`PromptMarkerEvent`] and the frame
//!   is SUPPRESSED (not forwarded to the parser).
//! - Any other OSC frame (a different OSC command, or a malformed 133
//!   frame) is forwarded to the parser unchanged.
//! - All non-OSC bytes are forwarded immediately.
//!
//! When no OSC 133 frames are present, every byte is forwarded unchanged
//! and no events are emitted — normal operation is preserved.
//!
//! [`MarkerQueue`] is a byte-bounded queue of [`PromptMarkerEvent`]s,
//! evicting the oldest event when the byte cap is exceeded (mirroring
//! [`crate::graphics_filter::GraphicsQueue`]). [`MarkerStore`] is a bounded
//! store of detected markers with 1-based IDs and byte-cost accounting
//! (mirroring [`crate::graphics_image::ImageStore`]).

use std::collections::{HashMap, VecDeque};

/// An OSC 133 prompt-marker kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    /// Prompt start.
    A,
    /// Command start (prompt end).
    B,
    /// Output start.
    C,
    /// Output end.
    D,
}

/// A detected OSC 133 prompt marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMarkerEvent {
    /// The marker kind.
    pub kind: MarkerKind,
}

impl PromptMarkerEvent {
    /// The byte cost of this event (for queue/store bounding).
    ///
    /// A marker carries no variable data, so its cost is a fixed 1 byte.
    pub fn byte_cost(&self) -> usize {
        1
    }
}

/// The byte cap for the [`MarkerQueue`] (1 MiB, matching the graphics
/// queue cap).
const MAX_MARKER_BYTES: usize = 1024 * 1024;

/// A prompt marker paired with the grid line it was detected on (N35b).
///
/// `line` is the cursor's grid line (an offset from the bottom, as
/// [`alacritty_terminal::index::Line`]) at the moment the marker was
/// processed. The engine records it by processing the forwarded bytes in
/// segments and reading the cursor after each marker's segment.
///
/// `scrolled` is the grid's monotonic [`total_scrolled`] counter at the
/// moment the marker was processed. Because the grid is a ring buffer whose
/// bottom advances on every full-screen scroll, a marker's line (relative
/// to the bottom) decreases by one for every line added to the bottom
/// after it was stored. Storing `scrolled` lets the engine recompute the
/// marker's current line later as
/// `line - (total_scrolled_now - scrolled) + (total_pulled_now - pulled)`,
/// which stays correct across scrolls (and across the scrollback cap,
/// unlike a raw line count) and across vertical resizes that pull lines
/// from history into the visible area.
///
/// [`total_scrolled`]: alacritty_terminal::grid::Grid::total_scrolled
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerAtLine {
    /// The grid line the marker was detected on.
    pub line: i32,
    /// The grid's `total_scrolled` counter when the marker was detected.
    pub scrolled: u64,
    /// The grid's `total_pulled` counter when the marker was detected.
    pub pulled: u64,
    /// The marker event.
    pub event: PromptMarkerEvent,
}

impl MarkerAtLine {
    /// The byte cost of this entry (for queue/store bounding).
    pub fn byte_cost(&self) -> usize {
        self.event.byte_cost()
    }

    /// The marker's current grid line, given the grid's `total_scrolled`
    /// and `total_pulled` counters now.
    ///
    /// Returns `None` when the marker has scrolled out of the reachable
    /// range (the grid line would underflow), so callers can fall back
    /// cleanly.
    pub fn current_line(&self, total_scrolled_now: u64, total_pulled_now: u64) -> Option<i32> {
        let delta_scrolled = total_scrolled_now.saturating_sub(self.scrolled) as i64;
        let delta_pulled = total_pulled_now.saturating_sub(self.pulled) as i64;
        (self.line as i64 - delta_scrolled + delta_pulled)
            .try_into()
            .ok()
    }
}

/// A byte-bounded queue of [`MarkerAtLine`]s.
///
/// Evicts the oldest entry when the total byte cost exceeds
/// [`MAX_MARKER_BYTES`].
#[derive(Debug, Clone, Default)]
pub struct MarkerQueue {
    events: VecDeque<MarkerAtLine>,
    total_bytes: usize,
}

impl MarkerQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an entry, evicting the oldest entry(ies) if the total byte
    /// cost exceeds [`MAX_MARKER_BYTES`].
    pub fn push(&mut self, entry: MarkerAtLine) {
        self.total_bytes += entry.byte_cost();
        self.events.push_back(entry);
        while self.total_bytes > MAX_MARKER_BYTES && !self.events.is_empty() {
            if let Some(old) = self.events.pop_front() {
                self.total_bytes -= old.byte_cost();
            }
        }
    }

    /// Drain the queue, returning all entries in order (oldest first).
    ///
    /// Resets the byte accounting to zero (the queue is empty afterward).
    pub fn drain(&mut self) -> Vec<MarkerAtLine> {
        self.total_bytes = 0;
        self.events.drain(..).collect()
    }

    /// The current total byte cost of the queue.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// The number of events in the queue.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// The maximum number of stored markers.
pub const MAX_MARKERS: usize = 1024;

/// A bounded store of detected prompt markers.
///
/// Marker IDs are auto-assigned starting at 1 (1-based). The store is
/// bounded by [`MAX_MARKERS`] and tracks the total byte cost of stored
/// markers (byte-cost accounting, mirroring
/// [`crate::graphics_image::ImageStore`]).
#[derive(Debug)]
pub struct MarkerStore {
    markers: HashMap<usize, MarkerAtLine>,
    total_bytes: usize,
    next_id: usize,
}

impl Default for MarkerStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkerStore {
    /// Create an empty store.
    ///
    /// Marker IDs are auto-assigned starting at 1 (1-based).
    pub fn new() -> Self {
        Self {
            markers: HashMap::new(),
            total_bytes: 0,
            next_id: 1,
        }
    }

    /// Store a marker, auto-assigning the next 1-based ID.
    ///
    /// Returns the marker ID on success, or `None` when the
    /// [`MAX_MARKERS`] cap would be exceeded.
    pub fn insert(&mut self, entry: MarkerAtLine) -> Option<usize> {
        if self.markers.len() >= MAX_MARKERS {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.total_bytes += entry.byte_cost();
        self.markers.insert(id, entry);
        Some(id)
    }

    /// The number of stored markers.
    pub fn len(&self) -> usize {
        self.markers.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    /// Query a marker by ID.
    pub fn query(&self, id: usize) -> Option<&MarkerAtLine> {
        self.markers.get(&id)
    }

    /// Iterate over all stored markers as `(id, &MarkerAtLine)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &MarkerAtLine)> + '_ {
        self.markers.iter().map(|(id, entry)| (*id, entry))
    }

    /// Delete a marker by ID, returning it if present.
    pub fn delete(&mut self, id: usize) -> Option<MarkerAtLine> {
        let entry = self.markers.remove(&id)?;
        self.total_bytes -= entry.byte_cost();
        Some(entry)
    }

    /// Remove all markers, returning the number removed.
    pub fn clear(&mut self) -> usize {
        let n = self.markers.len();
        self.total_bytes = 0;
        self.markers.clear();
        n
    }

    /// The total byte cost of all stored markers.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// The filter's state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    /// Ground state: no OSC frame in progress.
    Pass,
    /// Saw an ESC; the next byte may be `]` (start of an OSC frame).
    SawEsc,
    /// Inside an OSC frame; accumulating the data bytes.
    Osc,
    /// Saw an ESC inside an OSC frame; the next byte may be `\` (ST).
    OscEsc,
}

/// A byte-level state machine that extracts OSC 133 prompt markers in
/// parallel with the vte parser.
#[derive(Debug, Clone)]
pub struct PromptMarkerFilter {
    state: FilterState,
    /// The OSC data bytes being accumulated (between `]` and the
    /// terminator).
    buffer: Vec<u8>,
}

impl PromptMarkerFilter {
    /// Create a new filter in the `Pass` state.
    pub fn new() -> Self {
        Self {
            state: FilterState::Pass,
            buffer: Vec::new(),
        }
    }

    /// Process a chunk of bytes.
    ///
    /// Returns `(forwarded, events)`. `forwarded` contains every byte that
    /// should reach the parser: all non-OSC bytes, plus any OSC frame that
    /// is not a well-formed OSC 133 marker. Well-formed OSC 133 marker
    /// frames are suppressed (not forwarded) and instead appear in
    /// `events` as `(position, PromptMarkerEvent)` pairs, where `position`
    /// is the index in `forwarded` at which the marker was detected (i.e.
    /// the number of forwarded bytes before the marker).
    pub fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<(usize, PromptMarkerEvent)>) {
        let mut out: Vec<u8> = Vec::new();
        let mut events: Vec<(usize, PromptMarkerEvent)> = Vec::new();
        for &byte in bytes {
            match self.state {
                FilterState::Pass => {
                    if byte == 0x1B {
                        // Possible OSC start; hold the ESC and decide on the
                        // next byte.
                        self.state = FilterState::SawEsc;
                    } else {
                        out.push(byte);
                    }
                }
                FilterState::SawEsc => {
                    if byte == 0x5D {
                        // `]`: start of an OSC frame. Begin collecting the
                        // data bytes (the `]` itself is not data).
                        self.buffer.clear();
                        self.state = FilterState::Osc;
                    } else {
                        // Not an OSC: forward the held ESC and this byte.
                        out.push(0x1B);
                        out.push(byte);
                        self.state = FilterState::Pass;
                    }
                }
                FilterState::Osc => {
                    if byte == 0x1B {
                        // Possible ST (`ESC \`).
                        self.state = FilterState::OscEsc;
                    } else if byte == 0x9C {
                        // ST (single-byte form).
                        self.complete_frame(&mut out, &mut events, &[0x9C]);
                    } else if byte == 0x07 {
                        // BEL terminator.
                        self.complete_frame(&mut out, &mut events, &[0x07]);
                    } else if byte == 0x18 || byte == 0x1A {
                        // CAN / ABORT: discard the in-progress frame.
                        self.buffer.clear();
                        self.state = FilterState::Pass;
                    } else {
                        self.buffer.push(byte);
                    }
                }
                FilterState::OscEsc => {
                    if byte == 0x5C {
                        // `\`: ST (`ESC \`).
                        self.complete_frame(&mut out, &mut events, &[0x1B, 0x5C]);
                    } else {
                        // Not ST: the ESC is part of the data.
                        self.buffer.push(0x1B);
                        self.buffer.push(byte);
                        self.state = FilterState::Osc;
                    }
                }
            }
        }
        (out, events)
    }

    /// Finish an OSC frame: parse the accumulated data, and either emit a
    /// marker (suppressing the frame) or forward the whole frame.
    ///
    /// `terminator` is the byte sequence that terminated the frame (so it
    /// can be forwarded when the frame is not a marker).
    fn complete_frame(
        &mut self,
        out: &mut Vec<u8>,
        events: &mut Vec<(usize, PromptMarkerEvent)>,
        terminator: &[u8],
    ) {
        let data = std::mem::take(&mut self.buffer);
        self.state = FilterState::Pass;
        match parse_marker(&data) {
            Some(kind) => {
                // Well-formed OSC 133 marker: suppress the frame. Record
                // the position in the forwarded stream (the number of
                // forwarded bytes before this marker).
                events.push((out.len(), PromptMarkerEvent { kind }));
            }
            None => {
                // Not a marker (or malformed): forward the whole frame
                // unchanged (`ESC ] <data> <terminator>`).
                out.push(0x1B);
                out.push(0x5D);
                out.extend_from_slice(&data);
                out.extend_from_slice(terminator);
            }
        }
    }
}

impl Default for PromptMarkerFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse an OSC frame's data (the bytes between `]` and the terminator).
///
/// Returns `Some(kind)` when the frame is exactly `133 ; <kind>` with
/// `<kind>` one of `A`/`B`/`C`/`D`; `None` otherwise (a different OSC
/// command, or a malformed 133 frame). A single optional space around the
/// `;` is tolerated (both `133;A` and `133 ; A` are accepted).
fn parse_marker(data: &[u8]) -> Option<MarkerKind> {
    // Split on the first `;`.
    let semi = data.iter().position(|&b| b == b';')?;
    let head = &data[..semi];
    let rest = &data[semi + 1..];
    // The head must be exactly `133` (tolerating one optional trailing
    // space).
    let head = head.strip_suffix(b" ").unwrap_or(head);
    if head != b"133" {
        return None;
    }
    // The kind must be exactly one of `A`/`B`/`C`/`D` (tolerating one
    // optional leading space).
    let rest = rest.strip_prefix(b" ").unwrap_or(rest);
    match rest {
        b"A" => Some(MarkerKind::A),
        b"B" => Some(MarkerKind::B),
        b"C" => Some(MarkerKind::C),
        b"D" => Some(MarkerKind::D),
        _ => None,
    }
}
