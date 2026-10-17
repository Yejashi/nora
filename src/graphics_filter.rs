//! Bounded graphics (kitty-protocol) filter for the terminal engine (N32a).
//!
//! [`GraphicsFilter`] is a byte-level state machine that runs in parallel
//! with the vte parser. It sees the same bytes, extracts graphics APCs
//! (`ESC _ ... ST`), and forwards ALL bytes to the parser (no
//! transformation). The vte parser drops APC bytes correctly, so forwarding
//! all bytes keeps the parser in sync.
//!
//! The extracted APC payload is parsed into the real kitty-graphics wire
//! format: a `G` command character, a comma-separated `key=value` control
//! list, and an optional `;`-separated base64 payload (N34, per ADR-011).
//!
//! [`GraphicsQueue`] is a byte-bounded queue of [`GraphicsEvent`]s, evicting
//! the oldest event when the byte cap is exceeded. The cap matches the
//! recorder's 1 MiB byte cap, so large graphics payloads do not evict
//! unrelated events from the recorder.

use std::collections::VecDeque;

/// A parsed kitty-graphics APC payload (the real `G` + `key=value` protocol).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsEvent {
    /// The command character (`G` in the real protocol).
    pub command: u8,
    /// The parsed control keys (comma-separated `key=value` pairs, in order).
    pub keys: Vec<(String, String)>,
    /// Base64-decoded payload (empty when the APC has none).
    pub data: Vec<u8>,
}

impl GraphicsEvent {
    /// Look up a control key by name (case-sensitive), returning its value.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.keys
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Look up a control key as a parsed `u32`.
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key).and_then(|v| v.parse::<u32>().ok())
    }

    /// Look up a control key as a parsed `i32`.
    pub fn get_i32(&self, key: &str) -> Option<i32> {
        self.get(key).and_then(|v| v.parse::<i32>().ok())
    }

    /// The byte cost of this event (for queue bounding).
    fn byte_cost(&self) -> usize {
        self.data.len()
            + self
                .keys
                .iter()
                .map(|(k, v)| k.len() + v.len() + 2)
                .sum::<usize>()
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
    /// Saw an ESC; the next byte may be `_` (start of APC), `[` (CSI) or
    /// `c` (RIS).
    SawEsc,
    /// Inside a CSI sequence, watching for `CSI 2 J` / `CSI 3 J`.
    Csi,
    /// Inside an APC payload; accumulating bytes.
    Apc,
    /// Saw an ESC inside an APC; `\` (ST) ends the APC, any other byte
    /// abandons it and starts a new sequence after this ESC (as vte does).
    ApcEsc,
}

/// Largest single APC frame inspected for graphics (4 MiB; the kitty
/// protocol recommends 4096-byte chunks). Larger frames are ignored, and
/// the buffer never grows past the cap (audit P3).
pub const MAX_APC_BYTES: usize = 4 * 1024 * 1024;

/// What the filter found in the stream, for the graphics state (ADR-014).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicsSignal {
    /// A kitty graphics command.
    Command(GraphicsEvent),
    /// `CSI 2 J`: the screen was erased.
    ClearScreen,
    /// `CSI 3 J`: the scrollback was erased.
    ClearScrollback,
    /// `ESC c`: the terminal was reset.
    Reset,
}

/// A byte-level state machine that extracts graphics APCs (and the
/// screen clears that remove images) in parallel with the vte parser.
#[derive(Debug, Clone)]
pub struct GraphicsFilter {
    state: FilterState,
    buffer: Vec<u8>,
    /// The current APC outgrew [`MAX_APC_BYTES`]; it is ignored at its
    /// terminator.
    overflow: bool,
    /// The CSI parameter bytes so far (a few are enough for `2J`/`3J`).
    csi: Vec<u8>,
}

impl GraphicsFilter {
    /// Create a new filter in the `Pass` state.
    pub fn new() -> Self {
        Self {
            state: FilterState::Pass,
            buffer: Vec::new(),
            overflow: false,
            csi: Vec::new(),
        }
    }

    /// Buffer APC bytes up to [`MAX_APC_BYTES`]; past it, mark the frame
    /// overflowed and release the buffer.
    fn hold(&mut self, bytes: &[u8]) {
        if self.overflow {
            return;
        }
        if self.buffer.len() + bytes.len() > MAX_APC_BYTES {
            self.overflow = true;
            self.buffer = Vec::new();
        } else {
            self.buffer.extend_from_slice(bytes);
        }
    }

    /// The APC just terminated: its graphics command, if it is one.
    fn finish_apc(&mut self) -> Option<GraphicsSignal> {
        let overflow = std::mem::take(&mut self.overflow);
        let event = (!overflow)
            .then(|| parse_payload(&self.buffer))
            .flatten()
            // Only 'G' is the graphics protocol; other APC commands (e.g.
            // titles) are not graphics events.
            .filter(|event| event.command == b'G');
        self.buffer.clear();
        self.state = FilterState::Pass;
        event.map(GraphicsSignal::Command)
    }

