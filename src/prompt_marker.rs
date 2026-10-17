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

use std::collections::{BTreeMap, VecDeque};

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

/// A shell-integration event detected by the filter (N35a/N35d/hyperlinks).
///
/// The filter runs in parallel with the vte parser and extracts OSC
/// sequences that carry shell-integration metadata:
/// - [`ShellEvent::Marker`] — an OSC 133 prompt marker.
/// - [`ShellEvent::Cwd`] — an OSC 7 current-directory update (the decoded
///   path).
/// - [`ShellEvent::Hyperlink`] — an OSC 8 hyperlink (the URI).
///
/// Cwd and hyperlink values are metadata, not authorization: they are
/// never used to read files or spawn commands directly (hyperlink
/// activation goes through a validated opener argv, see
/// [`crate::engine::opener_argv`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellEvent {
    /// An OSC 133 prompt marker.
    Marker(PromptMarkerEvent),
    /// An OSC 7 current-directory update (the decoded path).
    Cwd(String),
    /// An OSC 8 hyperlink (the URI).
    Hyperlink(String),
    /// `OSC 6973 ; buffer ; <cursor> ; <base64 UTF-8>`: the shell line
    /// editor's buffer and cursor (ADR-013).
    Buffer(crate::shell_input::LineBuffer),
    /// `OSC 6973 ; end`: the line editor finished (accept or abort).
    BufferEnd,
    /// `OSC 133 ; C ; cmdline=` / `cmdline_url=`: the command line the
    /// shell is executing (decoded). Follows its `C` marker.
    CommandLine(String),
    /// `OSC 133 ; D ; <status>`: the finished command's exit status.
    /// Follows its `D` marker.
    CommandExit(i32),
}

impl ShellEvent {
    /// The byte cost of this event (for queue/store bounding).
    pub fn byte_cost(&self) -> usize {
        match self {
            ShellEvent::Marker(e) => e.byte_cost(),
            ShellEvent::Cwd(s) => s.len(),
            ShellEvent::Hyperlink(s) => s.len(),
            ShellEvent::Buffer(b) => b.text.len(),
            ShellEvent::CommandLine(s) => s.len(),
            ShellEvent::BufferEnd | ShellEvent::CommandExit(_) => 1,
        }
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
    /// Whether the alternate screen was shown. The line and counters are
    /// that screen's grid's (each screen has its own), so the marker is
    /// only aged and looked up on that screen.
    pub alt: bool,
    /// The alternate screen session (`Term::alt_session`) when `alt`; an
    /// entry of an earlier session is absent on a later one (meaningless
    /// on the main screen).
    pub alt_session: u64,
    /// The marker event.
    pub event: PromptMarkerEvent,
}

/// The alternate-screen entries of a queue or store: how many there are
/// and the alternate screen session they share. Inserting an entry
/// retires those of another screen or session first, so they always
/// share one.
#[derive(Debug, Clone, Copy, Default)]
struct AltEntries {
    count: usize,
    session: u64,
}

impl AltEntries {
    /// Count an entry recorded on `screen` (`None` = the main screen).
    fn add(&mut self, screen: Option<u64>) {
        if let Some(session) = screen {
            self.count += 1;
            self.session = session;
        }
    }

    /// Whether the counted entries are absent while `shown` is shown.
    fn stale(&self, shown: Option<u64>) -> bool {
        self.count > 0 && shown != Some(self.session)
    }
}

impl MarkerAtLine {
    /// The screen the marker was recorded on: `None` for the main screen,
    /// else its alternate screen session. It is only present while that
    /// screen is shown.
    pub fn screen(&self) -> Option<u64> {
        self.alt.then_some(self.alt_session)
    }

