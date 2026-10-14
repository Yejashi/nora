//! N17a: pure paste policy.
//!
//! Paste validation (size cap, bracketed paste markers, newline
//! normalization) and a bounded paste stream. No winit, no PTY, no
//! terminal state — headlessly testable. The pump wiring (N17b) validates
//! clipboard text with [`validate_paste`], then sends the bytes through
//! [`PasteStream`] in order; the bracket markers are already part of the
//! byte sequence, so the stream itself is bracket-stateless.

/// The maximum paste size in bytes. Pastes larger than this are rejected.
pub const MAX_PASTE_SIZE: usize = 1024 * 1024; // 1 MiB

/// Bracketed paste start marker: `CSI 200 ~`.
const BRACKET_START: &[u8] = b"\x1b[200~";
/// Bracketed paste end marker: `CSI 201 ~`.
const BRACKET_END: &[u8] = b"\x1b[201~";

/// Result of validating a paste before sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteValidation {
    /// The paste is within bounds and can be sent.
    Ok {
        /// The paste bytes, with bracketed paste markers added if needed.
        bytes: Vec<u8>,
        /// Whether bracketed paste mode was active.
        bracketed: bool,
    },
    /// The paste exceeds the maximum size.
    TooLarge {
        /// The actual size in bytes (including markers when bracketed).
        size: usize,
        /// The maximum allowed size.
        max: usize,
    },
}

/// Validate a paste and apply bracketed paste markers if needed.
///
/// Newlines are normalized first (`\r\n` → `\n`, standalone `\r` → `\n`,
/// `\n` unchanged), then — if `bracketed` — the normalized bytes are
/// wrapped in `CSI 200 ~ ... CSI 201 ~`. The final size (including
/// markers when bracketed) is compared against [`MAX_PASTE_SIZE`]:
/// strictly larger is [`PasteValidation::TooLarge`], exactly equal is
/// [`PasteValidation::Ok`].
pub fn validate_paste(paste: &str, bracketed: bool) -> PasteValidation {
    let normalized = normalize_newlines(paste);
    let total = if bracketed {
        BRACKET_START.len() + normalized.len() + BRACKET_END.len()
    } else {
        normalized.len()
    };
    if total > MAX_PASTE_SIZE {
        return PasteValidation::TooLarge {
            size: total,
            max: MAX_PASTE_SIZE,
        };
    }
    let bytes = if bracketed {
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(BRACKET_START);
        out.extend_from_slice(normalized.as_bytes());
        out.extend_from_slice(BRACKET_END);
        out
    } else {
        normalized.into_bytes()
    };
    PasteValidation::Ok { bytes, bracketed }
}

/// A bounded paste stream: a byte queue with a read position.
///
/// The pump (N17b) pulls chunks of at most `max_size` bytes per
/// iteration and sends them to the PTY in order. Bracket markers are
/// already part of the byte sequence (added by [`validate_paste`]), so
/// the stream is stateless with respect to bracketing.
pub struct PasteStream {
    bytes: Vec<u8>,
    pos: usize,
    cancelled: bool,
}

impl PasteStream {
    /// Create a new paste stream from validated bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            pos: 0,
            cancelled: false,
        }
    }

    /// Get the next chunk (up to `max_size` bytes), or `None` if the
    /// stream is exhausted or cancelled. `max_size` must be positive
    /// for the stream to make progress.
    pub fn next_chunk(&mut self, max_size: usize) -> Option<&[u8]> {
        if self.cancelled || self.pos >= self.bytes.len() {
            return None;
        }
        let end = (self.pos + max_size).min(self.bytes.len());
        let chunk = &self.bytes[self.pos..end];
        self.pos = end;
        Some(chunk)
    }

    /// Cancel the stream. Any remaining bytes are dropped.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }

    /// Check if the stream is fully sent (or cancelled).
    pub fn is_done(&self) -> bool {
        self.cancelled || self.pos >= self.bytes.len()
    }

    /// Get the number of bytes remaining (0 when cancelled).
    pub fn remaining(&self) -> usize {
        if self.cancelled {
            0
        } else {
            self.bytes.len() - self.pos
        }
    }
}

/// Normalize paste newlines: `\r\n` → `\n`, standalone `\r` → `\n`.
fn normalize_newlines(paste: &str) -> String {
    // First pass folds CRLF to LF; the second pass folds any standalone
    // CR to LF. LF passes through unchanged.
    paste.replace("\r\n", "\n").replace('\r', "\n")
}
