# N23: Tabs and active-session routing

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N22.
- Read: [03-architecture.md](../03-architecture.md), [02-product.md](../02-product.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Run independent shell sessions in one window with a minimal tab UI.

## Small implementation increment

Add workspace TabId/PaneId ownership, new/close/switch/reorder as separate useful slices. Render tab titles from policy-filtered engine metadata. Route keyboard/focus/clipboard/search only to the correct active pane. Cap sessions; reuse the one window/device/font system.

If the listed work needs multiple concepts/commits, create explicit N23a/N23b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Each tab has independent output/history/cwd/session lifecycle. Active input never reaches another tab. Closing a tab reaps its sessions and stale events are ignored. A background flood does not build a snapshot queue.

## Validation

Workspace/session generation tests; two child helpers with distinct tagged echoes; close/switch during output; live keyboard shortcuts/tab UI.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(workspace): add independent terminal tabs (N23)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not create one GPU device or font system per tab.