    /// Process a chunk of bytes.
    ///
    /// Returns `(bytes_to_forward, signals)`. `bytes_to_forward` is the
    /// input slice (all bytes, no transformation). Each signal comes with
    /// the offset just past the sequence that produced it, so the caller
    /// can act on it at that point of the stream.
    pub fn process<'a>(&mut self, bytes: &'a [u8]) -> (&'a [u8], Vec<(usize, GraphicsSignal)>) {
        let mut signals = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            // Fast path: skip plain text to the next ESC.
            if self.state == FilterState::Pass {
                match bytes[i..].iter().position(|&b| b == 0x1B) {
                    Some(run) => i += run,
                    None => break,
                }
            }
            let byte = bytes[i];
            i += 1;
            match self.state {
                FilterState::Pass => {
                    if byte == 0x1B {
                        self.state = FilterState::SawEsc;
                    }
                }
                FilterState::SawEsc => match byte {
                    0x5F => {
                        self.state = FilterState::Apc;
                        self.buffer.clear();
                    }
                    b'[' => {
                        self.state = FilterState::Csi;
                        self.csi.clear();
                    }
                    b'c' => {
                        signals.push((i, GraphicsSignal::Reset));
                        self.state = FilterState::Pass;
                    }
                    0x1B => {}
                    _ => self.state = FilterState::Pass,
                },
                FilterState::Csi => match byte {
                    0x30..=0x3F => {
                        if self.csi.len() < 8 {
                            self.csi.push(byte);
                        }
                    }
                    0x20..=0x2F => self.csi.push(b' '),
                    0x40..=0x7E => {
                        if byte == b'J' {
                            match self.csi.as_slice() {
                                b"2" => signals.push((i, GraphicsSignal::ClearScreen)),
                                b"3" => signals.push((i, GraphicsSignal::ClearScrollback)),
                                _ => {}
                            }
                        }
                        self.state = FilterState::Pass;
                    }
                    0x1B => self.state = FilterState::SawEsc,
                    // C0 controls execute inside a CSI without ending it.
                    0x00..=0x1F => {}
                    _ => self.state = FilterState::Pass,
                },
                FilterState::Apc => {
                    if byte == 0x1B {
                        self.state = FilterState::ApcEsc;
                    } else if byte == 0x9C {
                        if let Some(signal) = self.finish_apc() {
                            signals.push((i, signal));
                        }
                    } else if byte == 0x18 || byte == 0x1A {
                        self.buffer.clear();
                        self.overflow = false;
                        self.state = FilterState::Pass;
                    } else {
                        self.hold(&[byte]);
                    }
                }
                FilterState::ApcEsc => {
                    if byte == 0x5C {
                        if let Some(signal) = self.finish_apc() {
                            signals.push((i, signal));
                        }
                    } else {
                        // vte ends an APC at any ESC: abandon this one and
                        // read the byte as following a fresh ESC.
                        self.buffer.clear();
                        self.overflow = false;
                        self.state = FilterState::SawEsc;
                        i -= 1;
                    }
                }
            }
        }
        (bytes, signals)
    }
}

impl Default for GraphicsFilter {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse an APC payload into a [`GraphicsEvent`].
///
/// The payload has the form `G<control data>[;<base64 payload>]`, where the
/// control data is a comma-separated list of `key=value` pairs (the real
/// kitty-graphics protocol, per ADR-011). The optional base64 payload follows
/// the first `;`. The base64 alphabet contains no `;`, so the first `;` is
/// always the control/payload separator. Examples:
/// - `Gf=24,s=10,v=20;<payload>` — transmit 24-bit RGB, 10x20.
/// - `Gm=1;<chunk>` — first chunk (not the last).
/// - `Ga=d,d=i,i=10` — delete image 10 (no payload).
pub fn parse_payload(buffer: &[u8]) -> Option<GraphicsEvent> {
    if buffer.is_empty() {
        return None;
    }
    let command = buffer[0];
    if !command.is_ascii_alphanumeric() {
        return None;
    }
    let rest = &buffer[1..];
    // Split on the first `;`: control data before it, base64 payload after.
    let (control, payload_b64) = match rest.iter().position(|&b| b == b';') {
        Some(pos) => (&rest[..pos], &rest[pos + 1..]),
        None => (rest, &b""[..]),
    };
    let keys = parse_control_data(control)?;
    let data = if payload_b64.is_empty() {
        Vec::new()
    } else {
        decode_base64_bytes(payload_b64)?
    };
    Some(GraphicsEvent {
        command,
        keys,
        data,
    })
}

/// Parse comma-separated `key=value` control data into an ordered list of
/// `(key, value)` pairs. Pairs without an `=` are skipped (lenient); an empty
/// control string yields an empty list.
fn parse_control_data(control: &[u8]) -> Option<Vec<(String, String)>> {
    let control_str = std::str::from_utf8(control).ok()?;
    let mut keys = Vec::new();
    for pair in control_str.split(',') {
        if pair.is_empty() {
            continue;
        }
        if let Some(eq) = pair.find('=') {
            let key = &pair[..eq];
            let value = &pair[eq + 1..];
            keys.push((key.to_string(), value.to_string()));
        }
    }
    Some(keys)
}

/// A hand-written base64 decoder over bytes (no external dependency).
/// Returns the decoded bytes, or `None` on invalid input.
fn decode_base64_bytes(input: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut buf: u32 = 0;
    let mut bits = 0;
    for &b in input {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(output)
}

#[cfg(test)]
mod apc_cap_tests {
    use super::*;

    /// Audit P3: an endless APC keeps the buffer at or below the cap and
    /// yields no graphics event.
    #[test]
    fn unterminated_apc_is_bounded() {
        let mut filter = GraphicsFilter::new();
        let _ = filter.process(b"\x1b_Ga=t;");
        let chunk = vec![b'A'; 1 << 20];
        for _ in 0..100 {
            let (_, events) = filter.process(&chunk);
            assert!(events.is_empty());
            assert!(filter.buffer.len() <= MAX_APC_BYTES);
        }
        let (_, events) = filter.process(b"\x1b\\");
        assert!(events.is_empty(), "an overlong APC is ignored");
        let (_, events) = filter.process(b"\x1b_Ga=q,i=1;AAAA\x1b\\");
        assert_eq!(events.len(), 1, "the next APC is parsed again");
    }
}
