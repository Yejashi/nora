# Runtime audit, 2026-10-15

Scope: the whole repository at `803e64c`, read against the real windowed
path. Method: four independent static reviews (window/input, rendering,
PTY/engine/concurrency, configuration), a RustSec audit of `Cargo.lock`,
and end-to-end runs of the release binary in a real X11 window (Xvfb,
lavapipe) driven with xdotool and checked by screenshot.

**CI.** First fully green run: `44cfa9d` (run 38799086818), including the end-to-end window job.

**Headline.** The v0.1 acceptance in `docs/13-v0.1-acceptance.md` was
established through library-level tests and a smoke cycle that draws an
empty frame. The interactive window path was not exercised, and it had
critical defects: default text rendered black on black, every fill was
white, the child ran on a raw tty and never learned its size, common
shell keys were hijacked, mouse reporting was malformed, and the config
file had no effect. The v0.1 acceptance claim is withdrawn until the
milestones in [14-production-roadmap.md](../14-production-roadmap.md)
re-establish it with end-to-end evidence.

Status legend: CONFIRMED follows from code or was observed at runtime;
SUSPECTED depends on behaviour not yet observed. IDs are referenced by the
roadmap and by commits. "Fixed" entries cite the commit.

## Fixed in this audit round

| ID | Defect | Commit |
| --- | --- | --- |
| R2 | No base palette: default fg resolved to black | `994181d` |
| R1 | Fill shader ignored vertex color (all fills white) | `994181d` |
| R9a | Fills not linearized for sRGB targets; clear color ignored theme/OSC 11 | `994181d` |
| X1 | IME commit text discarded (typing lost on Wayland text-input) | `4756819` |
| X2 | Vertex buffers lacked COPY_DST (panic on first selection) | `4756819` |
| X3 | No cursor, no cell backgrounds drawn | `4756819` |
| P1 | Resize never reached the child; resize injected `CSI 4;h;w t` as input | M1.2 (this commit series) |
| P2 | Child started on a raw tty (no echo, ISIG, ONLCR, ICRNL) | M1.2 |
| P7 | Headless run killed the child after 5 s; replies not forwarded while waiting | M1.2 |
| I-F1 | Bare Ctrl+T/W/D/X/Tab/arrows and Ctrl+F hijacked from the shell | M1.3 |
| I-F2 | Closing the last pane left a frozen window | M1.3 |
| I-F5 | Kitty encoder emitted raw key codes | M1.3 (removed) |
| I-F7 | Held shortcut chords leaked control bytes | M1.3 |
| I-F14, F15 | Legacy encodings: modifiers, Shift+Tab, Alt prefixes, Ctrl C0 set, keypad DECKPAM | M1.3 |
| I-F17 (keys) | Shift+PageUp/PageDown/Home/End did not scroll | M1.3 |
| P-lost | Fast-exiting child's final output dropped under lock contention (vendored `pty_read`) | `18901e8` |
| I-F4, F12 | Mouse: SGR format, 1002/1003 swapped, 1000 lost releases, no legacy/UTF-8 encodings, wrong modifier bits, Back/Forward codes | M1.4 |
| I-F16 | Click selection while the app reports the mouse; one-cell highlight stuck | M1.4 |
| I-F17 (wheel) | No alternate scroll in the alternate screen | M1.4 |
| I-F9 | No RedrawRequested handling: no frame on expose, zoom, search, tab/pane switch | M1.5 |
| R7 | Surface recovery: Suboptimal loop, retry budget never reset, Lost not recreated, device loss only logged | M1.5 |
| R8 | Glyph atlas never trimmed (unbounded growth) | M1.5 (trim per frame; soak test pending in M3) |
| I-F16 (late) | Selection changes reached the screen only with the next output | M1.5 |
| P13 | Poisoned-mutex unwraps on the snapshot slot | M1.5 |
| R3, I-F13 | Scale factor applied to glyph size only; positions logical on a physical surface | M1.6 |
| R4, I-F8 | Zoom changed glyph size only: no cell resize, no PTY resize, no redraw | M1.6 |
| R5, CFG font | `font.family`/`font.size` ignored; cell size hardcoded (20 px / 24 px); implausible-width assert | M1.6 |
| R11 (bold) | Only the Regular face loaded | M1.6 (bold/italic faces loaded when installed; no synthetic bold) |
| P3 | Unbounded OSC/APC buffering (200 MiB unterminated OSC grew RSS by 798 MiB) | M1.7a (64 KiB OSC / 4 MiB APC caps; +5.5 MiB peak under the same flood) |
| P5 | Pump slept 10 ms per loop: 10-20 ms added to every echo; 100 wakeups/s idle | M1.7b (median key-to-present 12-19 ms -> ~1 ms; idle ~2 wakeups/s), see `docs/performance/2026-10-15-input-latency.md` |
| P4, I-F18 | Input unbounded downstream of the writer (channel, pending queue, loop write list) | M1.7c (2 MiB queue + 1 MiB writer + 1 MiB in flight per pane) |
| P6 | Session close stalled the pump up to 5 s per pane; a SIGHUP-ignoring child leaked the loop thread, process and fds | M1.7d (close off the pump thread; group SIGKILL after 1.5 s) |
| P9 | Recorder overflow could evict the child-exit record (exit status lost) | M1.7d |
| P12 (title, bell) | OSC 0/2 titles only printed to stderr at exit; bell discarded | M1.7d (window title per active pane; bell requests attention when unfocused) |
| R6, I-F3 | Splits froze rendering (stale check against the full window); only the active pane drawn at 0,0; no separators or tab bar | M2.1 |
| M2.1 found | Coalesced pulses acquired a swapchain image and dropped it unpresented; with several panes every acquire then timed out | M2.1 (gather before acquire) |
| M2.1 found | A split lost the previous pane's snapshot slot (it stopped updating) | M2.1 |
| M2.1 found | Tab bar row and tab open/close did not resize other tabs' panes | M2.1 |
| I-F6 | A child exiting in one half of a split closed the whole window (stale tab layout); a dead pane otherwise stayed frozen | M2.2 (exited pane closes; window closes with the last pane of the last tab; `--hold` keeps panes) |
| I-F10 | Search bar never drawn; Space/arrows leaked to the shell while searching | M2.3 (bar with query and n/m; all keys captured) |
| IME (lower) | Preedit never drawn; IME cursor area never set (candidate window misplaced) | M2.4 implemented: preedit drawn underlined at the cursor, cursor area reported every frame. **Unverified with a real input method** (Xvfb has none); needs a manual check under fcitx5/ibus |
| M3 found | Startup 1.16 s: all installed fonts scanned twice per launch (nora's style lookup and cosmic-text's `new_with_fonts`) | M3 (fontconfig lookups; background fallback system; first frame ~180 ms) |
| M3 found | Pump snapshotted under the terminal lock on every wake while the window held an undrawn snapshot | M3 (backpressure; 56 MiB/s windowed) |
| CFG (all) | Config parsed but nothing applied; no reload; scrollback 0 | M4 (schema v2, every key consumed, file watch + Ctrl+Shift+F5, scrollback 10 000 default) |
| R10 | Cursor shape ignored; no hollow cursor when unfocused | M4 (block/beam/underline/hidden; hollow when unfocused) |
| P10 | Child inherited the parent terminal's identity (VTE_VERSION, TMUX, ...); no TERM_PROGRAM; TERMINFO_DIRS ignored | M1.8 |
| P11 | OSC 10/11/12 replies hardcoded white/black; pixel sizes nominal 8x16 | M1.8 (palette replies; real cell pixels, updated on zoom/scale) |
| I-lower | Hyperlink click and prompt navigation advertised but not wired | M1.8 (Ctrl+click; Ctrl+Shift+Z / Ctrl+Shift+G) |
| M3 regression | Redraw loop while scrolled back: empty damage with an off-screen cursor counted as damage, and the post-frame pump wake (M3) turned it into a frame per wake (1703 frames in a short idle run) | fixed (17 frames; e2e `idle` guards it) |
| I-F10 (rest) | Search covered the visible screen only | fixed: pump searches scrollback + screen, scrolls to and selects the match; IME-committed text goes to the search query |
| RV2 | View changes on an idle pane (scroll, search, prompt jump, selection, config) showed ~507 ms late: the pump ticked before applying commands, then slept its idle heartbeat | fixed (re-tick after any command; `NORA_TRACE_LATENCY` key-to-fresh trace; e2e `viewlat`: 507 ms before, 7-9 ms after) |
| RV1 | Redraw loop while partly scrolled back (cursor on screen): the probe's cursor was in screen rows, the damage in viewport rows, so the always-damaged cursor never matched (2026 frames in 3 s idle) | fixed (viewport-mapped probe cursor; scrollback tests; e2e `idle` partly-scrolled leg, 0 frames) |
| RV-OSC7 | `ESC]7;file:///%aé BEL` panicked the PTY reader (the pane died); a trailing `%XX` was never decoded | fixed (byte-level decoder; unit + filter tests) |
| X4 | CI never green: clippy 1.99 lints, Ubuntu font paths, Weston runtime dir, two machine-specific tests | `042621c`, `98f2af1` |

