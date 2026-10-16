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
                        Some(old) => self.executed_bytes -= old.cmdline.len(),
                        None => break,
                    }
                }
            }
            ShellEvent::CommandExit(status) => {
                if let Some(last) = self.executed.back_mut()
                    && last.exit.is_none()
                {
                    last.exit = Some(*status);
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

    /// Take the executed commands recorded so far, oldest first. A
    /// command whose exit status has not arrived yet stays, so its status
    /// is not lost.
    pub fn take_executed(&mut self) -> Vec<ExecutedCommand> {
        let done = self
            .executed
            .iter()
            .position(|c| c.exit.is_none())
            .unwrap_or(self.executed.len());
        let taken: Vec<ExecutedCommand> = self.executed.drain(..done).collect();
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
pub fn unquote_shell(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '\'' => loop {
                match chars.next()? {
                    '\'' => break,
                    c => out.push(c),
                }
            },
            '"' => loop {
                match chars.next()? {
                    '"' => break,
                    '\\' => {
                        let next = chars.next()?;
                        if !matches!(next, '"' | '\\' | '$' | '`') {
                            out.push('\\');
                        }
                        out.push(next);
                    }
                    c => out.push(c),
                }
            },
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        '\\' => out.push(ansi_c_escape(&mut chars)?),
                        c => out.push(c),
                    }
                }
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// One escape inside `$'...'`, after its backslash.
fn ansi_c_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    let c = chars.next()?;
    Some(match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        'a' => '\x07',
        'b' => '\x08',
        'e' | 'E' => '\x1b',
        'f' => '\x0c',
        'v' => '\x0b',
        'x' => {
            let mut value = 0u32;
            let mut digits = 0;
            while digits < 2
                && let Some(d) = chars.peek().and_then(|c| c.to_digit(16))
            {
                value = value * 16 + d;
                chars.next();
                digits += 1;
            }
            if digits == 0 {
                return None;
            }
            char::from_u32(value)?
        }
        '0'..='7' => {
            let mut value = c.to_digit(8)?;
            let mut digits = 1;
            while digits < 3
                && let Some(d) = chars.peek().and_then(|c| c.to_digit(8))
            {
                value = value * 8 + d;
                chars.next();
                digits += 1;
            }
            char::from_u32(value)?
        }
        other => other,
    })
}
