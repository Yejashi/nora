# ADR-010: Terminal identity — dedicated `nora` terminfo entry

- **Status**: accepted
- **Date**: 2026-10-14
- **Deciders**: nora team
- **Supersedes**: none

## Context

A terminal emulator advertises its identity to child processes through the
`TERM` environment variable. Applications (editors, TUIs, shells) read the
terminfo entry named by `TERM` to learn what the terminal supports: cursor
addressing, key sequences, color depth, modes, and erase/clear sequences.

Two options exist for nora's identity:

1. **Dedicated `nora` terminfo entry** — nora compiles and installs its own
   terminfo entry under `~/.terminfo/n/nora` (or a system path) and sets
   `TERM=nora` for its children. The entry advertises exactly the
   capabilities nora has tested and validated, no more and no less.

2. **`xterm-256color` fallback** — nora sets `TERM=xterm-256color` and relies
   on the ubiquitous xterm-256color entry. This requires no installation on
   remote hosts, but advertises capabilities nora has not validated (e.g.
   xterm-specific key encodings, `khome`/`kend` modifier forms, `xterm`-only
   mode sequences) and hides nora's actual behavior from applications that
   branch on `TERM`.

The guardrail for this card is: *"No default unknown `TERM` on remote hosts
without an explained installation/fallback path."* A remote host that has
neither the `nora` entry nor a fallback would leave `TERM` unset or pointing
at a missing entry, which breaks curses/TUI programs. The decision must
therefore pair the identity with a documented installation path and a
documented fallback.

## Decision

**Ship a dedicated `nora` terminfo entry**, derived from a minimal,
test-backed capability set. Set `TERM=nora` and `COLORTERM=truecolor` for
every child nora spawns. Document the installation path (compile with `tic`,
install under `~/.terminfo` or `/usr/share/terminfo`) and the fallback
(`TERM=xterm-256color`) for remote hosts where the `nora` entry is not
installed.

### Rationale

- **Honest advertising.** A dedicated entry advertises only what nora has
  tested. The xterm-256color entry advertises xterm-specific behavior
  (modifier-encoded keys, `xterm`-only modes) that nora does not implement,
  which can cause applications to emit sequences nora does not understand or
  to skip features nora does support.
- **Test-backed.** Every capability in the `nora` entry is backed by a test
  in `tests/` (cursor addressing, key sequences, SGR styles, modes, colors,
  erase/clear). The entry is the compiled form of the test suite's
  assertions about nora's behavior.
- **Minimal.** The entry is small: a handful of boolean/numeric capabilities
  and the key sequences nora has validated. It does not claim kitty graphics,
   image protocols, or any capability that is a later milestone.
- **Portable.** The entry is a plain text file that compiles with `tic` on
  any host with ncurses. The installation path is one `tic` invocation.
- **Fallback is explicit.** Remote hosts that cannot install the `nora`
  entry fall back to `TERM=xterm-256color`, which is present on every
  Linux distribution. The fallback is documented in the ADR and in
  `docs/07-build-release.md`, satisfying the guardrail.

## Capability set

The `nora` terminfo entry advertises the following capabilities. Each is
backed by a test in `tests/` (the test file is named in the "Tested by"
column). Capabilities not listed are not advertised; nora may still accept
them, but it does not promise them.

### Boolean capabilities

| Cap | Meaning | Tested by |
|-----|---------|-----------|
| `am` | Auto-wrap at right margin | `engine_adapter::pending_wrap_continues_on_next_row` |
| `bce` | Backspace erases (clears the cell) | `engine_adapter` (SGR/erase tests) |
| `km` | Keyboard local mode | `navigation_input` (DECCKM) |
| `mir` | Move inserts (cursor insert mode) | `engine_adapter` (insert tests) |
| `msgr` | Status line present | `engine_adapter` (status line) |
| `xenl` | New line works | `engine_adapter` (CRLF) |

### Numeric capabilities

| Cap | Value | Tested by |
|-----|-------|-----------|
| `colors` | 256 | `engine_adapter` (256-color SGR) |
| `pairs` | 65536 | `engine_adapter` (color pairs) |
| `cols` | 80 | default grid |
| `lines` | 24 | default grid |
| `it` | 8 | tab stop interval |

