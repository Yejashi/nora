# N26: Rust benchmarks and structured diagnostics

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N25.
- Read: [04-performance.md](../04-performance.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Create reproducible measurements that distinguish processing from presentation.

## Small implementation increment

Add nora-dev corpus generator and headless engine benchmark first, then real PTY completion handshake, then renderer/input instrumentation. Each is a separate validated tool increment. Include metadata/schema/raw samples and state correctness. Diagnostics omit user input/output by default.

If the listed work needs multiple concepts/commits, create explicit N26a/N26b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

P01–P10 have documented runnable subsets; parser/PTY/render timings are labeled; DSR processing completion is not claimed as visible presentation. Instrumented echo detects a submitted frame containing its tagged marker. Results include build/machine/font/backend metadata.

## Validation

Benchmark harness self-checks with known corpus bytes/state; PTY timeout/error tests; instrumented pipeline ordering; run smoke trials and mark absent hardware results pending.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(dev): add reproducible terminal benchmarks (N26)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No predicted speedup or parser-only number presented as real user latency.

