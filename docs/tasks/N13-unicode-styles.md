# N13: Unicode, fallback and text styles

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N12.
- Read: [05-rendering.md](../05-rendering.md), [06-compatibility.md](../06-compatibility.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Complete the renderer's declared v0.1 text policy.

## Small implementation increment

Deliver independent slices for combining/wide/fallback; bold/italic/faint/reverse/conceal; underline/strikeout; emoji format if supported. Keep engine span authoritative, glyph caches epoch-keyed and copy text untouched. Document complex grapheme/RTL limitations rather than inventing widths.

If the listed work needs multiple concepts/commits, create explicit N13a/N13b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

CJK, e+accent, box drawing and Ethiopic fallback align with cursor/columns at tested scales. Style images are correct. Tiny atlas eviction cannot corrupt retained cells. Supported/limited text cases appear in compatibility docs.

## Validation

Geometry assertions/offscreen images with pinned font; deliberate cache eviction; native DPI/font samples. Each useful slice gets its own commit/push.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(render): render fixed-cell Unicode and styles (N13)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

Never reshape all unchanged content or silently turn on cross-cell ligatures.