    /// The byte cost of this entry (for queue/store bounding): its size,
    /// as it owns no heap data.
    pub fn byte_cost(&self) -> usize {
        std::mem::size_of::<Self>()
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
///
/// Alternate-screen entries ([`MarkerAtLine::alt`]) belong to the
/// alternate screen session they were recorded in: a main-screen entry, or
/// one of a later session ([`MarkerAtLine::alt_session`]), pushed after
/// them means that session ended, so they are dropped, as they are by
/// [`MarkerQueue::retire_alternate`].
#[derive(Debug, Clone, Default)]
pub struct MarkerQueue {
    events: VecDeque<MarkerAtLine>,
    total_bytes: usize,
    /// How many of `events` are alternate-screen entries.
    alt: AltEntries,
}

impl MarkerQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an entry, evicting the oldest entry(ies) if the total byte
    /// cost exceeds [`MAX_MARKER_BYTES`]. An entry of another screen or
    /// session drops the queued alternate-screen ones (that session ended).
    pub fn push(&mut self, entry: MarkerAtLine) {
        self.retire_alternate(entry.screen());
        self.total_bytes += entry.byte_cost();
        self.alt.add(entry.screen());
        self.events.push_back(entry);
        while self.total_bytes > MAX_MARKER_BYTES && !self.events.is_empty() {
            if let Some(old) = self.events.pop_front() {
                self.total_bytes -= old.byte_cost();
                self.alt.count -= usize::from(old.alt);
            }
        }
    }

    /// Drain the queue, returning all entries in order (oldest first).
    ///
    /// Resets the byte accounting to zero (the queue is empty afterward).
    pub fn drain(&mut self) -> Vec<MarkerAtLine> {
        self.total_bytes = 0;
        self.alt.count = 0;
        self.events.drain(..).collect()
    }

    /// Drop the alternate-screen entries (that screen's session ended);
    /// returns how many were dropped.
    pub fn drop_alternate(&mut self) -> usize {
        let dropped = std::mem::take(&mut self.alt.count);
        if dropped > 0 {
            let total = &mut self.total_bytes;
            self.events.retain(|e| {
                if e.alt {
                    *total -= e.byte_cost();
                }
                !e.alt
            });
        }
        dropped
    }

    /// Drop the alternate-screen entries unless the shown screen `shown`
    /// ([`MarkerAtLine::screen`]) is their session: on the main screen, or
    /// on a later alternate screen session, they mean nothing. Returns how
    /// many were dropped; free when there are none.
    pub fn retire_alternate(&mut self, shown: Option<u64>) -> usize {
        if self.alt.stale(shown) {
            self.drop_alternate()
        } else {
            0
        }
    }

    /// The queued entries, oldest first, to move their lines (a resize
    /// that reflowed the screen). Their byte costs and screens must not
    /// change.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut MarkerAtLine> + '_ {
        self.events.iter_mut()
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
/// Marker IDs are auto-assigned starting at 1 (1-based) and grow with
/// detection order. The store is bounded by [`MAX_MARKERS`], evicting the
/// oldest marker (the newest prompts are the useful ones), and tracks the
/// total byte cost of stored markers (byte-cost accounting, mirroring
/// [`crate::graphics_image::ImageStore`]). As in [`MarkerQueue`], a
/// marker of another screen or session drops the alternate-screen ones
/// stored before it.
#[derive(Debug)]
pub struct MarkerStore {
    markers: BTreeMap<usize, MarkerAtLine>,
    total_bytes: usize,
    next_id: usize,
    /// How many of `markers` are alternate-screen markers.
    alt: AltEntries,
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
            markers: BTreeMap::new(),
            total_bytes: 0,
            next_id: 1,
            alt: AltEntries::default(),
        }
    }

