# Acceptance status (re-established 2026-10-15, updated 2026-10-16)

The original v0.1 acceptance (docs/13-v0.1-acceptance.md) was withdrawn
by the runtime audit: the interactive window had never been exercised.
This record re-establishes acceptance per roadmap milestone
(docs/14-production-roadmap.md) from end-to-end evidence. Statuses:
**accepted** (acceptance criteria met, evidence in CI or recorded),
**partial** (some criteria open, listed), **open**.

Evidence sources: `scripts/e2e-x11.sh` (29 scenarios in a real X11 window
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
| M1.5 frames | accepted | e2e `redraw`, `selection`, `idle` (fully and partly scrolled back: 0 frames idle), `viewlat` (view change on an idle pane 7-9 ms), `atlas` (about 32 000 distinct CJK/Hangul glyphs in two passes at two zoom levels: no failed frame, text visible, second pass +5-6 MiB; +144 MiB with the trim removed) | — |
| M1.6 fonts and metrics | accepted | e2e `zoom`, `scale` (forced X11 scale 1, 1.25, 2: grid 24x80 at each, window and text area scale with the factor; fixed 2026-10-17: at 1.25 the window was sized from scale-1 cells and lost a row), metric tests | — |
| M1.7 bounds and latency | accepted | e2e `flood` (+5.5 MiB under a 200 MiB OSC flood), input-bound test, close escalation test; key-to-present ~1 ms | — |
| M1.8 reports and environment | accepted | e2e `reports`, `hyperlink`, `prompts` (Ctrl+Shift+Z puts the previous prompt on the top row and stops at the first, Ctrl+Shift+G selects the last output; fixed 2026-10-17: markers never reached the store in the window, the jump targeted the current prompt, and a full store refused new markers); OSC 7 decoder byte-safe (RV-OSC7) | — |
| M2 multi-pane UI | accepted | e2e `panes`, `tinysplit`, `paneexit`, `search` (scrollback, chords), `title`; review RV3, RV4, RV6, RV8-RV10 fixed | candidate-window placement not verified: nora reports the cursor area (`set_ime_cursor_area`), but the e2e engine (ibus simple) shows no candidates; preedit and commit are verified by e2e `ime` |
| M3 performance | accepted (baseline recorded) | docs/performance/2026-10-15-baseline.md, input latency record, docs/performance/2026-10-17-frame-allocations.md (full-screen frame 25 596 -> 181 allocations, `alloc-count` measurement build) | window-path throughput ~56 MiB/s vs ~110 MiB/s headless (audit P8) |
| M4 customization | accepted | e2e `config` (startup, live reload incl. palette repaint of an idle pane, invalid edit), schema tests; RV5, RV7 fixed | — |
| M5 cross-platform | deferred (Linux only for now; partial) | Linux X11/Wayland verified; Windows: build + headless tests green in CI; macOS: build, unit, platform-neutral and PTY tests green in CI (first full green run `80047545`) | `cwd_is_honored` failed once on macOS (suspected flaky; panic messages now reach CI annotations); macOS/Windows interactive use untested; no macOS/Windows packaging |
| M6 autocomplete | accepted | `tests/shell_input.rs`, `tests/suggest.rs`, PTY test; e2e `zshell` (buffer follows typing, completion, kill/yank, history recall), `suggest` (ghost text, Right runs exactly the suggestion, path completion, history saved on SIGTERM); `ime` (real ibus over XIM: preedit drawn, ghost text hidden while composing, commit reaches the shell once); password-input check (`password_input_follows_the_child_line_discipline`, `pty_zsh_editing_is_not_password_input`); docs/performance/2026-10-16-suggestions.md | zsh only, by design (ADR-013): fish and bash with ble.sh draw their own autosuggestions, plain bash cannot report its editor buffer |

Real applications exercised end to end: vim (insert, Escape, `:wq`), tmux
(status line and pane), htop (colors and meters).

What "accepted" does not claim: hardware coverage beyond one AMD GPU
(RADV) and Mesa lavapipe; compositors beyond GNOME and Weston; a manual
accessibility review.
