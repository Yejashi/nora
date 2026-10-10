# N16: Mouse reports and focus behavior

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N15.
- Read: [06-compatibility.md](../06-compatibility.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Honor application mouse/focus modes while preserving local selection.

## Small implementation increment

Implement requested mouse click/drag/motion encoding and grid coordinates, release/modifier handling, wheel routing and Shift selection override. Route OS/pane focus to the active session under its mode. Keep pure encoding separate from winit events.

If the listed work needs multiple concepts/commits, create explicit N16a/N16b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

A mouse-aware TUI receives correct reports; Shift allows local selection; normal history scrolling and alternate/application scrolling follow documented modes; focus events are not duplicated.

## Validation

Exact mode/coordinate/modifier unit fixtures plus TUI/manual focus/selection/wheel tests; bounds at pane edges.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): add mouse and focus reporting (N16)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Encode only supported modes and update the compatibility table; no guessed protocol extensions.

