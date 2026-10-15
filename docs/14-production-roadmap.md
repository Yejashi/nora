# Production roadmap (post-audit)

Written 2026-10-15 from [audit/2026-10-15-runtime-audit.md](audit/2026-10-15-runtime-audit.md).
It supersedes the "v0.1 accepted" status until M1 is re-accepted. Each
milestone is dependency-ordered and has an acceptance condition that is
observable from outside the code, plus a verification method. Finding IDs
(R*, I = input "F*", P*, CFG) refer to the audit.

**Evidence rule.** A feature counts as verified only when an end-to-end
check drives the real window (keyboard via xdotool, pixels via screenshot,
PTY state via files the child shell writes) or a real PTY. Library-level
unit tests are regression coverage, not acceptance. Manual Wayland checks
are recorded only with the command and output.

**Stop rule.** A milestone is done when its acceptance passes in CI (or,
for manual-only checks, is recorded with evidence). Further changes to a
done subsystem need a measured defect or a measured benefit.

## M0 — Evidence harness

- `scripts/e2e-x11.sh`: run the release binary in Xvfb (lavapipe), type
  with xdotool, capture screenshots, let the child write evidence files.
  Assertions in shell (pixel samples via ImageMagick, file contents).
- CI job running it on every push.
- Acceptance: the job fails on the pre-fix palette defect (black text) and
  passes on HEAD. Risk: Xvfb focus/keymap quirks; mitigated by
  `windowfocus` and plain ASCII scenarios.

## M1 — A correct single-pane terminal

| Slice | Findings | Acceptance (e2e unless noted) |
| --- | --- | --- |
| M1.1 colors | R1, R2, R9a | default text visible, ANSI/256/truecolor bg pixels match palette (done) |
| M1.2 PTY | P1, P2, P7, P9 | `stty -a` in the child shows `isig icanon echo onlcr icrnl`; `stty size` follows a window resize; no bytes injected on resize; headless `nora -- sleep 7` exits 0 |
| M1.3 keyboard | I-F1, F2, F5, F7, F14, F15 | Ctrl+D/W/X/T/F reach the shell (byte-exact via `od` in the child); closing the last pane exits; kitty protocol off until conformant; Shift+Tab, Alt+Backspace, Ctrl+\\, modified arrows encoded per xterm |
| M1.4 mouse | I-F4, F12, F16, F17 | SGR reports byte-exact; 1002 vs 1003 semantics; click selects only when reporting is off or with Shift; Shift+PageUp scrolls |
| M1.5 frames | I-F9, R7, R8 | redraw on expose/resize/zoom with no new output; Suboptimal/Outdated recover; atlas trimmed (long CJK soak keeps text) |
| M1.6 fonts | R3, R4, R5, CFG | `font.family`/`font.size` applied; cell height from font metrics; correct at scale 1, 1.25, 2 (unit geometry tests + screenshot at forced scale) |
| M1.7 bounds/latency | P3, P4, P5, P6, I-F18, P13 | unterminated OSC/APC capped (RSS bounded under a 100 MiB flood); input bytes bounded end to end; no fixed sleep on the echo path (latency measured, M3) |

Risk: M1.2/M1.3 change long-standing behaviour that tests pinned (for
example the raw termios); such tests are corrected, not preserved.

## M2 — Multi-pane UI

Tabs and splits drawn (per-pane origin, borders, tab bar), per-pane
staleness (R6/I-F3/F6), search bar UI (I-F10), IME preedit and cursor
area, window title (OSC 0/2), bell. Acceptance: e2e split/tab scenarios
with screenshots and per-pane `stty size`.

## M3 — Performance baseline, then optimization

Baseline first (on HEAD after M1): keypress-to-pixel latency (xdotool
timestamp to frame-presented log), frame time at 80x24 and 200x60,
throughput (`cat` of a fixed corpus), idle CPU, RSS, allocations per
frame, startup time. Then address measured hot spots (R12, P5, P8).
Acceptance: numbers recorded in `docs/performance/` with method; each
optimization shows a before/after.

## M4 — Customization

Coherent schema (Alacritty-shaped colors, typed key bindings with an
action enum, window, cursor, scrolling, mouse), validation with line
numbers, documented precedence, live reload (Ctrl+Shift+F5 and file
watch) applying each change class as `classify_change` already models.
Acceptance: every documented key has a consumer and an e2e or unit test;
`nora --check-config` reports errors with location.

## M5 — Cross-platform

Investigation first: isolate PTY (alacritty_terminal supports ConPTY),
clipboard, fonts, windowing (winit) and process management behind the
existing boundaries; add macOS and Windows CI build + headless tests.
Support levels are stated exactly: "verified" only for a platform whose
interactive path ran under a real display with recorded evidence;
"builds and passes headless tests in CI" otherwise.

## M6 — Autocomplete

Research and ADR first (shell-cooperative design: the shell's line
editor owns the buffer; the terminal learns prompt and command extents
via OSC 133 and a small shell-side protocol, never by scraping the
screen). Prototype, then implement opt-in, local-only by default, off in
alternate screen, password prompts and IME composition.
