# N33: Bounded inline images and placement

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N32.
- Read: [13-extensions.md](../13-extensions.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Implement a deliberately narrow, correctly advertised graphics subset.

## Small implementation increment

Separate inline upload/decode and visual placement/delete into useful increments. Validate base64/size/IDs with checked allocation arithmetic; enforce per-image/total budgets. Draw images with clipping and a tested scroll/alternate-screen policy. Maintain a capability table and neutral Rust protocol client.

If the listed work needs multiple concepts/commits, create explicit N33a/N33b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Supported uploads get accurate replies; malformed/oversize payloads fail safely; placement/delete/resize/scroll behave as documented; image cache resources are released. Unsupported capabilities are not advertised as full protocol support.

## Validation

Protocol positive/negative fixtures, streaming limits, decompression-size checks if adding PNG, offscreen placement and live compatible-client tests.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(graphics): add bounded inline image subset (N33)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

A single working image is a subset, not full kitty graphics compatibility.

