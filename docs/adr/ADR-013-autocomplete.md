# ADR-013: Shell-cooperative inline autocomplete

Status: accepted (2026-10-16); in implementation. Step 1 is done
(`src/shell_input.rs`, parsing in `src/prompt_marker.rs`).

## Problem

Suggest text as the user types at a shell prompt (commands, arguments,
paths), shown inline and accepted with a key, without corrupting terminal
state or interfering with full-screen programs, password prompts, IME
composition or terminal protocols.

The terminal does not own the command line. The shell's line editor does
(readline, ZLE, fish's reader): it decides what the buffer contains after
history recall, completion, kill/yank and multi-line edits. A terminal that
infers the buffer from the screen or from the keys it forwarded will be
wrong whenever the editor changes the line by itself.

## Prior art (primary sources)

- **zsh-autosuggestions** draws ghost text inside zsh: it sets `POSTDISPLAY`
  and a `region_highlight` entry after `BUFFER`; strategies `history`,
  `completion` (via zpty) and `match_prev_cmd`; Right arrow or End at the end
  of the buffer accepts, `forward-word` accepts partially, history widgets
  clear it.
  <https://raw.github.com/zsh-users/zsh-autosuggestions/master/zsh-autosuggestions.zsh>
- **zsh `zle-line-pre-redraw`** is a special widget run before every line
  redraw (added for highlighting; `$WIDGET` keeps the invoking widget's name).
  It is the hook that lets a plugin see every buffer change.
  <https://zsh.sourceforge.io/Doc/Release/zsh_17.html>,
  <https://www.zsh.org/mla/workers/2016/msg01426.html>
- **fish** has built-in autosuggestions in its reader (history and
  completions).
- **bash/readline** has no per-keystroke hook. `bind -x` runs a function on
  a specific key with `READLINE_LINE`/`READLINE_POINT`; ble.sh replaces the
  editor to get hooks.
- **kitty's OSC 133** extensions accept the executed command line on the
  command-start marker (`OSC 133;C;cmdline=...`) and the exit status on
  `OSC 133;D;<status>`.
  <https://git.wessel.gg/mirrors/kitty/src/commit/c0927a36431ae3bf082d67aee9e12b0e9f7af460/docs/shell-integration.rst>

## Options considered

1. **Screen inference.** Read the prompt line from the grid between the
   OSC 133 B marker and the cursor. Rejected: wrong after wrapped or
   multi-line edits, right-prompts, syntax-highlighting redraws and editor
   changes the terminal never sees.
2. **Keystroke shadow.** Reconstruct the line from the bytes nora forwarded.
   Rejected: diverges on history recall, completion, kill/yank and any
   editor-side change.
3. **Shell does everything** (zsh-autosuggestions model). Robust, but the
   suggestion model lives in each shell, cannot use terminal-side context
   (which directory a command succeeded in, across shells and panes), and
   cannot be configured or rendered consistently by nora.
4. **Shell reports, terminal suggests and renders (chosen).** The shell
   integration reports the authoritative buffer and cursor; nora computes
   the suggestion and draws it as ghost text; acceptance sends the suffix
   as typed input, so the shell's editor inserts it like any keystrokes.

## Decision

Option 4, with these parts:

- **Protocol.** A private OSC from the shell integration on every buffer
  change: `OSC 6973 ; buffer ; <cursor> ; <base64 UTF-8 buffer> ST`, plus
  `OSC 6973 ; end ST` when the editor finishes (accept or abort). nora
  shows suggestions only while the last OSC 133 marker is B (input) and a
  buffer report is current. (6973 is unassigned in the xterm OSC list;
  final number to be checked against the kitty/iTerm2/WezTerm registries
  before release.)
- **Shells.** zsh: `zle-line-pre-redraw` and `zle-line-finish` via
  `add-zle-hook-widget`. fish: not enabled by default (fish already
  autosuggests; showing two would conflict). bash: only with ble.sh hooks;
  plain bash gets no inline suggestions (documented limitation).
- **Model, local only.** Candidates from (a) the user's executed command
  lines reported by `OSC 133;C;cmdline=`, scored by frecency per working
  directory (OSC 7) and by exit status (OSC 133 D; failures demoted);
  (b) path completion for the token under the cursor, from the pane's
  reported cwd; (c) executable names on the shell's PATH for the first
  token. Stored in `$XDG_STATE_HOME/nora/history` with a size cap; disabled
  entirely with `[autocomplete] enabled = false`; never stores lines typed
  while the tty has ECHO off.
- **Rendering.** Ghost text after the cursor in a dim color, through the
  same scene path as the IME preedit; never written into the grid.
- **Acceptance.** Right arrow or End with the cursor at the end of the
  buffer accepts the whole suggestion (the zsh-autosuggestions and fish
  convention); Alt+Right accepts one word. nora sends the suffix as typed
  bytes. Every other key passes through unchanged and the next buffer
  report recomputes or clears the suggestion. Keys are configurable.
- **Suppression.** No suggestion when: no current buffer report; alternate
  screen; IME composition active; the PTY's ECHO flag is off (password
  prompts); a bracketed paste is in flight; the pane is not focused.

## Consequences

- Works where the shell can report its buffer (zsh today; bash with
  ble.sh). This is a deliberate trade: correctness over coverage.
- The shell integration grows a per-keystroke hook; its cost (one OSC
  write per redraw) is measured before enabling it by default.
- New state and storage: a history file under the user's state directory,
  capped and opt-out, documented with what it contains.

## Prototype plan and acceptance

1. Engine: parse `OSC 6973` and `OSC 133;C;cmdline=`; expose the current
   buffer per pane. Unit tests for parsing and the B/C gating.
   **Done 2026-10-16:** `cmdline=` (shell-quoted, unquoted by
   `shell_input::unquote_shell`) and `cmdline_url=` (percent-encoded), the
   `D` exit status, and markers with parameters (previously forwarded
   unrecognized). The buffer cursor counts characters. The executed
   commands (with cwd and exit status; at most 256 / 256 KiB pending) are
   taken by the consumer once their status arrives. Tests:
   `tests/shell_input.rs`, `pty_shell_input_reports_reach_shared_state`.
2. zsh integration script; e2e: type into a zsh prompt under nora and
   assert the reported buffer matches `$BUFFER` after history recall,
   completion and kill/yank.
   **Done 2026-10-16:** `shell/nora-zsh.zsh` (base64 and percent encoding
   in pure zsh; hook widgets need `zle -N` before `add-zle-hook-widget`).
   e2e `zshell` checks the buffer sequence through typing, Tab completion,
   Ctrl+U, Ctrl+Y and Up (`NORA_TRACE_SHELL=1` logs each report).
3. Suggestion model with frecency; unit tests on ranking.
   **Done 2026-10-16** (history part): `src/suggest.rs`. Score = count x
   recency (4 within an hour, 2 within a day, 1 within a week, else 0.5),
   x 3 when it ran in the current directory, x (0.2 + 0.8 x success
   ratio). Not recorded: lines starting with a space, multi-line or with
   control characters, over 4 KiB. Caps: 5 000 entries, 2 MiB; the lowest
   ranked is evicted. File `$XDG_STATE_HOME/nora/history` (mode 0600,
   atomic rename, damaged lines skipped). Path and PATH candidates
   (sources b and c) are not implemented yet. Tests: `tests/suggest.rs`.
4. Rendering and acceptance; e2e: a suggestion appears for a prefix of a
   previously run command, Right accepts it, and the command executed is
   exactly the suggestion; no suggestion in vim, at a `read -s` prompt, or
   during IME composition.
   **Done 2026-10-16:** the pump owns the history (loaded at start; saved
   when changed, at most every 30 s, and at shutdown), records each pane's
   finished commands, and sends `AppEvent::Suggestion` on every buffer
   change. The window draws it through the preedit path (foreground 2/5
   of the way from the background) and drops it as soon as other input
   goes to the shell, so a stale suggestion is never typed. Bindings
   `accept_suggestion` (Right, End) and `accept_suggestion_word`
   (Alt+Right) fall through when there is none. e2e `suggest`: ghost
   drawn, Right then Enter runs exactly the suggested command, history
   saved. vim and `read -s` are excluded by construction (no buffer report
   outside the input phase); the alternate screen and an in-flight paste
   are checked too. **Open:** the ECHO-off check (the PTY master is owned
   by the event loop); the IME case is unverified with a real input
   method. (SIGTERM/SIGHUP/SIGINT now close nora in order and save the
   history: `src/signals.rs`, Unix only.)
5. Measure keystroke-to-ghost-text latency and the cost of the zsh hook.

## Gate

Implementation started on 2026-10-16 with M1-M4 accepted and M5 partial.
The macOS PTY tests are green in CI and scrollback search is done. Still
open from M5: interactive use on macOS and Windows, and a manual IME
check.
