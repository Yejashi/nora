# N22: Native IME and composition UI

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N21.
- Read: [06-compatibility.md](../06-compatibility.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Support text composition without premature or duplicate PTY input.

## Small implementation increment

Display preedit as UI state, commit text exactly once, set candidate rectangle from current cursor/pane/DPI, and cancel/transfer composition on focus change. Verify local shortcut precedence while composing. Keep existing key encoding paths coherent.

If the listed work needs multiple concepts/commits, create explicit N22a/N22b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Preedit does not alter terminal state; commit emits once; candidate UI follows cursor/scale. Native Wayland and X11 composition results are separately recorded. Dead keys/AltGr remain usable.

## Validation

Event replay for preedit/commit/cancel/focus and duplicate paths; real IME/non-US layout/manual tests; resize while composing.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): integrate IME composition (N22)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Passing replay tests cannot replace a native IME check.

