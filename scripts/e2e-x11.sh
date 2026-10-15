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
    HOME="$home" XDG_CONFIG_HOME="$home/.config" PS1='$ ' NORA_DEBUG=1 \
        "$NORA_BIN" -- "$@" >"$OUT/$name/nora.log" 2>&1 &
    NORA_PID=$!
    WIN=""
    for _ in $(seq 1 100); do
        WIN=$(xdotool search --name '^nora$' 2>/dev/null | head -1)
        [ -n "$WIN" ] && break
        sleep 0.1
    done
    [ -n "$WIN" ] || return 1
    # Ready when the first frame with terminal content is presented (a
    # software GPU compiles its pipelines on first use, which can take
    # seconds on a CI runner).
    for _ in $(seq 1 200); do
        grep -q "debug: frame rendered" "$OUT/$name/nora.log" 2>/dev/null && break
        sleep 0.1
    done
    xdotool windowfocus --sync "$WIN" 2>/dev/null
    sleep 0.5
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

# Keys: shell control characters reach the application byte-exact, UI
# chords leak nothing, and xterm encodings are used for modified keys.
scenario_keys() {
    local dir="$OUT/keys"; mkdir -p "$dir"
    launch keys bash --norc --noprofile || { fail keys "no window"; return; }
    xdotool type --delay 15 "stty raw -echo; head -c 17 | od -An -tx1 > $dir/bytes.txt; stty sane"
    xdotool key Return
    sleep 0.5
    # Two UI chords first (search toggled open and closed): no bytes.
    xdotool key ctrl+shift+f ctrl+shift+f
    xdotool key ctrl+w ctrl+t ctrl+x ctrl+f ctrl+d shift+Tab alt+BackSpace \
        ctrl+backslash ctrl+Left
    wait_file "$dir/bytes.txt" || { finish; fail keys "child received too few bytes"; return; }
    local got want
    got=$(tr -s ' \n' ' ' < "$dir/bytes.txt" | sed 's/^ //; s/ $//')
    want="17 14 18 06 04 1b 5b 5a 1b 7f 1c 1b 5b 31 3b 35 44"
    echo "  bytes: $got"
    # Ctrl+Shift+W on the only pane closes the window and nora exits.
    xdotool key ctrl+shift+w
    local exited=no
    for _ in $(seq 1 30); do kill -0 "$NORA_PID" 2>/dev/null || { exited=yes; break; }; sleep 0.1; done
    finish
    [ "$got" = "$want" ] || { fail keys "expected [$want]"; return; }
    [ "$exited" = yes ] || { fail keys "closing the last pane left nora running"; return; }
    pass keys
}

# Mouse: with SGR click reporting enabled (1000 + 1006), a click on a
# known cell arrives as exact press/release reports.
scenario_mouse() {
    local dir="$OUT/mouse"; mkdir -p "$dir"
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
stty raw -echo
printf '\033[?1000h\033[?1006h'
head -c 18 > "$dir/bytes.raw"
printf '\033[?1006l\033[?1000l'
stty sane
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    launch mouse "$dir/child.sh" || { fail mouse "no window"; return; }
    local cell cw ch
    cell=$(sed -n 's/^fonts: cell \([0-9]*\)x\([0-9]*\).*/\1 \2/p' "$OUT/mouse/nora.log" | head -1)
    read -r cw ch <<<"$cell"
    [ -n "$cw" ] || { finish; fail mouse "no cell size in log"; return; }
    # Centre of column 5, row 3 (1-based).
    xdotool mousemove --window "$WIN" $((cw * 4 + cw / 2)) $((ch * 2 + ch / 2)) click 1
    wait_file "$dir/bytes.raw" || { finish; fail mouse "no report"; return; }
    sleep 0.3
    finish
    local got
    got=$(od -An -c "$dir/bytes.raw" | tr -s ' \n' ' ' | sed 's/^ //; s/ $//')
    echo "  report: $got"
    [ "$(cat "$dir/bytes.raw")" = "$(printf '\033[<0;5;3M\033[<0;5;3m')" ] \
        || { fail mouse "expected ESC[<0;5;3M ESC[<0;5;3m"; return; }
    pass mouse
}

# Redraw: after the window is unmapped and mapped again (expose), the
# content is redrawn without any new output from the child.
scenario_redraw() {
    local dir="$OUT/redraw"; mkdir -p "$dir"
    printf '#!/bin/sh\nprintf "REDRAW CHECK TEXT\\r\\n"\nsleep 30\n' >"$dir/child.sh"
    chmod +x "$dir/child.sh"
    launch redraw "$dir/child.sh" || { fail redraw "no window"; return; }
    xdotool windowunmap --sync "$WIN"
    sleep 0.5
    xdotool windowmap --sync "$WIN"
    shot "$dir/shot.png"
    finish
    local white
    white=$(count_color "$dir/shot.png" '#FFFFFF')
    echo "  white px after expose: $white"
    [ "$white" -gt 200 ] || { fail redraw "content not redrawn after expose"; return; }
    pass redraw
}

