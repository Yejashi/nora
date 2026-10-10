# nora implementation progress

Plan created: 10 October 2026. Application implementation has not started. No build, compatibility, hardware or performance test has been run for nora.

## Current state

- Active feature: none.
- Next task: `N00-bootstrap`.
- Accepted default: Rust + alacritty_terminal + winit + wgpu; text adapter subject to N02 gate.
- Target: Linux native Wayland and X11.
- Git remote/branch/authentication: not known to this document; inspect/configure from the owner's repository.
- Required workflow: validate → update notes → commit → push → verify, for every completed feature change.

## Completed implementation increments

None. The existence of this guide does not count as implementing any N task.

When recording a task, use: ID; observable behavior; validation commands/results; remaining manual release checks. Status values: `not_started`, `in_progress`, `validated`, `blocked`. Overall completion additionally requires the post-commit push verification.

## Blockers and limitations

- Dependency tuple not compiled/pinned.
- Fixed-cell text adapter not validated.
- End-to-end session write/event limits not implemented.
- No native display, protocol or performance evidence yet.

## Last verified Git state

None recorded. Record a previous feature's hash/remote verification only after it exists. Never fabricate the hash of the commit currently being authored.

## Next action

Read `AGENTS.md`, `docs/08-agent-workflow.md` and `docs/tasks/N00-bootstrap.md`; inspect repository/Git state and create the minimal build scaffold.