### String capabilities — cursor addressing

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `cup` | `\E[%i%p1%d;%p2%dH` | `engine_adapter` (CUP) |
| `home` | `\E[H` | `engine_adapter` (home) |

### String capabilities — cursor movement

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `cub1` | `\E[D` | `navigation_input` (normal arrows) |
| `cud1` | `\E[B` | `navigation_input` (normal arrows) |
| `cuf1` | `\E[C` | `navigation_input` (normal arrows) |
| `cuu1` | `\E[A` | `navigation_input` (normal arrows) |
| `kcub1` | `\EOD` | `navigation_input` (app arrows) |
| `kcud1` | `\EOB` | `navigation_input` (app arrows) |
| `kcuf1` | `\EOC` | `navigation_input` (app arrows) |
| `kcuu1` | `\EOA` | `navigation_input` (app arrows) |

### String capabilities — keys

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `kbs` | `\177` | `navigation_input` (backspace) |
| `kHOM` | `\E[H` | `navigation_input` (Home, normal) |
| `kEND` | `\E[F` | `navigation_input` (End, normal) |
| `kIC` | `\E[2~` | `navigation_input` (Insert) |
| `kDC` | `\E[3~` | `navigation_input` (Delete) |
| `kLFT` | `\E[5~` | `navigation_input` (Page Up) |
| `kRIT` | `\E[6~` | `navigation_input` (Page Down) |
| `kf1` | `\EOP` | `navigation_input` (F1) |
| `kf2` | `\EOQ` | `navigation_input` (F2) |
| `kf3` | `\EOR` | `navigation_input` (F3) |
| `kf4` | `\EOS` | `navigation_input` (F4) |
| `kf5` | `\E[15~` | `navigation_input` (F5) |
| `kf6` | `\E[17~` | `navigation_input` (F6) |
| `kf7` | `\E[18~` | `navigation_input` (F7) |
| `kf8` | `\E[19~` | `navigation_input` (F8) |
| `kf9` | `\E[20~` | `navigation_input` (F9) |
| `kf10` | `\E[21~` | `navigation_input` (F10) |
| `kf11` | `\E[23~` | `navigation_input` (F11) |
| `kf12` | `\E[24~` | `navigation_input` (F12) |

### String capabilities — styles (SGR)

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `bold` | `\E[1m` | `engine_adapter` (SGR bold) |
| `dim` | `\E[2m` | `text_policy` (N13 faint) |
| `blink` | `\E[5m` | `text_policy` (N13) |
| `invis` | `\E[8m` | `text_policy` (N13 conceal) |
| `smso` | `\E[3m` | `text_policy` (N13 italic) |
| `smul` | `\E[4m` | `text_policy` (N13 underline) |
| `rev` | `\E[7m` | `text_policy` (N13 reverse) |
| `rmso` | `\E[m` | `engine_adapter` (SGR reset) |
| `rmul` | `\E[m` | `text_policy` (SGR reset) |
| `sgr` | `\E[0;10%p1%;%p2%%;%p3%%;%p4%%;%p5%%;%p6%%;%p7%m` | `engine_adapter` (SGR) |

### String capabilities — modes

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `smcup` | `\E[?1049h` | `engine_adapter` (alt screen) |
| `rmcup` | `\E[?1049l` | `engine_adapter` (alt screen) |
| `smam` | `\E[?7h` | `engine_adapter` (autowrap) |
| `rmam` | `\E[?7l` | `engine_adapter` (autowrap) |
| `smkx` | `\E[?1h\E=` | `navigation_input` (DECCKM + DECKPAM) |
| `rmkx` | `\E[?1l\E>` | `navigation_input` (DECCKM + DECKPAM) |

### String capabilities — colors

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `initc` | `\E]4;%p1%d;rgb:%p2%{255}%*%{1000}%/%2.2X/%p3%{255}%*%{1000}%/%2.2X/%p4%{255}%*%{1000}%/%2.2X\E\\` | `session_bounded` (OSC 4) |

