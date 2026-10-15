# N37: Accessible terminal-text exposure (Linux AT-SPI2)

Implementation status: not_started. This card is a plan, not evidence of
completion.

- Dependencies: N36 (ADR-012).
- Read: [13-extensions.md](../13-extensions.md), [03-architecture.md](../03-architecture.md), [ADR-012](../adr/ADR-012-accessible-terminal-text.md).

## Observable goal

Expose the terminal grid as accessible text on Linux via an AT-SPI2 provider,
so screen readers and other assistive technologies can read the terminal
content, cursor, and selection.

## Small implementation increment

One slice at a time (N37a, then N37b, then N37c). Each slice is independently
usable; finish, validate, commit and push each before the next. The accessible
text is derived from the existing vendored `Term` grid — no new VT parser or
screen model.

## Slices

- **N37a: AT-SPI2 provider skeleton.** Connect to the AT-SPI2 D-Bus bus,
  register nora as an application, and expose the accessible object tree
  (window → terminal viewport → text). Acceptance: nora registers as an
  AT-SPI2 application; the object tree is queryable via `accerciser` or an
  AT-SPI2 dump tool.
- **N37b: Terminal grid as accessible text.** 2D→1D mapping (row-major,
  newline between rows) of the screen (and scrollback when scrolled) into the
  AT-SPI2 `Text` interface (`get_text`, `character_extents`,
  `offset_to_position`). Acceptance: the terminal text is readable via
  AT-SPI2 and matches the visible grid.
- **N37c: Cursor/selection exposure + events.** Expose the terminal cursor and
  selection as AT-SPI2 text state (caret offset, selection range) and emit
  accessibility events (text changed, cursor moved, selection changed).
  Acceptance: a screen reader (Orca) announces terminal output and cursor
  movement; the selection is exposed.

## Acceptance

Each slice has its own acceptance (above). Overall: the terminal text, cursor,
and selection are exposed to Linux assistive technologies via AT-SPI2, with
accurate limitations documented.

## Validation

Platform-specific: verify with actual AT-SPI2 tools (`accerciser`, AT-SPI2
dump). Accessibility behavior with actual tools (Orca) if pursued. Document
prerequisites (AT-SPI2 D-Bus bus, accessibility enabled) and any pending
hardware. Also run relevant formatting/Clippy/build checks; record commands
actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff;
commit per slice (suggested: `feat(a11y): N37<slice> … (N37)`); push to the
configured remote branch; verify remote state. A failed push blocks the next
slice.

## Guardrail

Do not treat this as a broad accessibility-compliance effort; the goal is to
expose the terminal text to assistive technologies (the primary need). Keep
the 2D→1D mapping and event emission bounded. Do not add a new VT parser or
screen model; derive accessible text from the existing `Term` grid.
