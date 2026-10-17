#!/usr/bin/env bash
# Native Wayland clipboard check (ADR-016) under a headless compositor.
#
# Starts a headless kwin_wayland (virtual backend: a keyboard seat and
# ext-data-control, so wl-copy/wl-paste need no window) on a private
# session bus, with no X11 display and no XWayland, and runs the ignored
# `tests/wayland_clipboard.rs` against it. Headless, it checks that nora
# pastes another client's selection natively. The copy checks (nora pastes
# its own copy, another client reads it) need the serial of a real key
# press, which a headless compositor cannot produce: they are skipped
# here. Run them in a desktop Wayland session (no X11 fallback involved)
# and press a key in the probe window when asked:
#   NORA_WAYLAND_COPY_CHECK=1 cargo test --test wayland_clipboard -- --ignored --nocapture
#
# Usage: scripts/wayland-clipboard-check.sh
# Needs: kwin_wayland, dbus-run-session, wl-copy/wl-paste (wl-clipboard),
#        a Vulkan or GL driver for wgpu.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
for tool in kwin_wayland dbus-run-session wl-copy wl-paste; do
    command -v "$tool" >/dev/null || { echo "wayland-clipboard: missing $tool" >&2; exit 2; }
done

if [ "${1:-}" != "--inner" ]; then
    # Build first, outside the compositor's lifetime.
    (cd "$ROOT" && cargo test --quiet --no-run --test wayland_clipboard) || exit 1
    exec timeout 120 dbus-run-session -- "$0" --inner
fi

OUT=$ROOT/target/wayland-clipboard
mkdir -p "$OUT"
SOCKET=nora-clipboard-$$
unset DISPLAY WAYLAND_DISPLAY
kwin_wayland --virtual --no-lockscreen --no-global-shortcuts \
    --socket "$SOCKET" --width 800 --height 600 >"$OUT/kwin.log" 2>&1 &
KWIN_PID=$!
trap 'kill $KWIN_PID 2>/dev/null' EXIT
for _ in $(seq 1 100); do
    [ -S "${XDG_RUNTIME_DIR:?}/$SOCKET" ] && break
    sleep 0.1
done
[ -S "$XDG_RUNTIME_DIR/$SOCKET" ] || { echo "wayland-clipboard: compositor did not start" >&2; exit 1; }
export WAYLAND_DISPLAY=$SOCKET

cd "$ROOT" && timeout 60 cargo test --quiet --test wayland_clipboard -- --ignored --nocapture
