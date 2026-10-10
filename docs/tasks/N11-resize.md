# N11: Window/DPI to grid and PTY resize

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N10.
- Read: [03-architecture.md](../03-architecture.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Keep renderer geometry, terminal state and child window size coherent.

## Small implementation increment

Implement deterministic physical cell metrics and valid row/column derivation. Resize engine and PTY with documented ordering; tag snapshots by geometry epoch. Suspend zero-sized rendering and retain last valid PTY geometry. Invalidate/rebuild on DPI changes.

If the listed work needs multiple concepts/commits, create explicit N11a/N11b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Child-reported size eventually matches the displayed grid; SIGWINCH updates a TUI; stale epoch frames are rejected; tiny/minimized windows never request zero columns.

## Validation

Headless size/ordering tests and live resize/minimize/DPI tests; flood output during resize and inspect reflow/cursor alignment.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(window): synchronize terminal and PTY resize (N11)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No blocking font or GPU operation under terminal lock.