### String capabilities — erase/clear

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `clear` | `\E[H\E[2J` | `engine_adapter` (ED) |
| `ed` | `\E[J` | `engine_adapter` (ED) |
| `el` | `\E[K` | `engine_adapter` (EL) |
| `el1` | `\E[1K` | `engine_adapter` (EL) |
| `dch1` | `\E[P` | `engine_adapter` (DCH) |
| `dl1` | `\E[M` | `engine_adapter` (DL) |

### String capabilities — cursor visibility

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `civis` | `\E[?25l` | `engine_adapter` (cursor visibility) |
| `cnorm` | `\E[?25h` | `engine_adapter` (cursor visibility) |

### String capabilities — basic

| Cap | Sequence | Tested by |
|-----|----------|-----------|
| `cr` | `\r` | `engine_adapter` (CRLF) |
| `ind` | `\n` | `engine_adapter` (LF) |
| `bel` | `\a` | `engine_adapter` (bell) |
| `ht` | `\t` | `engine_adapter` (tab) |

## COLORTERM handling

nora sets `COLORTERM=truecolor` for every child it spawns. This is the
standard signal that the terminal supports 24-bit (true) color, independent
of the `TERM` value. Applications that check `COLORTERM` (e.g. `ls`, `git`,
`vim`) will enable true-color output regardless of whether `TERM` is `nora`
or `xterm-256color`.

nora supports true-color (SGR 38;2 / 48;2) in addition to the 256-color
palette, so `COLORTERM=truecolor` is accurate.

## SSH fallback

On a remote host, nora's children inherit the environment from the SSH
session, which includes `TERM` set by the local terminal. The fallback
strategy is:

1. **Preferred**: the remote host has the `nora` terminfo entry installed
   (see "Installation" below). nora sets `TERM=nora` for its children, and
   applications use the `nora` entry.
2. **Fallback**: the remote host does not have the `nora` entry. nora sets
   `TERM=xterm-256color` for its children, and applications use the
   ubiquitous xterm-256color entry. The fallback is safe because every
   capability in the `nora` entry is also present in xterm-256color (the
   `nora` entry is a strict subset of xterm-256color's capabilities).

The choice between preferred and fallback is made at nora startup: nora
checks whether the `nora` terminfo entry is resolvable (via `TERMINFO` or
the default search path). If it is, nora sets `TERM=nora`; otherwise it sets
`TERM=xterm-256color`. This ensures no child ever sees an unknown or
missing `TERM`, satisfying the guardrail.

## Installation

To install the `nora` terminfo entry on a host:

```sh
# From the nora repository root:
tic -x terminfo/nora.terminfo
```

This compiles the entry and installs it under `~/.terminfo/n/nora` (the
user's home directory). To install system-wide:

```sh
sudo tic -x -o /usr/share/terminfo terminfo/nora.terminfo
```

The `-x` flag preserves extended capabilities (those not in the base
terminfo set) that nora uses.

## Consequences

- Applications that branch on `TERM` will see `nora` and can enable
  nora-specific features (e.g. kitty keyboard negotiation, bracketed paste,
  mouse reporting) without guessing.
- The terminfo entry is a living document: as nora adds and tests new
  capabilities, the entry is updated and recompiled.
- The `nora` entry is a strict subset of xterm-256color, so the fallback
  is always safe.
- Remote hosts that cannot install the `nora` entry fall back to
  `TERM=xterm-256color`, which is present on every Linux distribution.

## Alternatives considered

- **`xterm-256color` only (no dedicated entry).** Simpler, but advertises
  capabilities nora has not validated and hides nora's actual behavior from
  applications that branch on `TERM`. Rejected.
- **`xterm-kitty` identity.** Nora is not kitty; claiming the kitty identity
  would advertise kitty-specific capabilities (graphics, image protocols)
  that nora does not implement. Rejected.
- **No `TERM` set (inherit from parent).** Leaves `TERM` unset or pointing
  at a missing entry on remote hosts, which breaks curses/TUI programs.
  Rejected (violates the guardrail).