## Dependency audit

`cargo audit` (RustSec, 2026-10-15): no vulnerabilities. Two warnings on
transitive dependencies of glyphon/cosmic-text: `ttf-parser` unmaintained
(RUSTSEC-2026-0192), `lru` unsound `LruCache::pop` panic safety
(RUSTSEC-2026-0253). Action: track upstream; nora does not call `lru`
directly.

## INPUT/WINDOW AUDIT

```text
F1 CRIT bare Ctrl+T/W/D/Shift+D/X/Tab/arrows hijacked as tab/split/focus (mod.rs:1716-1776), no Shift check: Ctrl+D splits, Ctrl+W closes tab, Ctrl+X closes pane.
F2 CRIT closing last tab/pane never exits; frozen window (:825, :881-905).
F3 CRIT only active pane drawn, stale check vs full grid => no redraw after split; no tab bar/borders.
F4 CRIT SGR mouse format "\x1b[<b;x yM" space instead of ';' (mouse.rs:168; test copies it).
F5 CRIT encode_kitty emits raw KeyCode discriminant bytes, no CSI u (input.rs:197-225; flag names wrong engine.rs:105-125).
F6 HIGH tab_layouts stale after split/close (:1395-1399, :844-905): child exit in a split closes window.
F7 HIGH autorepeat of shortcuts falls through to encode (:1576, :1714): holding Ctrl+Shift+C sends ^C.
F8 HIGH zoom only glyph size; no PTY resize; no redraw.
F9 HIGH no RedrawRequested arm; frames only on DirtyPulse.
F10 HIGH search bar never drawn; Ctrl+F stolen; keys leak; visible-screen only.
F11 HIGH config unused; BindingTable::default(); no Ctrl+Shift+F5.
F12 HIGH MOUSE_DRAG/MOUSE_MOTION swapped (mouse.rs:52-57); motion per pixel.
F13 SUSP HIGH scale factor coordinates.
F14 MED legacy encodings: Shift+Tab, Alt+Backspace/Enter, Ctrl+[\]^_, modified arrows/Home/End/F-keys, Ctrl+Alt+letter.
F15 MED keypad: DECCKM switches numpad; keypad Home/End missing.
F16 MED left-click selects even with mouse reporting; ClearSelection never sent; selection viewport coords.
F17 MED Shift+PageUp to PTY; no alternate scroll.
F18 MED PumpMsg channel + pending_input unbounded (main.rs:337, pump.rs:216,512).
Lower: hyperlink click & prompt nav not wired; IME preedit not drawn, no set_ime_cursor_area; no focus out/in on pane switch; --hold no status; window title never set; stray eprintln (zoom, search query text!, config loaded, title); arboard without wayland-data-control; Fonts::load panics; Back/Forward buttons 4/5 should be 128/129.
Verified: IME commit once; copy/paste path; focus reports; wheel scrollback; backgrounds+cursor; basic key encodings; resize; slot lock released before GPU.
```

