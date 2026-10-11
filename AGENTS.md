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

## Parallel work with orchestrate-tellico

These rules apply when the session runs as `orchestrate-tellico-0`/`-1` with `tellico-worker-0`/`-1`. In `--solo` sessions skip this section; the rest of this file still applies.

### Roles

- The lead owns the plan, slicing, shared decisions, `docs/status/PROGRESS.md`, diff inspection and the final report. It may read, grep and edit; its shell is limited to read-only `git`. It therefore never runs `cargo`, `git add`, `git commit` or `git push` itself: it dispatches them.
- Workers execute one bounded brief each inside an explicit write scope and report compactly. Workers never edit `PROGRESS.md`, never change the roadmap, never pick a new card and never commit or push unless the brief is a commit brief.
- Only the lead decides anything two briefs both depend on: module and file names, public signatures, error types, test fixture layout, channel types. State the decision verbatim in every brief that touches it. A worker that must choose takes the narrowest reading and flags it with `## Note:`; the lead reconciles before commit.

### Where parallelism comes from

The roadmap is a dependency chain, so two cards are almost never implemented at once. Parallelize inside the current card instead. Legal pairs, in order of preference:

1. **Production slice + test slice** of the same card, in disjoint files: one worker implements `src/…` against a signature the lead fixed in both briefs; the other writes the regression tests in `tests/…` against that same signature. Integration is the lead compiling the two halves together through one worker.
2. **Implementation + read-only readiness spike** for the next card or slice: inspect the pinned crate source under `~/.cargo/registry` for the public APIs the next card needs, and report paths, signatures and gaps. No edits.
3. **Commit round + readiness spike**: while one worker runs the full check set and commits/pushes, the other does the spike for the following slice.
4. **Implementation + independent review** of the previous increment's diff for correctness, bounds and rule violations. Read-only.
5. **Code slice + documentation slice**: behavior docs, compatibility matrix rows or an ADR draft in `docs/`, with the code owner given exclusive rights to `src/` and `tests/`.

Never run two write scopes that share a file, two workers that both compile, or two workers that both touch git. Do not split a card into fragments smaller than one coherent file set to manufacture a second task; a single implementation plus a read-only spike is the normal round.

### Cargo and the shared worktree

- `target/` is shared and cargo takes a build-directory lock, so two concurrent `cargo` invocations serialize and waste a node. Give exactly one worker per round the right to run `cargo`; the other brief says "do not run cargo". If a second compiling worker is unavoidable, its brief sets `CARGO_TARGET_DIR=target/w1` (gitignored under `/target`) and accepts the cold build.
- The compiling worker runs the checks in the order the Validation section lists, with `--quiet`/`--message-format short`, and reports the first failures only.
- A test slice written without compiling is accepted only after the integrating worker compiles and runs it on the combined tree. The lead does not report a check as passing on a worker's word: it reads the worker's exact command and result, and for the final round has the commit worker rerun the full set.

### Round structure

1. Lead re-reads `PROGRESS.md`, the current card and `git status --short`. Lead writes the slice plan into `PROGRESS.md` under "Active feature": slices, owner node, write scope, acceptance, and the shared signatures.
2. Lead dispatches the two briefs in one batch with `background: true`, the first unit to the far node's worker. Every brief carries the six elements from the orchestrate prompt plus: the exact write scope, "run cargo"/"do not run cargo", the shared signatures, and the budget in files and checks.
3. On each report the lead inspects `git diff --stat` and the changed hunks it needs to judge, reconciles any `## Note:` items by editing or redispatching, and immediately refills the free node with the next legal unit.
4. Integration: one worker gets a brief to compile the combined tree, run fmt, clippy `-D warnings`, build and the relevant tests, and fix only compile/lint errors inside both scopes. The lead updates `PROGRESS.md` and behavior docs itself while that runs.
5. Commit round: one worker, one brief, explicit file list, commit message with feature ID, `git push`, then `git ls-remote --heads origin main` compared to `git rev-parse HEAD`, reported verbatim. The lead confirms with `git log -1` and `git status --short`. Untracked or modified files outside the increment are never staged.
6. Only after the push is verified does the lead open the next card or slice. A failed push blocks the next feature exactly as in the solo workflow.

### Blockers and handoff

- A worker `## Blocker:` on a shared decision is answered by the lead from the repository and redispatched; it is not forwarded to the other worker.
- Before context gets crowded (the lead compacts around 90k), the lead records in `PROGRESS.md` under "Active feature": which slices are dispatched, which reported, which scopes are dirty in the worktree, and whether the tree is uncommitted, committed/unpushed or committed/pushed. After compaction the lead re-reads `PROGRESS.md`, then `git status --short`, before dispatching anything.
- A worker report that claims a manual GPU/Wayland test passed without a command and its output is recorded as unverified.
