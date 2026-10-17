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

**Ship a dedicated `nora` terminfo entry**, derived from the engine's (see
"Capability set"). Set `TERM=nora` and `COLORTERM=truecolor` for
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
- **Engine-derived.** The entry is the ncurses `alacritty` entry for
  nora's engine, minus what nora does not implement; the key sequences
  match nora's encoder and `tests/terminfo.rs` checks the core sequences
  through `tput`. It does not claim kitty graphics, image protocols, or any
  capability that is a later milestone.
- **Portable.** The entry is a plain text file that compiles with `tic` on
  any host with ncurses. The installation path is one `tic` invocation.
- **Fallback is explicit.** Remote hosts that cannot install the `nora`
  entry fall back to `TERM=xterm-256color`, which is present on every
  Linux distribution. The fallback is documented in the ADR and in
  `docs/07-build-release.md`, satisfying the guardrail.

## Capability set

**Amended 2026-10-17.** The first entry was a hand-written list of about
seventy capabilities. It lacked basics every curses program needs
(`setaf`, `setab`, `sgr0`, `op`, `csr`, `sc`, `rc`, `ri`, `khome`, `kend`,
`kdch1`, `kpp`, `knp`, `kmous`), named PageUp/PageDown as `kLFT`/`kRIT`,
made `smso` italic and had a malformed `sgr`. It is replaced as follows.

nora's engine is `alacritty_terminal`, so the entry is the ncurses
`alacritty` entry renamed to `nora|nora terminal emulator` and written out
in full (no `use=`, so a host without an alacritty entry still compiles
it). Starting from it, every key capability was checked against nora's
legacy key encodings (`src/input.rs`, `src/input/focus.rs`,
`src/input/mouse.rs`) and every output capability against what
alacritty_terminal 0.26 parses and nora renders. Capabilities nora does not
implement were removed: `blink` (and the blink parameter of `sgr`),
`flash` (DECSCNM), the media-copy group `mc0`/`mc4`/`mc5`/`mc5i`,
`meml`/`memu`, `smm`/`rmm` (mode 1034), `kb2` and `Ms` (OSC 52 is disabled).
`is2`/`rs2` drop DECSTR and the smooth-scroll mode, which the engine
ignores, and `rv` matches any version in the secondary device attributes
reply. The header comment of `terminfo/nora.terminfo` lists the same
changes.

`tests/terminfo.rs` compiles the entry with `tic` into a temporary
directory and checks, through `tput -T nora`, the sequences of the
capabilities listed above, that `sgr` composes bold, underline and reverse
into one SGR sequence, and that the removed capabilities stay absent.

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
   ubiquitous xterm-256color entry. The fallback works because the engine
   implements the xterm sequences that entry's common capabilities use
   (alacritty itself relies on the same fallback).

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
- The fallback is xterm-256color, whose common capabilities the engine
  implements.
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
