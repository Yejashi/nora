# N17: Bounded ordinary and bracketed paste

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N16.
- Read: [03-architecture.md](../03-architecture.md), [06-compatibility.md](../06-compatibility.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Paste safely and losslessly within a real byte budget, including slow children.

## Small implementation increment

Validate whole paste size before writing; normalize newlines/control policy; handle bracket terminator injection. Stream a bounded ordered transaction with capacity feedback; keep key order and parser replies defined. Add cancellation that closes an already-open bracketed transaction before normal input resumes.

If the listed work needs multiple concepts/commits, create explicit N17a/N17b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Ordinary and bracketed byte streams match policy; a 1 MiB paste to a slow child remains bounded/responsive; oversized paste is visibly rejected before any write; cancellation leaves the child input protocol coherent.

## Validation

Exact paste payload/terminator tests; real blocked/slow child and query-during-paste tests; byte-reservation/cancel/close accounting.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(input): stream bounded bracketed paste (N17)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

A frontend queue forwarding to unbounded engine input is not an implementation of this feature.

