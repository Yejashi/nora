# Rules for agents implementing nora

These rules express the project owner's requirements. Read them at the start of every session.

## Product and implementation

- The executable and project name are `nora`; Linux is the initial supported platform.
- Write the application in Rust. Project-owned build helpers, tests and benchmark tools use Rust or shell, not Python. Normal native dependencies are acceptable; do not assert that all dependencies are pure Rust.
- Use `alacritty_terminal` as the default terminal engine. Do not create another VT parser or screen model. Change this decision only after a documented failed capability gate and an ADR.
- Native Wayland and X11 are required for v0.1. A successful XWayland run does not validate native Wayland.
- Correct terminal behavior, bounded resource use, low latency and maintainable code have priority over headline throughput.
- Read `docs/02-product.md` for release scope. Kitty-style graphics are a later milestone, not an implied v0.1 promise.
- Do not add a webview, Python configuration interpreter, plugin runtime, custom lock-free collection or handwritten SIMD without a measured need and an ADR.

## Increment size and context

- Work on one task or one independently usable slice of a task at a time.
- Read the current card, relevant reference sections and targeted source files. Do not ingest all docs, source or terminal dumps.
- Aim for a working context of about 8k–24k tokens at task entry. Keep at least 40% of the 128k window available for implementation and debugging. These are operating guidelines, not a claim about model quality.
- Compaction fires near 90k tokens, not 128k. Update `docs/status/PROGRESS.md` with exact state before context gets crowded; after any compaction, re-read it, the current card and `git status` before acting.
- Keep command output short: `cargo --quiet` or `--message-format short`, filtered to the first errors. Never run a command that opens a window or waits for input without a `timeout`; prefer headless tests.
- If a feature spans more than roughly five production files or several new concepts, plan smaller slices first. Test files and mechanical changes may justify exceeding this guide; explain why.
- Commit and push every completed feature change before starting another feature. Do not combine unrelated features into a single commit.
- Also commit and push completed fixes, dependency changes and meaningful documentation increments; use an appropriate commit type.

## Validation and Git

1. Inspect branch, remote, worktree and existing user changes.
2. Define one observable acceptance condition.
3. Implement the increment and its meaningful regression coverage.
4. Run formatting, relevant tests, relevant Clippy checks, and a build when applicable.
5. Update `docs/status/PROGRESS.md` and any changed behavior documentation before committing.
6. Inspect the diff and explicitly stage only the increment's files.
7. Commit, push to the configured remote branch, and verify the remote head contains the commit.
8. Report feature ID, behavior, checks, commit, push status and next task.

No force-push, history rewriting, blanket staging of user changes, destructive reset, credential insertion, or automatic remote invention. A failed push blocks advancing to another feature. Preserve the validated commit, report the cause and resume that push when the environment is fixed. Do not mark a manual GPU/Wayland test as passed when the environment cannot run it.

## Engineering standards

- Pin compatible dependencies, check in Cargo.lock, and use documented public APIs. Verify examples against the pinned versions.
- Keep terminal state out of GPU and platform modules. Keep engine-specific types inside the engine adapter, except the deliberately encapsulated synchronization boundary.
- Never hold the terminal mutex during font work, GPU calls, clipboard access, filesystem work or a blocking send.
- Process all accepted PTY bytes in order. Coalesce redraw notifications and replaceable presentation state, never terminal output or accepted key bytes.
- Bound queues in bytes as well as messages. An unbounded dependency queue remains unbounded even behind a bounded frontend queue.
- Handle child exit, partial writes, resize, zero-sized surfaces and shutdown explicitly.
- Restrict `unsafe` to a documented boundary, with a safety explanation and targeted tests. Prefer existing safe APIs.
- Label targets, measured results, inferred tradeoffs and unverified behavior separately.
- Record a small ADR for a major dependency, renderer or threading change. Do not rewrite the architecture casually mid-feature.
- Maintain compatibility status and resource-limit evidence with the actual implementation.

## Durable state

Use `docs/status/PROGRESS.md` as the resume index and plan file. Keep it short. Where harness instructions name `.agent/PLANS.md`, use PROGRESS.md instead and do not create `.agent/`. It records implementation/validation status; push evidence is reported after the commit and verified again at the next session. Never fabricate a commit hash inside the commit that creates it. A context handoff must state whether the current change is uncommitted, committed/unpushed, or committed/pushed.