## RENDER AUDIT

```text
R1 CRIT fills always white: graphics.rs:94-98 shader out.color=vec4f(1.0); layout :118-122 only location 0.
R2 CRIT palette none => default fg black (style.rs:160-170, engine.rs:1234). term.rs assumes white fallback; modules disagree.
R3 HIGH scale factor: positions logical, viewport physical, TextArea.scale enlarges glyphs (graphics.rs:541-548,586-594; mod.rs:141-148).
R4 HIGH zoom changes glyph size only (graphics.rs:512); no pane resize / redraw on zoom (mod.rs:1630-1643).
R5 HIGH FONT_SIZE=20, CELL_H=24 hardcoded; cell_h not from metrics; assert glyphs.rs:168 panics on larger size.
R6 HIGH splits freeze: attempt_render compares active pane snapshot to full-window grid (mod.rs:1152-1165, policy.rs:32) => stale forever; panes drawn at 0,0 (mod.rs:732 drops _x,_y); no borders/tab bar.
R7 HIGH surface recovery: Suboptimal loop (set_size early-return graphics.rs:408); surface_retry never reset (state.rs:146) => 3 Outdated per session exits; Lost not recreated; device lost only logs (mod.rs:1350).
R8 HIGH glyphon atlas never trimmed => AtlasFull eventually, text vanishes.
R9 MED fill colors not linearized for sRGB target (graphics.rs:170-175); clear color constant BG ignores OSC 11.
R10 MED cursor shape (engine.rs:428) ignored; no hollow unfocused; no blink.
R11 MED bold: only Regular loaded; undercurl/dotted/dashed collapse; SGR58 underline color unused.
R12 MED perf: per-cell Buffer::clone (glyph.rs:144), String keys, ~12k TextAreas at 200x60, 4 new GPU buffers/frame, full grid rescan, build_draw_items unused, non_noop hardcoded true (mod.rs:964), last_visible rebuilt per frame.
R13 MED kitty graphics never drawn; drain_graphics no caller in app; a=q no reply.
R14 LOW box drawing no builtin; fallback.rs::first_supporting unused.
Verified OK: zero-size resize, frame drop before reconfigure, submit before present, inverse, faint, hidden, truecolor, wide chars, combining marks, glyph cache bounded 16384.
```

