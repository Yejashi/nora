# N19: Transactional config reload and zoom

Implementation status: not_started. This card is a plan, not evidence of completion.

- Dependencies: N18.
- Read: [03-architecture.md](../03-architecture.md), [05-rendering.md](../05-rendering.md); [agent workflow](../08-agent-workflow.md).

## Observable goal

Apply local UI configuration changes without corrupting sessions.

## Small implementation increment

Implement explicit reload shortcut first; classify changes into immediate UI, staged font/geometry and new-session launch settings. Validate font replacement before swap. Add zoom/reset and cache/font epoch changes. A file watcher is optional after the explicit path is proven.

If the listed work needs multiple concepts/commits, create explicit N19a/N19b slices with their own acceptance; finish, validate, commit and push each before the next.

## Acceptance

Bad reload keeps the last valid config. Color/keys update; font/zoom changes align renderer and PTY grids. Existing shell/env remains unchanged while new sessions receive new launch settings. Stale background results are ignored.

## Validation

Reload rollback/epoch/unit tests; live font/scale/zoom during TUI/output; inspect old glyph handle invalidation.

Also run relevant formatting/Clippy/build checks. Record commands actually run and manual checks still pending.

## Completion and Git

Update progress/behavior notes; inspect and explicitly stage the feature diff; commit (suggested message: `feat(config): reload settings transactionally (N19)`); push to the configured remote branch; verify remote state. A failed push blocks the next feature. Do not force-push or claim complete without the push.

## Guardrail

No partial valid/invalid configuration application or implicit shell restart.

