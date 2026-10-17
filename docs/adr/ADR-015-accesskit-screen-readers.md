# ADR-015: Screen readers through AccessKit

- **Status**: accepted
- **Date**: 2026-10-17
- **Builds on**: ADR-012 (feasibility of exposing terminal text), card N37

## Problem and evidence

Screen readers on Linux read applications through AT-SPI2 on D-Bus. winit
and wgpu expose nothing, so nora has no accessible text at all (the product
contract lists screen-reader integration as an unresolved limitation).
ADR-012 found an AT-SPI2 provider feasible and planned it as card N37:
an object tree (window, terminal, text), the grid as text, caret and
selection, and change events.

## Options

1. **Hand-written AT-SPI2 provider** on a D-Bus crate: the registration,
   `Accessible`, `Text`, `Component` and event interfaces, and their
   edge cases (offsets in characters, extents, caret and text-changed
   events), all owned by nora.
2. **AccessKit** (`accesskit` 0.25 + `accesskit_unix` 0.24): the
   cross-toolkit Rust accessibility model (used by egui and others). A
   tree of nodes with roles; text is `TextRun` nodes with per-character
   lengths, positions and widths, and a text selection as caret. The Unix
   adapter implements AT-SPI2 over zbus, runs on its own thread, and
   derives the AT-SPI2 events (text inserted and removed, caret moved,
   selection changed) by diffing successive trees.

## Decision

Option 2. A `Role::Terminal` node with `TextRun` children gets the AT-SPI2
`Text` interface (`accesskit_consumer` `supports_text_ranges`), which is
what Orca uses for terminals. nora builds the tree from the active pane's
snapshot after each frame: one run per visible row (trailing blanks
trimmed, `\n` between rows), the caret at the cursor or the selection, and
character positions from the cell grid. It sends a tree only while an
assistive technology has activated the adapter and only when the text,
caret or selection changed; with none active the cost is a flag check.

`accesskit_winit` is not used: on Unix it only forwards to
`accesskit_unix`, but it needs the event loop at construction, which nora
does not have where it creates its window. The Unix adapter needs neither.

## Consequences

- 49 crates enter the lock (zbus and its async stack, atspi, accesskit),
  for Linux and the BSDs only; other platforms compile a no-op.
- The adapter's thread connects to the session bus only when the
  accessibility bus is enabled (`org.a11y.Status`).
- Validation uses AT-SPI2 itself (`gdbus` against the accessibility bus)
  under Xvfb in the e2e harness; Orca's speech is a manual check.
