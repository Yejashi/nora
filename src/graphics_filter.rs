//! Bounded graphics (kitty-protocol) filter for the terminal engine (N32a).
//!
//! [`GraphicsFilter`] is a byte-level state machine that runs in parallel
//! with the vte parser. It sees the same bytes, extracts graphics APCs
//! (`ESC _ ... ST`), and forwards ALL bytes to the parser (no
//! transformation). The vte parser drops APC bytes correctly, so forwarding
//! all bytes keeps the parser in sync.
//!
//! [`GraphicsQueue`] is a byte-bounded queue of [`GraphicsEvent`]s, evicting
//! the oldest event when the byte cap is exceeded. The cap matches the
//! recorder's 1 MiB byte cap, so large graphics payloads do not evict
//! unrelated events from the recorder.

use std::collections::VecDeque;

/// A parsed kitty-graphics APC payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsEvent {
    /// The command character (e, t, q, c, ...).
    pub command: u8,
    /// The raw params string (the part after the first `;`), unparsed.
    pub params: String,
    /// Base64-decoded image data, if present; empty when the payload has none.
    pub data: Vec<u8>,
}

impl GraphicsEvent {
    /// The byte cost of this event (for queue bounding). The command is 1
    /// byte and is not counted separately.
    fn byte_cost(&self) -> usize {
        self.data.len() + self.params.len()
    }
}

/// The byte cap for the [`GraphicsQueue`] (1 MiB, matching the recorder cap).
const MAX_GRAPHICS_BYTES: usize = 1024 * 1024;

/// A byte-bounded queue of [`GraphicsEvent`]s.
///
/// Evicts the oldest event when the total byte cost exceeds
/// [`MAX_GRAPHICS_BYTES`].
#[derive(Debug, Clone, Default)]
pub struct GraphicsQueue {
    events: VecDeque<GraphicsEvent>,
    total_bytes: usize,
}

impl GraphicsQueue {
    /// Create an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push an event, evicting the oldest event(s) if the total byte cost
    /// exceeds [`MAX_GRAPHICS_BYTES`].
    pub fn push(&mut self, event: GraphicsEvent) {
        self.total_bytes += event.byte_cost();
        self.events.push_back(event);
        while self.total_bytes > MAX_GRAPHICS_BYTES && !self.events.is_empty() {
            if let Some(old) = self.events.pop_front() {
                self.total_bytes -= old.byte_cost();
            }
        }
    }

    /// Drain the queue, returning all events in order (oldest first).
    pub fn drain(&mut self) -> Vec<GraphicsEvent> {
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

/// The filter's state machine states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FilterState {
    /// Ground state: no APC in progress.
    Pass,
    /// Saw an ESC; the next byte may be `_` (start of APC).
    SawEsc,
    /// Inside an APC payload; accumulating bytes.
    Apc,
    /// Saw an ESC inside an APC; the next byte may be `\` (ST terminator).
    ApcEsc,
}

/// A byte-level state machine that extracts graphics APCs in parallel with
/// the vte parser.
#[derive(Debug, Clone)]
pub struct GraphicsFilter {
    state: FilterState,
    buffer: Vec<u8>,
}

impl GraphicsFilter {
    /// Create a new filter in the `Pass` state.
    pub fn new() -> Self {
        Self {
            state: FilterState::Pass,
            buffer: Vec::new(),
        }
    }

    /// Process a chunk of bytes.
    ///
    /// Returns `(bytes_to_forward, events)`. `bytes_to_forward` is the input
    /// slice (all bytes, no transformation). `events` is the list of
    /// [`GraphicsEvent`]s extracted from this chunk.
    pub fn process<'a>(&mut self, bytes: &'a [u8]) -> (&'a [u8], Vec<GraphicsEvent>) {
        let mut events = Vec::new();
        for &byte in bytes {
            match self.state {
                FilterState::Pass => {
                    if byte == 0x1B {
                        self.state = FilterState::SawEsc;
                    }
                }
                FilterState::SawEsc => {
                    if byte == 0x5F {
                        self.state = FilterState::Apc;
                        self.buffer.clear();
                    } else {
                        self.state = FilterState::Pass;
                    }
                }
                FilterState::Apc => {
                    if byte == 0x1B {
                        self.state = FilterState::ApcEsc;
                    } else if byte == 0x9C {
                        if let Some(event) = parse_payload(&self.buffer) {
                            events.push(event);
                        }
                        self.buffer.clear();
                        self.state = FilterState::Pass;
                    } else if byte == 0x18 || byte == 0x1A {
                        self.buffer.clear();
                        self.state = FilterState::Pass;
                    } else {
                        self.buffer.push(byte);
                    }
                }
                FilterState::ApcEsc => {
                    if byte == 0x5C {
                        if let Some(event) = parse_payload(&self.buffer) {
                            events.push(event);
                        }
                        self.buffer.clear();
                        self.state = FilterState::Pass;
                    } else {
                        self.buffer.push(0x1B);
                        self.buffer.push(byte);
                        self.state = FilterState::Apc;
                    }
                }
            }
        }
        (bytes, events)
    }
}

impl Default for GraphicsFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse an APC payload into a [`GraphicsEvent`].
///
/// The payload is split on the first `;` — the part before is the command
/// char, the part after is `params` + optional base64 `data`. The base64
/// `data` is decoded into `data` (empty on failure/absent).
fn parse_payload(buffer: &[u8]) -> Option<GraphicsEvent> {
    // The buffer is the APC payload (after `ESC _`, before the ST
    // terminator). It has the form: <command> ; <params> [ <base64> ]
    // e.g. "e;25;25;0;0;1;100;100;..."
    if buffer.is_empty() {
        return None;
    }
    let command = buffer[0];
    if !command.is_ascii_alphanumeric() {
        return None;
    }
    // Find the first `;` after the command char.
    let rest = &buffer[1..];
    let split = rest.iter().position(|&b| b == b';')?;
    let params = &rest[split + 1..];
    // Everything after the first `;` is the params string (unparsed). The
    // base64 data is the last comma-separated field if it looks like base64.
    // For N32a, we keep the params unparsed and attempt to decode the last
    // field as base64 if it is long enough to be image data.
    let params_str = String::from_utf8_lossy(params).into_owned();
    // Attempt to decode the last comma-separated field as base64.
    let data = decode_trailing_base64(&params_str);
    Some(GraphicsEvent {
        command,
        params: params_str,
        data,
    })
}

/// Attempt to decode the last comma-separated field of `params` as base64.
/// Returns the decoded bytes, or empty if the field is not valid base64 or
/// is too short to be image data (less than 4 chars).
fn decode_trailing_base64(params: &str) -> Vec<u8> {
    // The last comma-separated field is the base64 data (if present).
    let last_field = params.rsplit(';').next().unwrap_or("");
    if last_field.len() < 4 {
        return Vec::new();
    }
    // Validate that the field is valid base64 (A-Z, a-z, 0-9, +, /, =).
    let is_base64 = last_field
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' || c == '-');
    if !is_base64 {
        return Vec::new();
    }
    // Decode base64. We use a simple hand-written decoder to avoid a
    // dependency.
    decode_base64(last_field)
}

/// A simple hand-written base64 decoder (no external dependency).
/// Returns the decoded bytes, or empty on failure.
fn decode_base64(input: &str) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for c in input.chars() {
        if c == '=' {
            break;
        }
        let val = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' | '-' => 62,
            '/' | '_' => 63,
            _ => return Vec::new(),
        };
        buf = (buf << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    output
}
