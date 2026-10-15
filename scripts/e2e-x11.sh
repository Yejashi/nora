#!/usr/bin/env bash
# End-to-end checks of the real nora window under a virtual X11 display.
#
# Drives the release binary the way a user does: a real window (Xvfb +
# the system Vulkan driver, lavapipe in CI), real key events (xdotool),
# screenshots (ImageMagick) for rendering assertions, and evidence files
# written by the child shell for PTY assertions.
#
# Usage: scripts/e2e-x11.sh [SCENARIO...]   (default: all)
# Env:   NORA_BIN   binary to test (default target/release/nora)
#        E2E_OUT    artifact directory (default target/e2e)
# Needs: Xvfb, xdotool, ImageMagick (IM6 `convert`/`import` or IM7 `magick`).
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
NORA_BIN=${NORA_BIN:-$ROOT/target/release/nora}
OUT=${E2E_OUT:-$ROOT/target/e2e}
rm -rf "$OUT" && mkdir -p "$OUT"

if command -v magick >/dev/null 2>&1; then
    IM_CONVERT=(magick) IM_IMPORT=(magick import)
else
    IM_CONVERT=(convert) IM_IMPORT=(import)
fi
for tool in Xvfb xdotool; do
    command -v "$tool" >/dev/null || { echo "e2e: missing $tool" >&2; exit 2; }
done

DISPLAY_NUM=${E2E_DISPLAY:-97}
Xvfb ":$DISPLAY_NUM" -screen 0 1024x700x24 -nolisten tcp >"$OUT/xvfb.log" 2>&1 &
XVFB_PID=$!
trap 'kill $XVFB_PID 2>/dev/null' EXIT
export DISPLAY=":$DISPLAY_NUM"
unset WAYLAND_DISPLAY
for _ in $(seq 1 50); do xdotool getmouselocation >/dev/null 2>&1 && break; sleep 0.1; done

FAILED=0
pass() { echo "PASS $1"; }
fail() {
    echo "FAIL $1: $2"
    # GitHub annotation, readable from the public check-run API.
    [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error::e2e $1: $2"
    FAILED=$((FAILED + 1))
}

# launch NAME CMD... : start nora on CMD with an isolated HOME, wait for
# its window and focus it. Sets NORA_PID and WIN.
launch() {
    local name=$1; shift
    local home="$OUT/$name/home"
    mkdir -p "$home"
    HOME="$home" XDG_CONFIG_HOME="$home/.config" PS1='$ ' \
        "$NORA_BIN" -- "$@" >"$OUT/$name/nora.log" 2>&1 &
    NORA_PID=$!
    WIN=""
    for _ in $(seq 1 100); do
        WIN=$(xdotool search --name '^nora$' 2>/dev/null | head -1)
        [ -n "$WIN" ] && break
        sleep 0.1
    done
    [ -n "$WIN" ] || return 1
    sleep 1
    xdotool windowfocus --sync "$WIN" 2>/dev/null
    sleep 0.3
}

# finish: stop nora if it is still running.
finish() {
    kill "$NORA_PID" 2>/dev/null
    for _ in $(seq 1 20); do kill -0 "$NORA_PID" 2>/dev/null || return 0; sleep 0.1; done
    kill -9 "$NORA_PID" 2>/dev/null
}

shot() { sleep 0.5; "${IM_IMPORT[@]}" -window root "$1"; }

# count_color FILE HEX : number of pixels exactly equal to HEX.
count_color() {
    "${IM_CONVERT[@]}" "$1" -fill black +opaque "$2" -fill white -opaque "$2" \
        -format '%[fx:round(mean*w*h)]' info:
}

# wait_file FILE : wait up to 5 s for the child to write FILE.
wait_file() {
    for _ in $(seq 1 50); do [ -s "$1" ] && return 0; sleep 0.1; done
    return 1
}

# --- scenarios -------------------------------------------------------------

# Rendering: default text is visible and palette colors reach the screen
# (regression for black-on-black default text and white-only fills).
scenario_render() {
    local dir="$OUT/render"; mkdir -p "$dir"
    cat >"$dir/child.sh" <<'CHILD'
#!/bin/sh
printf 'DEFAULT TEXT VISIBLE\r\n'
printf '\033[42m          \033[0m\r\n'
printf '\033[48;5;196m          \033[0m\r\n'
printf '\033[48;2;18;52;86m          \033[0m\r\n'
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    launch render "$dir/child.sh" || { fail render "no window"; return; }
    shot "$dir/shot.png"
    finish
    local white green red truecolor
    white=$(count_color "$dir/shot.png" '#FFFFFF')
    green=$(count_color "$dir/shot.png" '#00CD00')
    red=$(count_color "$dir/shot.png" '#FF0000')
    truecolor=$(count_color "$dir/shot.png" '#123456')
    echo "  pixels: fg-white=$white ansi-green=$green idx196-red=$red rgb-123456=$truecolor"
    [ "$white" -gt 200 ] || { fail render "default text not visible ($white white px)"; return; }
    [ "$green" -gt 1000 ] || { fail render "ANSI green background missing"; return; }
    [ "$red" -gt 1000 ] || { fail render "256-color background missing"; return; }
    [ "$truecolor" -gt 1000 ] || { fail render "truecolor background missing"; return; }
    pass render
}

# PTY line discipline: the child gets a cooked tty with echo, signals and
# output newline translation, like every other terminal provides.
scenario_tty() {
    local dir="$OUT/tty"; mkdir -p "$dir"
    launch tty bash --norc --noprofile || { fail tty "no window"; return; }
    xdotool type --delay 15 "stty -a > $dir/stty.txt"
    xdotool key Return
    wait_file "$dir/stty.txt" || { finish; fail tty "shell wrote no stty output (input path?)"; return; }
    finish
    local flag missing=""
    for flag in isig icanon echo onlcr icrnl; do
        grep -Eq "(^|[ ;])$flag( |;|\$)" "$dir/stty.txt" || missing="$missing $flag"
    done
    [ -z "$missing" ] || { fail tty "line discipline flags off:$missing"; return; }
    pass tty
}

# Resize: a window resize reaches the child (TIOCSWINSZ + SIGWINCH) and
# nothing is injected into its input.
scenario_resize() {
    local dir="$OUT/resize"; mkdir -p "$dir"
    launch resize bash --norc --noprofile || { fail resize "no window"; return; }
    xdotool type --delay 15 "trap 'echo winch >> $dir/winch.txt' WINCH; stty size > $dir/before.txt"
    xdotool key Return
    wait_file "$dir/before.txt" || { finish; fail resize "no initial size"; return; }
    xdotool windowsize --sync "$WIN" 500 300 2>/dev/null
    sleep 1
    xdotool type --delay 15 "stty size > $dir/after.txt"
    xdotool key Return
    wait_file "$dir/after.txt" || { finish; fail resize "no size after resize"; return; }
    finish
    echo "  size: $(cat "$dir/before.txt") -> $(cat "$dir/after.txt")"
    [ "$(cat "$dir/before.txt")" != "$(cat "$dir/after.txt")" ] \
        || { fail resize "child size unchanged"; return; }
    [ -s "$dir/winch.txt" ] || { fail resize "child got no SIGWINCH"; return; }
    pass resize
}

ALL=(render tty resize)
SELECTED=("$@")
[ ${#SELECTED[@]} -gt 0 ] || SELECTED=("${ALL[@]}")
for s in "${SELECTED[@]}"; do "scenario_$s"; done
echo "e2e: $FAILED failure(s); artifacts in $OUT"
[ "$FAILED" -eq 0 ]