    /// Store a marker, auto-assigning the next 1-based ID. At the
    /// [`MAX_MARKERS`] cap the oldest marker is evicted first. A
    /// marker of another screen or session drops the stored
    /// alternate-screen ones (that session ended).
    ///
    /// Returns the new marker's ID (always `Some`; the `Option` is kept
    /// for callers of the earlier refusing store).
    pub fn insert(&mut self, entry: MarkerAtLine) -> Option<usize> {
        self.retire_alternate(entry.screen());
        if self.markers.len() >= MAX_MARKERS
            && let Some((_, oldest)) = self.markers.pop_first()
        {
            self.total_bytes -= oldest.byte_cost();
            self.alt.count -= usize::from(oldest.alt);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.total_bytes += entry.byte_cost();
        self.alt.add(entry.screen());
        self.markers.insert(id, entry);
        Some(id)
    }

    /// Drop the alternate-screen markers (that screen's session ended);
    /// returns how many were dropped.
    pub fn drop_alternate(&mut self) -> usize {
        let dropped = std::mem::take(&mut self.alt.count);
        if dropped > 0 {
            let total = &mut self.total_bytes;
            self.markers.retain(|_, e| {
                if e.alt {
                    *total -= e.byte_cost();
                }
                !e.alt
            });
        }
        dropped
    }

    /// Drop the alternate-screen entries unless the shown screen `shown`
    /// ([`MarkerAtLine::screen`]) is their session: on the main screen, or
    /// on a later alternate screen session, they mean nothing. Returns how
    /// many were dropped; free when there are none.
    pub fn retire_alternate(&mut self, shown: Option<u64>) -> usize {
        if self.alt.stale(shown) {
            self.drop_alternate()
        } else {
            0
        }
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

    /// The stored markers, to move their lines (a resize that reflowed
    /// the screen). Their byte costs and screens must not change.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut MarkerAtLine> + '_ {
        self.markers.values_mut()
    }

    /// Delete a marker by ID, returning it if present.
    pub fn delete(&mut self, id: usize) -> Option<MarkerAtLine> {
        let entry = self.markers.remove(&id)?;
        self.total_bytes -= entry.byte_cost();
        self.alt.count -= usize::from(entry.alt);
        Some(entry)
    }

    /// Remove all markers, returning the number removed.
    pub fn clear(&mut self) -> usize {
        let n = self.markers.len();
        self.total_bytes = 0;
        self.alt.count = 0;
        self.markers.clear();
        n
    }

    /// The total byte cost of all stored markers.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

/// A hyperlink paired with the grid line it was detected on (N35d).
///
/// `line`/`scrolled`/`pulled` are recorded exactly as for
/// [`MarkerAtLine`], so the hyperlink's current grid line stays correct
/// across scrolls and vertical resizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperlinkAtLine {
    /// The grid line the hyperlink was detected on.
    pub line: i32,
    /// The grid column the hyperlink started at (stable across vertical
    /// scrolls; used to disambiguate multiple links on one line).
    pub col: usize,
    /// The grid's `total_scrolled` counter when it was detected.
    pub scrolled: u64,
    /// The grid's `total_pulled` counter when it was detected.
    pub pulled: u64,
    /// Whether the alternate screen was shown (see [`MarkerAtLine::alt`]).
    pub alt: bool,
    /// The alternate screen session (see [`MarkerAtLine::alt_session`]).
    pub alt_session: u64,
    /// The hyperlink URI.
    pub uri: String,
}

impl HyperlinkAtLine {
    /// The screen the hyperlink was recorded on (see
    /// [`MarkerAtLine::screen`]).
    pub fn screen(&self) -> Option<u64> {
        self.alt.then_some(self.alt_session)
    }

    /// The byte cost of this entry (for queue/store bounding): the entry
    /// itself plus the URI's heap allocation.
    pub fn byte_cost(&self) -> usize {
        std::mem::size_of::<Self>() + self.uri.capacity()
    }

    /// The hyperlink's current grid line, given the grid's `total_scrolled`
    /// and `total_pulled` counters now. Returns `None` when it has scrolled
    /// out of the reachable range.
    pub fn current_line(&self, total_scrolled_now: u64, total_pulled_now: u64) -> Option<i32> {
        let delta_scrolled = total_scrolled_now.saturating_sub(self.scrolled) as i64;
        let delta_pulled = total_pulled_now.saturating_sub(self.pulled) as i64;
        (self.line as i64 - delta_scrolled + delta_pulled)
            .try_into()
            .ok()
    }
}

/// A byte-bounded queue of [`HyperlinkAtLine`]s (mirrors [`MarkerQueue`]).
#[derive(Debug, Clone, Default)]
pub struct HyperlinkQueue {
    events: VecDeque<HyperlinkAtLine>,
    total_bytes: usize,
    /// How many of `events` are alternate-screen entries.
    alt: AltEntries,
}

impl HyperlinkQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an entry, evicting the oldest entry(ies) if the total byte
    /// cost exceeds [`MAX_MARKER_BYTES`]. An entry of another screen or
    /// session drops the queued alternate-screen ones (that session ended).
    pub fn push(&mut self, entry: HyperlinkAtLine) {
        self.retire_alternate(entry.screen());
        self.total_bytes += entry.byte_cost();
        self.alt.add(entry.screen());
        self.events.push_back(entry);
        while self.total_bytes > MAX_MARKER_BYTES && !self.events.is_empty() {
            if let Some(old) = self.events.pop_front() {
                self.total_bytes -= old.byte_cost();
                self.alt.count -= usize::from(old.alt);
            }
        }
    }

    /// Drain the queue, returning all entries in order (oldest first).
    pub fn drain(&mut self) -> Vec<HyperlinkAtLine> {
        self.total_bytes = 0;
        self.alt.count = 0;
        self.events.drain(..).collect()
    }

    /// Drop the alternate-screen entries (that screen's session ended);
    /// returns how many were dropped.
    pub fn drop_alternate(&mut self) -> usize {
        let dropped = std::mem::take(&mut self.alt.count);
        if dropped > 0 {
            let total = &mut self.total_bytes;
            self.events.retain(|e| {
                if e.alt {
                    *total -= e.byte_cost();
                }
                !e.alt
            });
        }
        dropped
    }