# Selection: a mouse drag highlights cells immediately (selection changes
# are not terminal damage; the pump must still publish them).
scenario_selection() {
    local dir="$OUT/selection"; mkdir -p "$dir"
    printf '#!/bin/sh\nprintf "SELECT ME PLEASE\\r\\n"\nsleep 30\n' >"$dir/child.sh"
    chmod +x "$dir/child.sh"
    launch selection "$dir/child.sh" || { fail selection "no window"; return; }
    local cw ch
    read -r cw ch <<<"$(sed -n 's/^fonts: cell \([0-9]*\)x\([0-9]*\).*/\1 \2/p' "$OUT/selection/nora.log" | head -1)"
    xdotool mousemove --window "$WIN" $((cw / 2)) $((ch / 2)) mousedown 1
    xdotool mousemove --window "$WIN" $((cw * 10)) $((ch / 2))
    xdotool mousemove --window "$WIN" $((cw * 12)) $((ch / 2)) mouseup 1
    shot "$dir/shot.png"
    finish
    local sel
    sel=$(count_color "$dir/shot.png" '#335577')
    echo "  selection px: $sel"
    [ "$sel" -gt $((cw * ch * 4)) ] || { fail selection "no selection highlight"; return; }
    pass selection
}

# Zoom: Ctrl+Shift+Plus enlarges the cells, so the child's grid shrinks
# (the PTY is resized), with no other trigger.
scenario_zoom() {
    local dir="$OUT/zoom"; mkdir -p "$dir"
    launch zoom bash --norc --noprofile || { fail zoom "no window"; return; }
    xdotool type --delay 15 "stty size > $dir/before.txt"
    xdotool key Return
    wait_file "$dir/before.txt" || { finish; fail zoom "no initial size"; return; }
    xdotool key ctrl+shift+equal ctrl+shift+equal
    sleep 1
    xdotool type --delay 15 "stty size > $dir/after.txt"
    xdotool key Return
    wait_file "$dir/after.txt" || { finish; fail zoom "no size after zoom"; return; }
    shot "$dir/shot.png"
    finish
    local r0 c0 r1 c1
    read -r r0 c0 < "$dir/before.txt"; read -r r1 c1 < "$dir/after.txt"
    echo "  size: ${r0}x${c0} -> ${r1}x${c1}"
    [ "$c1" -lt "$c0" ] && [ "$r1" -lt "$r0" ] || { fail zoom "grid did not shrink"; return; }
    pass zoom
}

# Flood: an unterminated OSC followed by 200 MiB of payload must not grow
# nora's memory (the frame is capped and dropped), and output after the
# terminator still renders.
scenario_flood() {
    local dir="$OUT/flood"; mkdir -p "$dir"
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
sleep 1
printf '\033]0;'
head -c 209715200 /dev/zero | tr '\\000' a
printf '\007FLOOD DONE\r\n'
touch "$dir/done"
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    launch flood "$dir/child.sh" || { fail flood "no window"; return; }
    local before after
    before=$(awk '/^VmRSS/{print $2}' "/proc/$NORA_PID/status")
    for _ in $(seq 1 600); do [ -e "$dir/done" ] && break; sleep 0.1; done
    sleep 1
    after=$(awk '/^VmHWM/{print $2}' "/proc/$NORA_PID/status")
    shot "$dir/shot.png"
    finish
    [ -e "$dir/done" ] || { fail flood "child did not finish the flood"; return; }
    echo "  rss before ${before} kB, peak after ${after} kB"
    [ $((after - before)) -lt 65536 ] || { fail flood "memory grew by $(( (after - before) / 1024 )) MiB"; return; }
    [ "$(count_color "$dir/shot.png" '#FFFFFF')" -gt 100 ] || { fail flood "no output after the flood"; return; }
    pass flood
}

# Latency (measurement, not a pass/fail gate): key-to-present times for
# 40 typed characters echoed by bash, from NORA_TRACE_LATENCY.
scenario_latency() {
    local dir="$OUT/latency"; mkdir -p "$dir"
    NORA_TRACE_LATENCY=1 launch latency bash --norc --noprofile \
        || { fail latency "no window"; return; }
    xdotool type --delay 120 "abcdefghijklmnopqrstuvwxyzabcdefghijklmn"
    sleep 1
    finish
    grep -o 'key-to-present [0-9.]*' "$OUT/latency/nora.log" | awk '{print $2}' \
        | sort -n > "$dir/samples.txt"
    local n med p95
    n=$(wc -l < "$dir/samples.txt")
    [ "$n" -ge 20 ] || { fail latency "only $n samples"; return; }
    med=$(awk -v n="$n" 'NR==int((n+1)/2){print}' "$dir/samples.txt")
    p95=$(awk -v n="$n" 'NR==int(n*0.95+0.5){print}' "$dir/samples.txt")
    echo "  key-to-present ms: n=$n median=$med p95=$p95"
    pass latency
}

# Title: OSC 2 from the application becomes the window title.
scenario_title() {
    local dir="$OUT/title"; mkdir -p "$dir"
    printf '#!/bin/sh\nsleep 2\nprintf "\\033]2;NORA TITLE TEST\\007"\nsleep 30\n' >"$dir/child.sh"
    chmod +x "$dir/child.sh"
    launch title "$dir/child.sh" || { fail title "no window"; return; }
    local name=""
    for _ in $(seq 1 50); do
        name=$(xdotool getwindowname "$WIN" 2>/dev/null)
        [ "$name" = "NORA TITLE TEST" ] && break
        sleep 0.1
    done
    finish
    echo "  window name: $name"
    [ "$name" = "NORA TITLE TEST" ] || { fail title "title not applied"; return; }
    pass title
}

ALL=(render tty resize keys mouse redraw selection zoom flood latency title)
SELECTED=("$@")
[ ${#SELECTED[@]} -gt 0 ] || SELECTED=("${ALL[@]}")
for s in "${SELECTED[@]}"; do "scenario_$s"; done
echo "e2e: $FAILED failure(s); artifacts in $OUT"
[ "$FAILED" -eq 0 ]
