# ADR-016: Native Wayland clipboard through smithay-clipboard

- **Status**: accepted
- **Date**: 2026-10-17
- **Builds on**: N15 (clipboard backend), `docs/06-compatibility.md`
  "Native clipboard and IME"

## Problem and evidence

nora's clipboard was `arboard` 3.6, which on Linux speaks only X11 (its
Wayland data-control support is an opt-in feature, and data-control is a
privileged protocol not meant for ordinary clients). In a native Wayland
session copy and paste therefore went through XWayland, and there was no
clipboard without it: with `DISPLAY` empty, `arboard::Clipboard::new`
fails ("X11 server connection timed out because it was unreachable").
The compatibility contract says not to start an X11 clipboard path quietly
in a native Wayland session and to prefer `smithay-clipboard` after
verifying it against the pinned stack.

## Options

1. **`smithay-clipboard` 0.7.3** (latest): depends on
   smithay-client-toolkit 0.20. winit 0.30.13 pins sctk 0.19.2, so the
   lock would carry two sctk stacks.
2. **`smithay-clipboard` 0.7.2**: the same API and protocol code on
   sctk 0.19.2 (`calloop` feature, no default features) and
   wayland-backend 0.3 (`client_system`), both already in the lock
   through winit.
3. **A clipboard on winit's own sctk objects**: winit does not expose its
   seat, data device or event queue, so this would mean a second
   connection or patching winit.

## Decision

Option 2, pinned `=0.7.2`, built only with the `wayland` feature. It adds
one crate to the lock (`smithay-clipboard`) and no duplicate:
`cargo tree -d` is unchanged.

- **Backend choice** (`clipboard_kind`): the window's raw display handle
  decides. A Wayland handle gets the smithay backend on the window's own
  `wl_display`; Xlib/Xcb (and any other kind) get `arboard`. A Wayland
  handle never falls back to `arboard`: without the backend in the build
  it gets no clipboard.
- **Threads**: each `smithay_clipboard::Clipboard` runs a worker thread
  with its own event queue and its own seat devices (it tracks focus and
  input serials itself). `load` blocks on a reply channel with no timeout,
  until the selection owner closes its pipe; `store` only queues a
  request. nora creates two instances: one for copies (`store` from the UI
  thread, never blocks) and one for pastes behind a `Mutex` taken with
  `try_lock` from the paste thread. A read stuck on its owner (abandoned
  by `PasteGate` after `PASTE_READ_TIMEOUT`) makes later pastes fail at
  once instead of queueing a thread each; replies stay matched to their
  request because loads are serialized on that instance.
- **Lifetime** (`unsafe`): `Clipboard::new` requires the `wl_display` to
  outlive it. winit's Wayland event loop owns the connection
  (`Connection::connect_to_env`), and wayland-backend disconnects the
  display when the last clone is dropped (the event loop and each window
  hold one). A paste thread stuck in `load` keeps the backend alive past
  the window, so `app::run_windowed` holds the event loop in
  `ManuallyDrop` and runs it with `run_app_on_demand` (what `run_app`
  does on Linux, without consuming the loop). With a Wayland clipboard
  the loop is never dropped, also not during an unwind, so the display
  stays connected until the process exits; on X11 it is dropped after
  the app as before. `create_clipboard` is an `unsafe fn` carrying this
  contract.

## Consequences

- Text only: smithay-clipboard offers and accepts
  `text/plain;charset=utf-8`, `UTF8_STRING` and `text/plain` (CRLF/CR
  normalized to LF), on the last seat that delivered a key or button
  event. PRIMARY selection is not used.
- A copy needs keyboard focus and the serial of a recent input event;
  without them the compositor (or the worker) drops it silently, which a
  real copy (button release, copy key) always has.
- A paste reads the whole offer before nora's paste cap applies, as with
  `arboard`; the read is off the UI thread and abandoned after 1 s.
- Copied text lives in nora: after nora exits the selection is gone
  unless a clipboard manager took it, as for any Wayland client.

## Validation and follow-up

- Unit tests (`src/clipboard.rs`): Wayland/X11 backend choice from the
  handle kind; a read behind a stuck read fails at once and the late
  reply reaches its own read; a panicked read leaves the reader usable.
- `scripts/wayland-clipboard-check.sh` runs `tests/wayland_clipboard.rs`
  (ignored by default) on headless `kwin_wayland --virtual` with no X11
  display and no XWayland: nora's backend pastes a selection owned by
  `wl-copy` (passed in this environment). nora's own copy (paste it back,
  and `wl-paste` reading it) needs a key press for a valid serial, which
  the headless compositor cannot inject (no virtual-keyboard protocol);
  KWin cancelled a selection set with the modifiers serial. Those checks
  run with `NORA_WAYLAND_COPY_CHECK=1` in a desktop session and are
  **unverified**.
- The release binary starts on the same headless compositor with no X11
  display and exits with its child's status.
- Upgrading winit to an sctk 0.20 release should move to
  smithay-clipboard 0.7.3 or later in the same change.