## PTY/ENGINE AUDIT

```text
P1 CRIT resize never reaches child: PtySession::resize (pty.rs:444-466) resizes grid only, no Msg::Resize => no TIOCSWINSZ/SIGWINCH; also enqueues TextAreaSizeRequest -> resolve_record (session/mod.rs:65) writes "\e[4;H;Wt" to child stdin on every resize.
P2 HIGH child starts raw: pty.rs:333-349 sets c_iflag=IUTF8, c_oflag=0, c_lflag=0 on master => no ISIG/ICANON/ECHO/ONLCR/ICRNL. Upstream only sets IUTF8.
P3 HIGH unbounded OSC/APC buffering: PromptMarkerFilter (prompt_marker.rs:556-562), GraphicsFilter APC buffer, vte osc_raw (std). printf '\e]0;'; cat /dev/zero => unbounded RAM, output invisible.
P4 HIGH input not bounded downstream: mpsc (main.rs:337) -> pending_input Vec -> BoundedWriter -> alacritty Msg::Input unbounded -> write_list; flush sink (pty.rs:530) releases budget on channel send.
P5 MED pump sleeps 10ms per loop (pump.rs end of pump_thread) + wait_drain 10ms per draining pane: echo latency 10-20ms; 100 wakeups/s idle.
P6 MED close() on pump thread waits up to 5s, stalls all panes; child ignoring HUP pins thread forever (detached, fd leak).
P7 MED headless join() times out at 5s: `nora -- sleep 10` headless exits after 5s code 1.
P8 MED full-grid snapshot under FairMutex every tick; filter runs under lock in pty_read (event_loop.rs:206-238); per-chunk to_vec.
P9 LOW/MED Recorder drops oldest past 256 records/1MiB silently; can lose PtyWrite replies & ChildExit.
P10 LOW env: only TERM/COLORTERM; no TERM_PROGRAM; inherited VTE_VERSION/TERM_PROGRAM/KITTY_WINDOW_ID not removed; WINDOWID wrong; resolve_term ignores TERMINFO_DIRS.
P11 LOW pixel sizes nominal 8x16 (session/mod.rs:38-43); OSC 10/11/12 replies hardcoded (mod.rs:71-88); DA2 alacritty version; XTVERSION unanswered.
P12 LOW title never set on window; bell dropped; graphics/marker/OSC7/hyperlink queues no consumer in app; kitty kbd stack never pushed.
P13 LOW poisoned-mutex unwrap pump.rs:349,391, app/mod.rs:1135.
P14 SUSP filters end OSC/APC on 0x9C (UTF-8 continuation byte).
Vendor diff: Filter trait + with_filter + process under lock; grid total_scrolled/total_pulled counters.
Verified: OSC52 disabled; hyperlink opener allowlist argv-only; no cmd/file triggers; lock order; partial writes/EIO; CLOEXEC; setsid+TIOCSCTTY; zero-size resize; coalesced slot; in-order bytes.
```

