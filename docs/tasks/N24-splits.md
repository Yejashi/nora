# N24: Split panes and deterministic layout

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N23.
- Read: [03-architecture.md](../03-architecture.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Place independently running sessions in a simple binary split tree.

## Small implementation increment

Implement horizontal split, then vertical split, then focus/collapse/resize as individually complete slices. Use physical integer rectangles with deterministic remainder allocation and minimum grids. Add borders/focus indication and mouse coordinate routing. Closing a pane collapses the tree consistently.

If the listed work needs multiple concepts/commits, create explicit N24a/N24b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Two/four panes have nonoverlapping valid rectangles and independent sessions. Focus follows documented shortcuts. Small windows do not request zero grids; pane resize updates PTYs; close/focus paths remain valid.

## Validation

Layout property tests; 2/4-pane tagged echo and mouse routing; resize/font change during multiple floods; lifecycle/leak checks.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(workspace): add deterministic split panes (N24)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

A complete all-layout system is unnecessary; keep the binary tree small and explicit.

