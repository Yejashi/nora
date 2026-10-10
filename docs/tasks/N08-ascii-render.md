# N08: Live ASCII/color/cursor rendering

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N02, N07.
- Read: [05-rendering.md](../05-rendering.md), [04-performance.md](../04-performance.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Turn the live PTY state into a usable basic terminal display.

## Small implementation increment

Use the accepted text adapter to draw snapshot cells, backgrounds and cursor with clipping and batched passes. Present coherent full frames on demand. Reuse buffers/glyphs. Log backend/surface errors without terminal content. Keep this card limited to basic text/colors/cursor.

If the listed work needs multiple concepts/commits, create explicit N08a/N08b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Live shell output appears; repeated content is stable; conceal/reverse basics have fixtures; idle with blink disabled stops redrawing. Zero-sized/lost/outdated surface behavior is handled using pinned APIs.

## Validation

Offscreen cell/color/cursor tests; real window shell-output smoke; inspect idle redraw counts and resize/minimize recovery.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(render): draw live terminal cells (N08)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

This is a development slice; full Unicode/styles follow N13.

