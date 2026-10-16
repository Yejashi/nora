# Acceptance status (re-established 2026-10-15)

The original v0.1 acceptance (docs/13-v0.1-acceptance.md) was withdrawn
by the runtime audit: the interactive window had never been exercised.
This record re-establishes acceptance per roadmap milestone
(docs/14-production-roadmap.md) from end-to-end evidence. Statuses:
**accepted** (acceptance criteria met, evidence in CI or recorded),
**partial** (some criteria open, listed), **open**.

Evidence sources: `scripts/e2e-x11.sh` (22 scenarios in a real X11 window
with real key and mouse events, run in CI job `e2e-x11`), the test suite
(about 600 tests), `scripts/perf.sh`, and the audit table of fixes
(docs/audit/2026-10-15-runtime-audit.md).

| Milestone | Status | Evidence | Open |
| --- | --- | --- | --- |
| M0 evidence harness | accepted | e2e-x11 CI job; mutation checks show the redraw, selection, flood and input-bound scenarios fail without their fixes | — |
| M1.1 colors | accepted | e2e `render`, palette tests | — |
| M1.2 PTY | accepted | e2e `tty`, `resize`; headless exit status; PTY tests | — |
| M1.3 keyboard | accepted | e2e `keys` (byte-exact), encoder tests | kitty keyboard protocol unsupported (documented) |
| M1.4 mouse | accepted | e2e `mouse` (byte-exact SGR), encoder tests | — |
| M1.5 frames | partial | e2e `redraw`, `selection`, `idle` | review RV1 (redraw loop when partly scrolled back), RV2 (idle view changes up to 500 ms late); atlas trim not soak-tested |
| M1.6 fonts and metrics | accepted | e2e `zoom`, metric tests | fractional scale verified by unit geometry only |
| M1.7 bounds and latency | accepted | e2e `flood` (+5.5 MiB under a 200 MiB OSC flood), input-bound test, close escalation test; key-to-present ~1 ms | — |
| M1.8 reports and environment | partial (RV-OSC7 crash) | e2e `reports`, `hyperlink` | prompt navigation has unit tests only (needs the bash integration in e2e) |
| M2 multi-pane UI | partial | e2e `panes`, `paneexit`, `search` (scrollback), `title` | review RV3-RV6, RV8-RV10; IME preedit and candidate placement unverified with a real input method |
| M3 performance | accepted (baseline recorded) | docs/performance/2026-10-15-baseline.md, input latency record | window-path throughput ~56 MiB/s vs ~110 MiB/s headless (snapshot under the terminal lock, audit P8); per-frame allocations not measured |
| M4 customization | accepted | e2e `config` (startup + live reload + invalid edit), schema tests | review RV5 (idle panes keep old colors after a reload), RV7 |
| M5 cross-platform | partial | Linux X11/Wayland verified; Windows: build + headless tests green in CI; macOS: build, unit and platform-neutral tests green | macOS PTY test fixes pushed, CI result not yet confirmed; macOS/Windows interactive use untested; no macOS/Windows packaging |
| M6 autocomplete | design accepted (ADR-013) | — | implementation gated on M5 |

Real applications exercised end to end: vim (insert, Escape, `:wq`), tmux
(status line and pane), htop (colors and meters).

What "accepted" does not claim: hardware coverage beyond one AMD GPU
(RADV) and Mesa lavapipe; compositors beyond GNOME and Weston; a manual
accessibility review.
