# N12: Child exit, final output and pane close

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N11.
- Read: [03-architecture.md](../03-architecture.md), [02-product.md](../02-product.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Give sessions deterministic shutdown with visible final output and no leaks.

## Small implementation increment

Implement Starting/Running/Exiting/Exited/Closed transitions, finite final drain, idempotent close/reap and stale-event discard. Add --hold and explicit-command exit-status behavior. Handle launch/worker failures visibly. Guard local close of active work.

If the listed work needs multiple concepts/commits, create explicit N12a/N12b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Final emitted bytes are shown/captured before normal close; held panes show status; child is reaped once; descendant-held slave PTY cannot hang forever; repeated spawn/close leaves no sessions/threads/fds.

## Validation

Rust child helper tests for last bytes/exit code, close during paste queue, launch failure, descendant-held PTY and repeated cleanup. Manual close UI smoke.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(session): drain and close children reliably (N12)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Signal only owned targets under documented engine/session semantics; do not kill arbitrary process groups.