    /// Drop the alternate-screen entries unless the shown screen `shown`
    /// ([`MarkerAtLine::screen`]) is their session: on the main screen, or
    /// on a later alternate screen session, they mean nothing. Returns how
    /// many were dropped; free when there are none.
    pub fn retire_alternate(&mut self, shown: Option<u64>) -> usize {
        if self.alt.stale(shown) {
            self.drop_alternate()
        } else {
            0
        }
    }

    /// The queued entries, oldest first, to move their lines (a resize
    /// that reflowed the screen). Their byte costs and screens must not
    /// change.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut HyperlinkAtLine> + '_ {
        self.events.iter_mut()
    }

    /// Remove and return the entry at `index`, or `None` if out of range.
    pub fn remove_at(&mut self, index: usize) -> Option<HyperlinkAtLine> {
        let len = self.events.len();
        if index >= len {
            return None;
        }
        let mut removed: Option<HyperlinkAtLine> = None;
        let mut kept = VecDeque::with_capacity(len - 1);
        for (i, e) in self.events.drain(..).enumerate() {
            if i == index {
                removed = Some(e);
            } else {
                kept.push_back(e);
            }
        }
        self.events = kept;
        if let Some(entry) = removed {
            self.total_bytes = self.total_bytes.saturating_sub(entry.byte_cost());
            self.alt.count -= usize::from(entry.alt);
            Some(entry)
        } else {
            None
        }
    }

    /// The current total byte cost of the queue.
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// The number of entries in the queue.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Iterate over the entries in order (oldest first), yielding
    /// `(index, &entry)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &HyperlinkAtLine)> + '_ {
        self.events.iter().enumerate()
    }
}

/// A bounded store of detected hyperlinks (mirrors [`MarkerStore`]).
#[derive(Debug)]
pub struct HyperlinkStore {
    links: BTreeMap<usize, HyperlinkAtLine>,
    total_bytes: usize,
    next_id: usize,
    /// How many of `links` are alternate-screen hyperlinks.
    alt: AltEntries,
}

impl Default for HyperlinkStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HyperlinkStore {
    /// Create an empty store (1-based auto-assigned IDs).
    pub fn new() -> Self {
        Self {
            links: BTreeMap::new(),
            total_bytes: 0,
            next_id: 1,
            alt: AltEntries::default(),
        }
    }

    /// Store a hyperlink, auto-assigning the next 1-based ID. At the
    /// [`MAX_MARKERS`] cap the oldest hyperlink is evicted first (the
    /// newest output is where links are clicked). A hyperlink of
    /// another screen or session drops the stored alternate-screen ones
    /// (that session ended).
    ///
    /// Returns the new hyperlink's ID (always `Some`; the `Option` is
    /// kept for callers of the earlier refusing store).
    pub fn insert(&mut self, entry: HyperlinkAtLine) -> Option<usize> {
        self.retire_alternate(entry.screen());
        if self.links.len() >= MAX_MARKERS
            && let Some((_, oldest)) = self.links.pop_first()
        {
            self.total_bytes -= oldest.byte_cost();
            self.alt.count -= usize::from(oldest.alt);
        }
        let id = self.next_id;
        self.next_id += 1;
        self.total_bytes += entry.byte_cost();
        self.alt.add(entry.screen());
        self.links.insert(id, entry);
        Some(id)
    }

    /// Drop the alternate-screen hyperlinks (that screen's session
    /// ended); returns how many were dropped.
    pub fn drop_alternate(&mut self) -> usize {
        let dropped = std::mem::take(&mut self.alt.count);
        if dropped > 0 {
            let total = &mut self.total_bytes;
            self.links.retain(|_, e| {
                if e.alt {
                    *total -= e.byte_cost();
                }
                !e.alt
            });
        }
        dropped
    }

    /// Drop the alternate-screen entries unless the shown screen `shown`
    /// ([`MarkerAtLine::screen`]) is their session: on the main screen, or
    /// on a later alternate screen session, they mean nothing. Returns how
    /// many were dropped; free when there are none.
    pub fn retire_alternate(&mut self, shown: Option<u64>) -> usize {
        if self.alt.stale(shown) {
            self.drop_alternate()
        } else {
            0
        }
    }

    /// The number of stored hyperlinks.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Query a hyperlink by ID.
    pub fn query(&self, id: usize) -> Option<&HyperlinkAtLine> {
        self.links.get(&id)
    }

