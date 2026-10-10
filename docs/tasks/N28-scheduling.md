# N28: Multi-pane fairness and presentation latency

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N27.
- Read: [03-architecture.md](../03-architecture.md), [04-performance.md](../04-performance.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Keep input responsive under output and presentation load.

## Small implementation increment

Measure P07 with input service, parse lock and surface wait timings. Tune bounded parse/write/control turns and redraw scheduling one change at a time. If measured surface/GPU work stalls the UI, use an ADR and small render-worker migration slices with complete snapshot handoff.

If the listed work needs multiple concepts/commits, create explicit N28a/N28b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Flood pane A plus timed echo pane B has recorded p50/p95/p99 and no dropped bytes; shutdown/input controls remain serviceable. Idle remains sleeping. No unbounded render queue is introduced.

## Validation

Actual multi-pane trials; readiness/control fairness tests; wakeup race and slow surface/recovery simulations; regression against idle and throughput.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `perf(session): improve measured multi-pane responsiveness (N28)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Do not reorder accepted paste/key bytes or disable rendering to meet a latency goal.

