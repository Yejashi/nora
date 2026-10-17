//! What the shell integration reports about its command line (ADR-013,
//! step 1): the line editor's buffer and cursor while the user edits a
//! command (`OSC 6973`), and each command line the shell executes
//! (`OSC 133 ; C ; cmdline=` / `cmdline_url=`) with its exit status
//! (`OSC 133 ; D ; <status>`).
//!
//! The shell's editor owns the command line; nora never infers it from
//! the screen or from forwarded keys. A buffer is current only between
//! the prompt's input marker (`B`) and the command's start (`C`), and only
//! until the editor reports `end`.
//!
//! These values are metadata from the child: they are shown and suggested
//! from, never executed by nora.

use std::collections::VecDeque;

use crate::prompt_marker::{MarkerKind, ShellEvent};

/// Most executed commands kept until the consumer takes them.
pub const MAX_EXECUTED: usize = 256;
/// Most bytes of executed command lines kept until taken.
pub const MAX_EXECUTED_BYTES: usize = 256 * 1024;

/// The line editor's buffer as last reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineBuffer {
    /// The buffer text.
    pub text: String,
    /// The cursor, in characters from the start of `text` (clamped to
    /// its length).
    pub cursor: usize,
}

impl LineBuffer {
    /// Whether the cursor is at the end of the buffer.
    pub fn cursor_at_end(&self) -> bool {
        self.cursor >= self.text.chars().count()
    }
}

/// A command line the shell executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutedCommand {
    /// The command line as the shell reported it.
    pub cmdline: String,
    /// The working directory the shell last reported (OSC 7), if any.
    pub cwd: Option<String>,
    /// The exit status from `OSC 133 ; D ; <status>`, once reported.
    pub exit: Option<i32>,
}

/// Per-pane shell input state, shared by the PTY filter (writer) and the
/// pump (reader).
#[derive(Debug, Clone, Default)]
pub struct ShellInput {
    phase: Option<MarkerKind>,
    buffer: Option<LineBuffer>,
    executed: VecDeque<ExecutedCommand>,
    executed_bytes: usize,
    /// `executed[..complete]` are finished: their exit status arrived, or
    /// the next prompt started without one (an integration that sends a
    /// bare `D`).
    complete: usize,
    generation: u64,
}

impl ShellInput {
    /// Fresh state: no marker seen, no buffer, nothing executed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one shell event into the state. `cwd` is the pane's current
    /// directory at the time of the event.
    pub fn apply(&mut self, event: &ShellEvent, cwd: Option<&str>) {
        match event {
            ShellEvent::Marker(marker) => {
                self.phase = Some(marker.kind);
                if marker.kind == MarkerKind::A {
                    self.complete = self.executed.len();
                }
                // A buffer belongs to one prompt's input phase.
                if marker.kind != MarkerKind::B {
                    self.buffer = None;
                }
            }
            ShellEvent::Buffer(buffer) => {
                let len = buffer.text.chars().count();
                self.buffer = Some(LineBuffer {
                    text: buffer.text.clone(),
                    cursor: buffer.cursor.min(len),
                });
            }
            ShellEvent::BufferEnd => self.buffer = None,
            ShellEvent::CommandLine(cmdline) => {
                self.executed_bytes += cmdline.len();
                self.executed.push_back(ExecutedCommand {
                    cmdline: cmdline.clone(),
                    cwd: cwd.map(str::to_string),
                    exit: None,
                });
                while self.executed.len() > MAX_EXECUTED || self.executed_bytes > MAX_EXECUTED_BYTES
                {
                    match self.executed.pop_front() {
                        Some(old) => {
                            self.executed_bytes -= old.cmdline.len();
                            self.complete = self.complete.saturating_sub(1);
                        }
                        None => break,
                    }
                }
            }
            ShellEvent::CommandExit(status) => {
                if self.complete < self.executed.len()
                    && let Some(last) = self.executed.back_mut()
                {
                    last.exit = Some(*status);
                    self.complete = self.executed.len();
                }
            }
            ShellEvent::Cwd(_) | ShellEvent::Hyperlink(_) => return,
        }
        self.generation += 1;
    }

