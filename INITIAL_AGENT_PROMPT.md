# Initial message for the local coding agent

You are implementing **nora**, a fast, efficient, maintainable Linux desktop terminal in Rust, with no Python dependency in the application or project-owned build and development tools. This repository contains the researched implementation guide. Read `AGENTS.md`, `README.md`, `docs/08-agent-workflow.md`, `docs/status/PROGRESS.md`, and `docs/tasks/N00-bootstrap.md` first; then read only the documents and source files needed for that increment. Follow the task dependencies, verify actual dependency APIs with compiling spikes, reuse the existing terminal engine, and do not invent untested performance results.

**Durable state.** `docs/status/PROGRESS.md` is your plan file and the only resume index; use it wherever your harness instructions mention `.agent/PLANS.md`, and do not create `.agent/`. Use `docs/status/HANDOFF_TEMPLATE.md` for handoffs.

**Context.** The window is 128k but compaction fires near 90k. Keep task entry small, and update PROGRESS.md with exact state before context gets crowded. After any compaction, re-read PROGRESS.md, the current task card and `git status` before acting.

**Commands.** Keep cargo output short (`--quiet`, `--message-format short`, filter to the first errors). Never run a command that opens a window or waits for input without a `timeout`; prefer headless tests. Record visual or native Wayland/X11 checks as pending manual tests for the owner, never as passed.

**Git.** The guide is already committed and `main` tracks `origin`. For every completed feature change, run its relevant checks, update progress notes, create a clear commit, push it, and verify the push before starting another feature. Preserve user changes, never force-push, and report a missing remote, authentication problem or required hardware test as a specific blocker.

**Scope of this run.** Start with N00 and continue one validated, committed and pushed increment at a time through the Foundation milestone (N00–N04). Then stop and report: per task, the behavior, checks run, commit and push status, and any pending manual checks.