    /// Iterate over all stored hyperlinks as `(id, &HyperlinkAtLine)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &HyperlinkAtLine)> + '_ {
        self.links.iter().map(|(id, entry)| (*id, entry))
    }

    /// The stored hyperlinks, to move their lines (a resize that
    /// reflowed the screen). Their byte costs and screens must not
    /// change.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut HyperlinkAtLine> + '_ {
        self.links.values_mut()
    }

    /// Delete a hyperlink by ID, returning it if present.
    pub fn delete(&mut self, id: usize) -> Option<HyperlinkAtLine> {
        let entry = self.links.remove(&id)?;
        self.total_bytes -= entry.byte_cost();
        self.alt.count -= usize::from(entry.alt);
        Some(entry)
    }

    /// Remove all hyperlinks, returning the number removed.
    pub fn clear(&mut self) -> usize {
        let n = self.links.len();
        self.total_bytes = 0;
        self.alt.count = 0;
        self.links.clear();
        n
    }

    /// The total byte cost of all stored hyperlinks.
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

/// Largest OSC frame passed to the parser (64 KiB). Larger frames are
/// discarded whole (audit P3): without a cap, `printf '\e]0;'` followed by
/// endless output grew this buffer and vte's own OSC buffer without bound.
pub const MAX_OSC_BYTES: usize = 64 * 1024;

/// A byte-level state machine that extracts OSC 133 prompt markers in
/// parallel with the vte parser.
#[derive(Debug, Clone)]
pub struct PromptMarkerFilter {
    state: FilterState,
    /// The current frame outgrew [`MAX_OSC_BYTES`]: it is dropped whole
    /// at its terminator instead of being forwarded.
    overflow: bool,
    /// The OSC data bytes being accumulated (between `]` and the
    /// terminator).
    buffer: Vec<u8>,
}