    /// The editor's buffer while it is current: after the prompt's input
    /// marker (`B`), before the command starts, and until `end`.
    pub fn current_buffer(&self) -> Option<&LineBuffer> {
        match self.phase {
            Some(MarkerKind::B) => self.buffer.as_ref(),
            _ => None,
        }
    }

    /// Take the finished commands recorded so far, oldest first. A
    /// command still running stays until its exit status arrives (or the
    /// next prompt starts), so its status is not lost.
    pub fn take_executed(&mut self) -> Vec<ExecutedCommand> {
        let taken: Vec<ExecutedCommand> = self.executed.drain(..self.complete).collect();
        self.complete = 0;
        self.executed_bytes -= taken.iter().map(|c| c.cmdline.len()).sum::<usize>();
        taken
    }

    /// Bumped on every change, so a reader can skip unchanged state.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
/// Undo the shell quoting that `printf %q` (bash) and `${(q)...}` (zsh)
/// produce: backslash escapes, `'...'`, `"..."` and ANSI-C `$'...'`.
/// `None` when a quote is left open or an escape is cut off.
///
/// `\xHH` and octal escapes in `$'...'` are bytes, as in the shells (a
/// non-ASCII character in a C locale is quoted as its UTF-8 bytes, e.g.
/// `$'\303\251'`), so the result is decoded as UTF-8 at the end, with
/// invalid sequences replaced.
pub fn unquote_shell(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let push = |out: &mut Vec<u8>, c: char| {
        out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
    };
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => push(&mut out, chars.next()?),
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => push(&mut out, c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => {
                        let next = chars.next()?;
                        if !matches!(next, '"' | '\\' | '$' | '`') {
                            out.push(b'\\');
                        }
                        push(&mut out, next);
                    }
                    c => push(&mut out, c),
                }
            },
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        '\\' => match ansi_c_escape(&mut chars)? {
                            Escape::Byte(b) => out.push(b),
                            Escape::Char(c) => push(&mut out, c),
                        },
                        c => push(&mut out, c),
                    }
                }
            }
            c => push(&mut out, c),
        }
    }
    Some(match String::from_utf8(out) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    })
}

/// What one `$'...'` escape stands for.
enum Escape {
    /// A raw byte (`\xHH`, octal).
    Byte(u8),
    /// A character (named escapes, `\uHHHH`, `\UHHHHHHHH`, the rest).
    Char(char),
}

/// Up to `max` digits of `radix` from `chars`, and how many there were.
fn digits(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    radix: u32,
    max: usize,
    mut value: u32,
) -> (u32, usize) {
    let mut count = 0;
    while count < max
        && let Some(d) = chars.peek().and_then(|c| c.to_digit(radix))
    {
        value = value * radix + d;
        chars.next();
        count += 1;
    }
    (value, count)
}

/// One escape inside `$'...'`, after its backslash.
fn ansi_c_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<Escape> {
    let c = chars.next()?;
    Some(Escape::Char(match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        'a' => '\x07',
        'b' => '\x08',
        'e' | 'E' => '\x1b',
        'f' => '\x0c',
        'v' => '\x0b',
        'x' => {
            let (value, count) = digits(chars, 16, 2, 0);
            if count == 0 {
                return None;
            }
            return Some(Escape::Byte(value as u8));
        }
        '0'..='7' => {
            let (value, _) = digits(chars, 8, 2, c.to_digit(8)?);
            // Bash keeps the low byte of `\400`-`\777`.
            return Some(Escape::Byte(value as u8));
        }
        'u' | 'U' => {
            let (value, count) = digits(chars, 16, if c == 'u' { 4 } else { 8 }, 0);
            if count == 0 {
                return None;
            }
            char::from_u32(value)?
        }
        other => other,
    }))
}