## CONFIG AUDIT

```text
- All schema keys UNUSED by windowed app: terminal.history (live Engine::new at session/pty.rs:308 => 0 scrollback; with_history only in bench), terminal.mouse, font.family (MONO_FAMILY glyphs.rs:20), font.size (FONT_SIZE=20, CELL_H=24 glyphs.rs:14,17), theme.* (BG graphics.rs:191, DEFAULT_BG term.rs:73), keys.bindings (hardcoded app/mod.rs:1574-1642, 1695-1730).
- Suspected: no default 256 palette; Snapshot.colors = Term::colors() (engine.rs:1234) None unless OSC set; term.rs:82 falls to DEFAULT_FG; style.rs:166 to slot 256/257 then black => ANSI colors may not render.
- Reload: --reload re-reads at startup only (main.rs:150-175); Ctrl+Shift+F5 not bound; ConfigState/classify_change have no live caller.
- Docs: 02-product.md:30 says nora.toml (code: config.toml); scrollback README 1000 vs actual 0.
- Validation: deny_unknown_fields ok; NaN font size passes; colors length unchecked; bindings unvalidated; --check-config error branch dead (main.rs:127-140); font.size range message misleading.
- Recommended order: wire existing keys; Alacritty-shaped colors + built 256 palette; live reload Ctrl+Shift+F5; typed bindings enum; window.padding/dimensions/decorations; cursor.style/blink, hide_when_typing, scrolling.multiplier; includes later.
```


## Open: independent review of f67ce22^..2370d9d (2026-10-15)

Read-only review of the multi-pane, performance, config, cross-platform,
search and redraw-loop commits. Not yet fixed; ordered by severity.

| ID | Sev | Finding | Fix direction |
| --- | --- | --- | --- |
| RV3 | medium | `search_text` scans the whole scrollback under the terminal lock on every keystroke and every Enter (stalls the reader and the pump with a large history) | Copy lines in chunks releasing the lock, or cache hits per query |
| RV4 | medium | With more than 10 000 hits, "newest match" is the 10 000th oldest | Scan bottom-up or keep the last N |
| RV5 | medium | A config color change does not repaint idle panes (retained snapshots hold already-overlaid colors; `Configure` forces no snapshot) | Retain raw colors, or force a snapshot of every pane on `Configure` |
| RV6 | medium | Splitting a 1-row pane, or while minimized, gives a zero-size pane with no session; it shares the previous pane's slot; same for a new tab with no pane area | Always give the new pane a fresh slot; spawn its session when its size becomes non-zero |
| RV7 | low | A font reload while the background fallback loader runs can drop the fallback fonts | Tag `FontsReady` with a generation |
| RV8 | low | While searching, Ctrl+Shift+V pastes into the shell; unbound Ctrl/Alt chords append letters to the query | Route paste to the query; ignore Ctrl/Alt chords in search |
| RV9 | low (suspected) | Search positions go stale when output arrives during a search | Store hits by absolute line, resolve under one lock |
| RV10 | low | A match ending in a wide char highlights only its left half; a preedit can overlap a wide glyph starting one column earlier | Extend `end` over the spacer; clear the overlapped glyph |

Also noted: a legacy `[keys]` binding with an unknown action now rejects
the whole config (previously ignored).