impl PromptMarkerFilter {
    /// Create a new filter in the `Pass` state.
    pub fn new() -> Self {
        Self {
            state: FilterState::Pass,
            overflow: false,
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
    pub fn process(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<(usize, ShellEvent)>) {
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
        let mut events: Vec<(usize, ShellEvent)> = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            // Fast path: plain text up to the next ESC is copied as one run
            // (it is almost all of a typical stream).
            if self.state == FilterState::Pass {
                let run = bytes[i..]
                    .iter()
                    .position(|&b| b == 0x1B)
                    .unwrap_or(bytes.len() - i);
                out.extend_from_slice(&bytes[i..i + run]);
                i += run;
                if i == bytes.len() {
                    break;
                }
            }
            let byte = bytes[i];
            i += 1;
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
                    } else if byte == 0x1B {
                        // vte restarts the escape at a second ESC, so the
                        // first one has no effect: drop it, hold the new
                        // one (forwarding it could pair it with bytes after
                        // a suppressed frame).
                    } else if matches!(byte, 0x00..=0x17 | 0x19 | 0x1C..=0x1F) {
                        // vte executes a C0 control inside an escape and
                        // stays in it. Forwarding the control before the
                        // held ESC is not equivalent: this filter does not
                        // track DCS/SOS/PM/APC strings, inside which vte
                        // ignores the control, while after the ESC (which
                        // ends such a string) it executes it. So the ESC
                        // and the control go out in their order and the
                        // filter returns to ground; the rare
                        // `ESC <C0> ] 133 …` is then parsed by vte and not
                        // recognised as a marker.
                        out.push(0x1B);
                        out.push(byte);
                        self.state = FilterState::Pass;
                    } else if byte >= 0x7F {
                        // vte ignores DEL and 8-bit bytes inside an escape
                        // and stays in it: drop the byte, keep the ESC.
                    } else {
                        // Not an OSC: forward the held ESC and this byte.
                        out.push(0x1B);
                        out.push(byte);
                        self.state = FilterState::Pass;
                    }
                }
                // The terminators match vte's `advance_osc_string` (UTF-8
                // mode): BEL ends the frame; CAN/SUB end it and are then
                // executed; any ESC ends it and starts the next sequence;
                // C1 bytes such as 0x9C are data (UTF-8 continuation bytes);
                // the C0 controls vte ignores are not data.
                FilterState::Osc => match byte {
                    0x1B => self.state = FilterState::OscEsc,
                    0x07 => {
                        self.complete_frame(&mut out, &mut events, &[0x07]);
                    }
                    0x18 | 0x1A => {
                        if !self.complete_frame(&mut out, &mut events, &[byte]) {
                            out.push(byte);
                        }
                    }
                    0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1C..=0x1F => {}
                    _ => self.hold(&[byte]),
                },
                FilterState::OscEsc => {
                    if byte == 0x5C {
                        // `\`: ST (`ESC \`).
                        self.complete_frame(&mut out, &mut events, &[0x1B, 0x5C]);
                    } else {
                        // vte ends the frame at the ESC, which begins the
                        // next sequence. A forwarded frame gets an explicit
                        // ST (the parser dispatches it the same way, and a
                        // suppressed frame after it cannot leave the parser
                        // inside this one); the ESC is then held as in
                        // ground and this byte reprocessed after it.
                        self.complete_frame(&mut out, &mut events, &[0x1B, 0x5C]);
                        self.state = FilterState::SawEsc;
                        i -= 1;
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
    /// Buffer frame bytes up to [`MAX_OSC_BYTES`]; past it, mark the frame
    /// overflowed and stop growing.
    fn hold(&mut self, bytes: &[u8]) {
        if self.overflow {
            return;
        }
        if self.buffer.len() + bytes.len() > MAX_OSC_BYTES {
            self.overflow = true;
            self.buffer = Vec::new();
        } else {
            self.buffer.extend_from_slice(bytes);
        }
    }

    /// Returns whether the frame (with `terminator`) was forwarded.
    fn complete_frame(
        &mut self,
        out: &mut Vec<u8>,
        events: &mut Vec<(usize, ShellEvent)>,
        terminator: &[u8],
    ) -> bool {
        let data = std::mem::take(&mut self.buffer);
        self.state = FilterState::Pass;
        if std::mem::take(&mut self.overflow) {
            // An overlong frame is dropped whole: neither this filter nor
            // the parser behind it ever buffers more than the cap.
            return false;
        }
        match classify_frame(&data) {
            FrameAction::Marker(kind, extra) => {
                // Well-formed OSC 133 marker: suppress the frame. Record
                // the position in the forwarded stream (the number of
                // forwarded bytes before this marker); a command line or
                // exit status it carries follows at the same position.
                events.push((out.len(), ShellEvent::Marker(PromptMarkerEvent { kind })));
                if let Some(extra) = extra {
                    events.push((out.len(), extra));
                }
                false
            }
            FrameAction::Shell(event) => {
                // OSC 6973 buffer report: metadata, suppressed.
                events.push((out.len(), event));
                false
            }
            FrameAction::Cwd(path) => {
                // OSC 7 cwd update: suppress the frame (metadata, no
                // display effect) and record the position.
                events.push((out.len(), ShellEvent::Cwd(path)));
                false
            }
            FrameAction::Hyperlink(uri) => {
                // OSC 8 hyperlink: forward the frame (it has a display
                // effect — link styling — handled by the parser) and
                // record the position at the frame's start.
                let pos = out.len();
                events.push((pos, ShellEvent::Hyperlink(uri)));
                out.push(0x1B);
                out.push(0x5D);
                out.extend_from_slice(&data);
                out.extend_from_slice(terminator);
                true
            }
            FrameAction::Forward => {
                // Not a recognized shell-integration frame (or
                // malformed): forward the whole frame unchanged
                // (`ESC ] <data> <terminator>`).
                out.push(0x1B);
                out.push(0x5D);
                out.extend_from_slice(&data);
                out.extend_from_slice(terminator);
                true
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
/// Returns `Some(kind)` when the frame is `133 ; <kind>` with `<kind>`
/// one of `A`/`B`/`C`/`D`, optionally followed by `;`-separated
/// parameters (returned too); `None` otherwise (a different OSC command,
/// or a malformed 133 frame). A single optional space around the first
/// `;` is tolerated (both `133;A` and `133 ; A` are accepted).
fn parse_marker(data: &[u8]) -> Option<(MarkerKind, &[u8])> {
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
    let (kind, params) = match rest.iter().position(|&b| b == b';') {
        Some(p) => (&rest[..p], &rest[p + 1..]),
        None => (rest, &rest[rest.len()..]),
    };
    let kind = match kind {
        b"A" => MarkerKind::A,
        b"B" => MarkerKind::B,
        b"C" => MarkerKind::C,
        b"D" => MarkerKind::D,
        _ => return None,
    };
    Some((kind, params))
}

/// The event a marker's parameters carry: the command line on `C`
/// (`cmdline=` shell-quoted, or `cmdline_url=` percent-encoded, as kitty
/// defines them) and the exit status on `D`.
fn marker_extra(kind: MarkerKind, params: &[u8]) -> Option<ShellEvent> {
    let params = std::str::from_utf8(params).ok()?;
    match kind {
        MarkerKind::C => params.split(';').find_map(|param| {
            if let Some(url) = param.strip_prefix("cmdline_url=") {
                Some(ShellEvent::CommandLine(percent_decode(url)))
            } else {
                param
                    .strip_prefix("cmdline=")
                    .and_then(crate::shell_input::unquote_shell)
                    .map(ShellEvent::CommandLine)
            }
        }),
        MarkerKind::D => params
            .split(';')
            .next()?
            .trim()
            .parse::<i32>()
            .ok()
            .map(ShellEvent::CommandExit),
        MarkerKind::A | MarkerKind::B => None,
    }
}

/// Parse an `OSC 6973` buffer report's data (after `6973;`): `buffer ;
/// <cursor> ; <base64 UTF-8 text>` or `end`.
fn parse_buffer_report(rest: &[u8]) -> Option<ShellEvent> {
    use base64::Engine as _;
    if rest == b"end" {
        return Some(ShellEvent::BufferEnd);
    }
    let rest = rest.strip_prefix(b"buffer;")?;
    let semi = rest.iter().position(|&b| b == b';')?;
    let cursor = std::str::from_utf8(&rest[..semi])
        .ok()?
        .parse::<usize>()
        .ok()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&rest[semi + 1..])
        .ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(ShellEvent::Buffer(crate::shell_input::LineBuffer {
        text,
        cursor,
    }))
}

/// What to do with a completed OSC frame.
#[derive(Debug, Clone)]
enum FrameAction {
    /// A well-formed OSC 133 marker: suppress and emit a marker event,
    /// then the command line or exit status it carries.
    Marker(MarkerKind, Option<ShellEvent>),
    /// An OSC 6973 buffer report: suppress and emit it.
    Shell(ShellEvent),
    /// An OSC 7 cwd update: suppress and emit a cwd event.
    Cwd(String),
    /// An OSC 8 hyperlink: forward and emit a hyperlink event.
    Hyperlink(String),
    /// Forward the frame unchanged (no event).
    Forward,
}

/// Classify a completed OSC frame's data (the bytes between `]` and the
/// terminator).
///
/// Returns the action to take:
/// - OSC 133 with a valid kind → [`FrameAction::Marker`].
/// - OSC 7 with a valid `file://` local path → [`FrameAction::Cwd`].
/// - OSC 8 with a non-empty URI → [`FrameAction::Hyperlink`].
/// - Anything else → [`FrameAction::Forward`] (forward unchanged).
fn classify_frame(data: &[u8]) -> FrameAction {
    // Split on the first `;`.
    let semi = match data.iter().position(|&b| b == b';') {
        Some(p) => p,
        None => return FrameAction::Forward,
    };
    let head = &data[..semi];
    let rest = &data[semi + 1..];
    let head = head.strip_suffix(b" ").unwrap_or(head);
    match std::str::from_utf8(head) {
        Ok("133") => match parse_marker(data) {
            Some((kind, params)) => FrameAction::Marker(kind, marker_extra(kind, params)),
            None => FrameAction::Forward,
        },
        Ok("6973") => match parse_buffer_report(rest) {
            Some(event) => FrameAction::Shell(event),
            None => FrameAction::Forward,
        },
        Ok("7") => match parse_cwd(rest) {
            Some(path) => FrameAction::Cwd(path),
            None => FrameAction::Forward,
        },
        Ok("8") => match parse_hyperlink(rest) {
            Some(uri) => FrameAction::Hyperlink(uri),
            None => FrameAction::Forward,
        },
        _ => FrameAction::Forward,
    }
}

/// Parse an OSC 7 cwd frame's data (the bytes after `7;`).
///
/// Accepts either a `file://` URI for a local path (host empty,
/// `localhost` or this machine's hostname, path absolute, percent-decoded)
/// or a bare absolute path. A path on another host (a shell over ssh) is
/// not a local cwd: new tabs must not `chdir` to it. Returns the path, or `None` when the frame is not a valid local path.
fn parse_cwd(data: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(data).ok()?;
    if let Some(rest) = s.strip_prefix("file://") {
        // `rest` is `host/path`. Split on the first `/`.
        let slash = rest.find('/')?;
        let host = &rest[..slash];
        let path = &rest[slash..];
        if !is_local_host(host) {
            return None;
        }
        // The path must be absolute.
        if !path.starts_with('/') {
            return None;
        }
        Some(percent_decode(path))
    } else {
        // A bare path (no `file://` scheme). Must be absolute.
        if !s.starts_with('/') {
            return None;
        }
        Some(s.to_string())
    }
}

/// Whether an OSC 7 host names this machine: empty, `localhost`, or the
/// hostname (case-insensitively; a short name matches the full name).
fn is_local_host(host: &str) -> bool {
    static HOSTNAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    host_matches(host, HOSTNAME.get_or_init(local_hostname))
}

fn host_matches(host: &str, local: &str) -> bool {
    let short = |name: &str| name.split('.').next().unwrap_or(name).to_ascii_lowercase();
    host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || (!local.is_empty() && short(host) == short(local))
}

/// This machine's hostname, or empty when it cannot be read.
fn local_hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its whole length, which is the length
    // passed; the name is cut at the first NUL (or the buffer end).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// Parse an OSC 8 hyperlink frame's data (the bytes after `8;`), which is
/// `params;URI`.
///
/// The params are `;`-separated `key=value` pairs (ignored); the URI is
/// the last `;`-separated field. Returns the URI, or `None` when it is
/// empty. (Senders percent-encode any `;` inside the URI, so the last
/// field is unambiguously the URI.)
fn parse_hyperlink(data: &[u8]) -> Option<String> {
    let fields: Vec<&[u8]> = data.split(|&b| b == b';').collect();
    let uri_field = fields.last()?;
    if uri_field.is_empty() {
        return None;
    }
    let uri = std::str::from_utf8(uri_field).ok()?;
    if uri.is_empty() {
        return None;
    }
    Some(uri.to_string())
}

/// Percent-decode a string (`%XX` → byte `XX`). Non-`%` bytes are copied
/// through; a malformed escape (not followed by two hex digits) is copied
/// as the literal `%`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // Decode on bytes: slicing the `str` could split a multi-byte
        // character and panic (audit RV-OSC7).
        let decoded = match (bytes[i], bytes.get(i + 1), bytes.get(i + 2)) {
            (b'%', Some(&hi), Some(&lo)) => hex_value(hi).zip(hex_value(lo)),
            _ => None,
        }
        .map(|(hi, lo)| hi << 4 | lo);
        if let Some(v) = decoded {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|v| v as u8)
}

#[cfg(test)]
mod osc_cap_tests {
    use super::*;

    /// Audit RV-OSC7: a `%` before a multi-byte character used to slice
    /// the `str` mid-character and panic the PTY reader; a trailing `%XX`
    /// was never decoded.
    /// zsh reports `file://$HOST/path`: this machine's name is local (in
    /// any case, short or full), another host is not.
    #[test]
    fn osc7_host_is_local_only_for_this_machine() {
        assert!(host_matches("", "box"));
        assert!(host_matches("LocalHost", "box"));
        assert!(host_matches("Box", "box.example.org"));
        assert!(host_matches("box.example.org", "box"));
        assert!(!host_matches("server", "box"));
        assert!(!host_matches("server", ""));
        let local = local_hostname();
        let uri = format!("file://{local}/tmp");
        assert_eq!(parse_cwd(uri.as_bytes()).as_deref(), Some("/tmp"));
        assert_eq!(parse_cwd(b"file://nora-remote-host.invalid/tmp"), None);
    }

    #[test]
    fn percent_decode_is_byte_safe_and_decodes_the_last_escape() {
        assert_eq!(percent_decode("/%a\u{e9}"), "/%a\u{e9}");
        assert_eq!(percent_decode("/%\u{e9}x"), "/%\u{e9}x");
        assert_eq!(percent_decode("/a%20b%2F"), "/a b/");
        assert_eq!(percent_decode("/x%2"), "/x%2");
        assert_eq!(percent_decode("/x%"), "/x%");
        assert_eq!(percent_decode("/caf%C3%A9"), "/caf\u{e9}");
    }

    /// Audit P3: an unterminated OSC followed by endless output keeps the
    /// held frame at or below the cap, the overlong frame is never
    /// forwarded, and output after its terminator flows normally.
    #[test]
    fn unterminated_osc_is_bounded_and_dropped() {
        let mut filter = PromptMarkerFilter::new();
        let (out, _) = filter.process(b"before\x1b]0;");
        assert_eq!(out, b"before");
        let chunk = vec![b'a'; 1 << 20];
        for _ in 0..100 {
            let (out, _) = filter.process(&chunk);
            assert!(out.is_empty(), "the held frame must not leak out");
            assert!(filter.buffer.len() <= MAX_OSC_BYTES);
        }
        let (out, _) = filter.process(b"\x07after");
        assert_eq!(out, b"after", "the overlong frame is dropped whole");
        let (out, _) = filter.process(b"\x1b]2;title\x07");
        assert_eq!(out, b"\x1b]2;title\x07", "the next frame is forwarded");
    }

    #[test]
    fn frame_at_the_cap_is_forwarded() {
        let mut filter = PromptMarkerFilter::new();
        let mut frame = b"\x1b]2;".to_vec();
        frame.extend(std::iter::repeat_n(b'x', MAX_OSC_BYTES - 2));
        frame.push(0x07);
        let (out, _) = filter.process(&frame);
        assert_eq!(out, frame);
    }
}
