# N31: Daily-use v0.1 release assessment

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N30.
- Read: [02-product.md](../02-product.md), [04-performance.md](../04-performance.md), [09-validation.md](../09-validation.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Assess the full v0.1 contract with evidence and a clear release decision.

## Small implementation increment

Run the application matrix and P01–P10 on named hardware/backends. Resolve regressions as individual feature/fix increments. Produce compatibility, performance, resources and known-limitations reports. Update README to describe implemented behavior. Keep this card as an assessment, not one giant fix commit.

If the listed work needs multiple concepts/commits, create explicit N31a/N31b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

All v0.1 requirements pass or the report says precisely why release is blocked. Native Wayland/X11, clipboard/IME, tabs/splits, resource limits and actual performance evidence are present. Every code feature has its own pushed commit.

## Validation

Full relevant check suite; manual real-app/backend/GPU matrix; raw benchmark results with correctness/completion; clean install and repeated lifecycle stress.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `docs(release): assess nora v0.1 acceptance (N31)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Code committed/pushed is not a claim that unavailable hardware checks passed.

