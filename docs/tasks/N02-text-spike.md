# N02: Prove fixed-cell text placement

Implementation status: N02a `validated` (headless per-cell tests in `tests/text_placement.rs`, 9/9); N02b not started. This card is a plan, not evidence of completion.

- Dependencies: N01.
- Read: [05-rendering.md](../05-rendering.md), [11-risks.md](../11-risks.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Choose the smallest text adapter that can draw terminal cells without layout drift.

## Small implementation increment

Render a static tiny cell grid with ASCII, e+combining accent, CJK wide cells, box drawing and Ethiopic fallback. Pin test font/provenance. Exercise multiple scales. Test glyphon reuse/placement/cache accounting. If it fails, write an ADR and spike cosmic raster output with positioned quads; keep only the accepted adapter.

If the listed work needs multiple concepts/commits, create explicit N02a/N02b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Cell starts/cursor positions match known columns; wide/combining spans remain fixed; unchanged cells avoid repeated font/shaping work. Candidate cache limits have an executable strategy. The accepted adapter and unverified cases are recorded.

## Validation

Numerical cell-position tests plus an offscreen or desktop image. Tiny-cache stress if API permits. Record actual dependency/scale/font evidence.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `chore(render): validate fixed-cell text adapter (N02)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

A visually nice proportional whole-row buffer is not a passing result. Split placement and cache experiments if needed.

