# ADR-012: Accessible terminal-text exposure (Linux AT-SPI2)

- **Status**: accepted (feasibility assessment)
- **Date**: 2026-10-15
- **Deciders**: nora team
- **Supersedes**: none

## Context

nora is a terminal emulator for Linux (v0.1). Accessibility — exposing the
terminal text to screen readers and other assistive technologies — is a
"later" feature, off the v0.1 critical path (per `13-extensions.md`). This ADR
assesses the feasibility of exposing the terminal grid as accessible text on
Linux, the initial supported platform.

Key facts (verified against the pinned sources):

1. **Linux accessibility is AT-SPI2.** The standard Linux accessibility API is
   AT-SPI2 (Assistive Technology Service Provider Interface 2), a D-Bus-based
   interface consumed by screen readers (Orca), magnifiers, and other
   assistive technologies. Toolkit-based apps (GTK, Qt) expose accessibility
   via AT-SPI2 automatically. nora uses **winit** for windowing and **wgpu**
   for rendering; neither provides AT-SPI2 integration (winit has no
   accessibility layer). nora therefore must implement an AT-SPI2 provider
   explicitly.
2. **The terminal state is in the vendored `Term`.** The grid (screen +
   scrollback), cursor, and selection live in the vendored
   `alacritty_terminal` `Term`, reachable through `Engine::term` (the
   `FairMutex`-guarded handle). `Term::grid()` returns the `Grid<Cell>`; the
   cursor and selection are on the `Term`/`Grid`. This is the data source for
   accessible text — no new VT parser or screen model is needed.
3. **The terminal is a 2D grid of cells, not a flat text buffer.** AT-SPI2's
   `Text` interface expects a linear text model (character offsets, ranges,
   extents). The terminal grid is 2D (rows × columns) with per-cell attributes
   (fg/bg color, bold/italic/underline, etc.). Exposing the grid as
   accessible text requires a 2D→1D mapping (row-major, newline between rows)
   and a policy for cell attributes (expose the text; attributes are
   secondary and can be omitted or annotated in a later slice).

## Options

1. **No accessibility (v0.1)** — document as a limitation. Accessibility is a
   nice-to-have, not required for v0.1. The terminal text is not exposed to
   assistive technologies.
2. **AT-SPI2 provider (explicit D-Bus implementation)** — implement the
   AT-SPI2 D-Bus interface to expose the terminal grid as accessible text.
   Requires: (a) a D-Bus connection and AT-SPI2 application registration,
   (b) an accessible object tree (window → terminal viewport → text),
   (c) the AT-SPI2 `Text` interface (`get_text`, `character_extents`,
   `offset_to_position`, caret/selection), and (d) accessibility event
   emission (text changed, cursor moved, selection changed). Selected as the
   post-v0.1 path.
3. **winit native accessibility** — not available: winit has no AT-SPI2
   support. Rejected (no such API to build on).

## Decision

- **Feasibility: feasible; a significant post-v0.1 increment.** Exposing the
  terminal grid as accessible text on Linux is feasible via an explicit
  AT-SPI2 provider. The work decomposes into:
  1. **D-Bus + AT-SPI2 registration** — connect to the AT-SPI2 D-Bus bus,
     register nora as an application, and expose the accessible object tree
     (window → terminal viewport → text).
  2. **2D→1D text mapping** — expose the terminal grid (screen, and
     scrollback when scrolled) as a linear text model (row-major, newline
     between rows). The `Text` interface methods operate on this model.
  3. **Cursor/selection exposure** — expose the terminal cursor and selection
     as AT-SPI2 text state (caret offset, selection range).
  4. **Event emission** — emit AT-SPI2 events on text change, cursor move, and
     selection change so screen readers announce updates.
- **v0.1 scope: document as a limitation.** Accessibility is off the v0.1
  critical path. v0.1 documents that the terminal text is not exposed to
  assistive technologies; the AT-SPI2 provider is a post-v0.1 increment (N37).
- **No new VT parser or screen model.** The accessible text is derived from
  the existing `Term` grid; the 2D→1D mapping is a presentation concern in the
  accessibility layer, not a change to terminal state.

## Consequences

- **Follow-up work (N37):** implement an AT-SPI2 provider that exposes the
  terminal grid as accessible text, scoped as N37a (D-Bus + AT-SPI2
  registration + object tree), N37b (terminal grid as accessible text +
  2D→1D mapping + `Text` interface), and N37c (cursor/selection exposure +
  event emission). See `docs/tasks/N37-accessible-text.md`.
- **v0.1 limitation:** the terminal text is not exposed to assistive
  technologies in v0.1; documented in `13-extensions.md`.
- **Platform-specific:** AT-SPI2 is Linux-specific. macOS (VoiceOver) and
  Windows (UI Automation) ports require their own accessibility APIs — a
  separate investigation per `13-extensions.md` (not covered here).
- **No broad compatibility claim:** the AT-SPI2 provider exposes the terminal
  text to assistive technologies (the primary accessibility need for a
  terminal emulator); it does not imply full accessibility compliance.
