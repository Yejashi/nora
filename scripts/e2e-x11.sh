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

# A non-US keyboard (v0.1 acceptance 4): German. Xvfb applies runtime
# keymap uploads (setxkbmap) inconsistently, so this scenario runs its own
# Xvfb whose default keymap is German: an XKB directory of links to the
# system data, with `symbols/us` including `de`. Layout letters, a dead
# key composed by Xlib's built-in input method, AltGr and the moved z
# arrive byte-exact.
scenario_layout() {
    local system_xkb=/usr/share/X11/xkb
    [ -d "$system_xkb/symbols" ] || { echo "SKIP layout: no $system_xkb"; return; }
    local dir="$OUT/layout"; rm -rf "$dir"; mkdir -p "$dir/xkb/symbols"
    local entry
    for entry in "$system_xkb"/*; do
        [ "$(basename "$entry")" = symbols ] || ln -s "$entry" "$dir/xkb/$(basename "$entry")"
    done
    for entry in "$system_xkb"/symbols/*; do
        [ "$(basename "$entry")" = us ] || ln -s "$entry" "$dir/xkb/symbols/$(basename "$entry")"
    done
    printf 'default partial alphanumeric_keys modifier_keys\nxkb_symbols "basic" {\n    include "de(basic)"\n};\n' \
        >"$dir/xkb/symbols/us"
    local outer=$DISPLAY display=":$((DISPLAY_NUM + 1))"
    Xvfb "$display" -screen 0 1024x700x24 -nolisten tcp -xkbdir "$dir/xkb" >"$dir/xvfb.log" 2>&1 &
    local xvfb=$!
    export DISPLAY=$display
    for _ in $(seq 1 50); do xdotool getmouselocation >/dev/null 2>&1 && break; sleep 0.1; done
    restore() { kill "$xvfb" 2>/dev/null; export DISPLAY=$outer; }
    if ! xdotool key --help >/dev/null 2>&1 || ! xmodmap -pke 2>/dev/null | grep -qE '^keycode +34 = udiaeresis'; then
        restore
        fail layout "the German keymap is not active on $display"
        return
    fi
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
stty raw -echo
echo 1 >"$dir/ready"
timeout --foreground 10 head -c 8 >"$dir/bytes.raw"; od -An -tx1 "$dir/bytes.raw" >"$dir/bytes.txt"
stty sane
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    if launch layout "$dir/child.sh" && wait_file "$dir/ready"; then
        sleep 0.3
        # ü, ß, dead acute + e, AltGr+q (@), and z (the US y key).
        xdotool key udiaeresis ssharp dead_acute e ISO_Level3_Shift+q z
        wait_file "$dir/bytes.txt"
    fi
    finish
    restore
    local got
    got=$(tr -s ' \n' ' ' <"$dir/bytes.txt" 2>/dev/null | sed 's/^ //; s/ $//')
    echo "  bytes: $got"
    [ "$got" = "c3 bc c3 9f c3 a9 40 7a" ] \
        || { fail layout "expected ü ß é @ z (c3 bc c3 9f c3 a9 40 7a)"; return; }
    pass layout
}

# Kitty keyboard protocol (M1.3): a raw-mode child pushes flags, and real
# key events arrive in the protocol's encoding; popping restores legacy.
scenario_kitty() {
    local dir="$OUT/kitty"; mkdir -p "$dir"
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
stty raw -echo
printf '\033[>1u'; echo 1 >"$dir/ready1"
head -c 19 | od -An -tx1 >"$dir/bytes1.txt"
printf '\033[>11u'; echo 2 >"$dir/ready2"
head -c 14 | od -An -tx1 >"$dir/bytes2.txt"
printf '\033[<u\033[<u'; echo 3 >"$dir/ready3"
head -c 1 | od -An -tx1 >"$dir/bytes3.txt"
stty sane
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    launch kitty "$dir/child.sh" || { fail kitty "no window"; return; }
    # Each phase starts once the child set its flags and the window has
    # them (the mode change reaches the window with the next frame).
    phase() { wait_file "$dir/ready$1" || return 1; sleep 0.5; }
    phase 1 || { finish; fail kitty "child did not start"; return; }
    xdotool key ctrl+a Escape b ctrl+Up
    phase 2 || { finish; fail kitty "phase 1 got too few bytes: $(cat "$dir/bytes1.txt" 2>/dev/null)"; return; }
    xdotool key c
    phase 3 || { finish; fail kitty "phase 2 got too few bytes"; return; }
    xdotool key ctrl+a
    wait_file "$dir/bytes3.txt"
    finish
    local got1 got2 got3
    got1=$(tr -s ' \n' ' ' <"$dir/bytes1.txt" | sed 's/^ //; s/ $//')
    got2=$(tr -s ' \n' ' ' <"$dir/bytes2.txt" | sed 's/^ //; s/ $//')
    got3=$(tr -s ' \n' ' ' <"$dir/bytes3.txt" 2>/dev/null | sed 's/^ //; s/ $//')
    echo "  disambiguate: $got1"
    echo "  all keys + event types: $got2"
    echo "  after pop: $got3"
    # CSI 97;5u, CSI 27u, b, CSI 1;5A
    [ "$got1" = "1b 5b 39 37 3b 35 75 1b 5b 32 37 75 62 1b 5b 31 3b 35 41" ] \
        || { fail kitty "disambiguate bytes differ"; return; }
    # CSI 99u (press), CSI 99;1:3u (release)
    [ "$got2" = "1b 5b 39 39 75 1b 5b 39 39 3b 31 3a 33 75" ] \
        || { fail kitty "all-keys bytes differ"; return; }
    [ "$got3" = "01" ] || { fail kitty "legacy Ctrl+A after pop: $got3"; return; }
    pass kitty
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

# View latency: a view change on an idle pane (scrollback paging, which
# the pump applies) reaches the screen promptly. The pump once waited for
# its 500 ms idle heartbeat before building the view (audit RV2).
scenario_viewlat() {
    local dir="$OUT/viewlat"; mkdir -p "$dir"
    NORA_TRACE_LATENCY=1 launch viewlat bash --norc --noprofile \
        || { fail viewlat "no window"; return; }
    xdotool type --delay 15 "seq 1 500"; xdotool key Return
    sleep 1
    local mark
    mark=$(grep -c 'key-to-fresh' "$OUT/viewlat/nora.log")
    for _ in 1 2 3 4; do
        xdotool key shift+Prior; sleep 0.8
        xdotool key shift+Next; sleep 0.8
    done
    finish
    grep -o 'key-to-fresh [0-9.]*' "$OUT/viewlat/nora.log" | tail -n +"$((mark + 1))" \
        | awk '{print $2}' | sort -n > "$dir/samples.txt"
    local n max
    n=$(wc -l < "$dir/samples.txt")
    [ "$n" -ge 6 ] || { fail viewlat "only $n samples"; return; }
    max=$(tail -1 "$dir/samples.txt")
    echo "  scroll key-to-fresh ms: n=$n median=$(awk -v n="$n" 'NR==int((n+1)/2)' "$dir/samples.txt") max=$max"
    awk -v m="$max" 'BEGIN{exit !(m < 100)}' || { fail viewlat "slow view change: max $max ms"; return; }
    pass viewlat
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

# Panes and tabs: a split gives each pane its share of the grid (the
# child's own stty size), a click focuses a pane, separators are drawn, a
# second tab loses a row to the tab bar and gets it back when closed.
scenario_panes() {
    local dir="$OUT/panes"; mkdir -p "$dir"
    launch panes bash --norc --noprofile || { fail panes "no window"; return; }
    local cw ch
    read -r cw ch <<<"$(sed -n 's/^fonts: cell \([0-9]*\)x\([0-9]*\).*/\1 \2/p' "$OUT/panes/nora.log" | head -1)"
    xdotool type --delay 15 "stty size > $dir/full.txt"; xdotool key Return
    wait_file "$dir/full.txt" || { finish; fail panes "no initial size"; return; }
    xdotool key ctrl+shift+Return
    sleep 1.5
    xdotool type --delay 15 "stty size > $dir/right.txt"; xdotool key Return
    wait_file "$dir/right.txt" || { finish; fail panes "right pane got no input"; return; }
    # Click into the left pane, then type there.
    xdotool mousemove --window "$WIN" $((cw * 3)) $((ch * 5)) click 1
    sleep 0.3
    xdotool type --delay 15 "stty size > $dir/left.txt"; xdotool key Return
    wait_file "$dir/left.txt" || { finish; fail panes "click did not focus the left pane"; return; }
    shot "$dir/split.png"
    xdotool key ctrl+shift+t
    sleep 1.5
    xdotool type --delay 15 "stty size > $dir/tab2.txt"; xdotool key Return
    wait_file "$dir/tab2.txt" || { finish; fail panes "new tab got no input"; return; }
    xdotool key ctrl+shift+w
    sleep 1
    xdotool type --delay 15 "stty size > $dir/after.txt"; xdotool key Return
    wait_file "$dir/after.txt" || { finish; fail panes "no input after closing the tab"; return; }
    finish
    local fr fc rr rc lr lc tr tc ar ac sep
    read -r fr fc < "$dir/full.txt"; read -r rr rc < "$dir/right.txt"
    read -r lr lc < "$dir/left.txt"; read -r tr tc < "$dir/tab2.txt"
    read -r ar ac < "$dir/after.txt"
    sep=$(count_color "$dir/split.png" '#555555')
    echo "  full ${fr}x${fc}; left ${lr}x${lc} right ${rr}x${rc}; tab2 ${tr}x${tc}; after close ${ar}x${ac}; separator px $sep"
    [ $((lc + rc)) -eq "$fc" ] && [ "$lr" -eq "$fr" ] || { fail panes "split sizes do not tile the grid"; return; }
    [ "$sep" -ge $((ch * fr / 2)) ] || { fail panes "no separator drawn"; return; }
    [ "$tr" -eq $((fr - 1)) ] && [ "$tc" -eq "$fc" ] || { fail panes "tab bar row not reserved"; return; }
    [ "$ar" -eq "$fr" ] || { fail panes "row not returned after closing the tab (${ar} vs ${fr})"; return; }
    pass panes
}

# Pane exit: a shell exiting in one half of a split closes that pane (the
# window stays and the other pane takes the full width); exiting the last
# pane closes nora.
scenario_paneexit() {
    local dir="$OUT/paneexit"; mkdir -p "$dir"
    launch paneexit bash --norc --noprofile || { fail paneexit "no window"; return; }
    xdotool type --delay 15 "stty size > $dir/full.txt"; xdotool key Return
    wait_file "$dir/full.txt" || { finish; fail paneexit "no initial size"; return; }
    xdotool key ctrl+shift+Return
    sleep 1.5
    xdotool type --delay 15 "exit"; xdotool key Return
    sleep 1.5
    kill -0 "$NORA_PID" 2>/dev/null || { fail paneexit "nora exited with a pane left"; return; }
    xdotool type --delay 15 "stty size > $dir/after.txt"; xdotool key Return
    wait_file "$dir/after.txt" || { finish; fail paneexit "remaining pane got no input"; return; }
    xdotool type --delay 15 "exit"; xdotool key Return
    local exited=no
    for _ in $(seq 1 40); do kill -0 "$NORA_PID" 2>/dev/null || { exited=yes; break; }; sleep 0.1; done
    finish
    echo "  full: $(cat "$dir/full.txt"), after pane exit: $(cat "$dir/after.txt")"
    [ "$(cat "$dir/after.txt")" = "$(cat "$dir/full.txt")" ] || { fail paneexit "remaining pane not resized to full"; return; }
    [ "$exited" = yes ] || { fail paneexit "nora did not exit with its last pane"; return; }
    pass paneexit
}

# Split with no room (audit RV6): splitting a two-row window twice gives
# the newest pane no rows. It must get its own session once the window grows, not
# stay dead or share the old pane's snapshot slot.
scenario_tinysplit() {
    local dir="$OUT/tinysplit"; mkdir -p "$dir/home/.config/nora"
    printf '[window]\ncolumns = 60\nlines = 2\n' >"$dir/home/.config/nora/config.toml"
    launch tinysplit bash --norc --noprofile || { fail tinysplit "no window"; return; }
    xdotool type --delay 15 "stty size > $dir/first.txt"; xdotool key Return
    wait_file "$dir/first.txt" || { finish; fail tinysplit "no initial size"; return; }
    xdotool key ctrl+shift+backslash; sleep 0.5
    xdotool key ctrl+shift+backslash; sleep 1
    xdotool windowsize --sync "$WIN" 700 500 2>/dev/null
    sleep 1.5
    xdotool type --delay 15 "stty size > $dir/new.txt"; xdotool key Return
    wait_file "$dir/new.txt" || { finish; fail tinysplit "the new pane has no session after growing"; return; }
    finish
    echo "  first pane: $(cat "$dir/first.txt"); new pane after growing: $(cat "$dir/new.txt")"
    pass tinysplit
}

# Search: the bar shows the query and position, matches in the scrollback
# are found and scrolled into view (marked with the selection color), and
# nothing typed into search (text, Space, arrows, Tab) reaches the shell.
scenario_search() {
    local dir="$OUT/search"; mkdir -p "$dir"
    launch search bash --norc --noprofile || { fail search "no window"; return; }
    xdotool type --delay 15 "echo NEEDLE-OLD; seq 1 200"; xdotool key Return
    sleep 1
    shot "$dir/before.png"
    xdotool key ctrl+shift+f
    xdotool type --delay 15 "needle-old"
    xdotool key space BackSpace Up Down Left Tab
    # Unbound Ctrl/Alt chords add nothing to the query (audit RV8): the
    # match must still be shown.
    xdotool key ctrl+a alt+b ctrl+e
    shot "$dir/bar.png"
    xdotool key Escape
    xdotool type --delay 15 "echo CLEAN > $dir/out.txt"; xdotool key Return
    wait_file "$dir/out.txt" || { finish; fail search "command after search did not run"; return; }
    finish
    local bar sel
    bar=$(count_color "$dir/bar.png" '#3A2E10')
    sel=$(count_color "$dir/bar.png" '#335577')
    echo "  search bar px: $bar, match highlight px: $sel, command output: $(cat "$dir/out.txt")"
    [ "$bar" -gt 1000 ] || { fail search "no search bar drawn"; return; }
    [ "$sel" -gt 100 ] || { fail search "scrollback match not shown"; return; }
    [ "$(cat "$dir/out.txt")" = "CLEAN" ] || { fail search "search keys leaked to the shell"; return; }
    pass search
}

# Config: [window] size, [colors] and [scrolling] apply at startup; the
# file is watched and a change applies live; an invalid edit keeps the
# last good config.
scenario_config() {
    local dir="$OUT/config"; mkdir -p "$dir/home/.config/nora"
    local cfg="$dir/home/.config/nora/config.toml"
    printf '[window]\ncolumns = 50\nlines = 10\n[colors]\nbackground = "#202040"\n[cursor]\nstyle = "beam"\n' >"$cfg"
    launch config bash --norc --noprofile || { fail config "no window"; return; }
    xdotool type --delay 15 "stty size > $dir/size.txt; seq 1 300; printf '\\033[41m%40s\\033[0m\\n' x"; xdotool key Return
    wait_file "$dir/size.txt" || { finish; fail config "no size"; return; }
    sleep 0.5
    shot "$dir/before.png"
    xdotool key shift+Prior
    shot "$dir/scrolled.png"
    xdotool key shift+End
    # The reload also recolors ANSI red: the idle pane's red cells must
    # repaint (audit RV5: retained snapshots kept the old colors). The
    # cursor style stays, since a cursor change forced a repaint anyway.
    printf '[window]\ncolumns = 50\nlines = 10\n[colors]\nbackground = "#402020"\n[colors.normal]\nred = "#00ff00"\n[cursor]\nstyle = "beam"\n' >"$cfg"
    sleep 2.5
    shot "$dir/reloaded.png"
    printf '[colors]\nbackground = "nonsense"\n' >"$cfg"
    sleep 2.5
    shot "$dir/invalid.png"
    finish
    local before after scrolled_diff
    before=$(count_color "$dir/before.png" '#202040')
    after=$(count_color "$dir/reloaded.png" '#402020')
    local green
    green=$(count_color "$dir/reloaded.png" '#00ff00')
    scrolled_diff=$("${IM_CONVERT[@]}" "$dir/before.png" "$dir/scrolled.png" -compose difference -composite -format '%[fx:round(mean*w*h*255)]' info: | xargs printf '%.0f')
    echo "  size: $(cat "$dir/size.txt"); bg px before=$before after reload=$after; recolored red px=$green; scroll diff=$scrolled_diff"
    [ "$(cat "$dir/size.txt")" = "10 50" ] || { fail config "window cells not applied"; return; }
    [ "$before" -gt 10000 ] || { fail config "background color not applied"; return; }
    [ "$scrolled_diff" -gt 0 ] || { fail config "no scrollback (Shift+PageUp changed nothing)"; return; }
    [ "$after" -gt 10000 ] || { fail config "live reload did not apply"; return; }
    [ "$green" -gt 1000 ] || { fail config "palette reload did not repaint the idle pane"; return; }
    [ "$(count_color "$dir/invalid.png" '#402020')" -gt 10000 ] || { fail config "invalid edit lost the last good config"; return; }
    pass config
}

# Reports and environment: an OSC 11 query answers with the configured
# background, CSI 14 t reports the real pixel size, TERM_PROGRAM is set,
# and an inherited VTE_VERSION does not reach the child.
scenario_reports() {
    local dir="$OUT/reports"; mkdir -p "$dir/home/.config/nora"
    printf '[colors]\nbackground = "#123456"\n' >"$dir/home/.config/nora/config.toml"
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
printf '%s|%s' "\$TERM_PROGRAM" "\${VTE_VERSION:-unset}" > "$dir/env.txt"
stty raw -echo
printf '\033]11;?\033\\\\'
dd bs=1 count=24 of="$dir/osc11.raw" 2>/dev/null
printf '\033[14t'
dd bs=1 count=12 of="$dir/px.raw" 2>/dev/null
stty sane
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    VTE_VERSION=7000 launch reports "$dir/child.sh" || { fail reports "no window"; return; }
    wait_file "$dir/px.raw" || { finish; fail reports "no reply to the size query"; return; }
    finish
    local osc px
    osc=$(tr -d '\033\\' < "$dir/osc11.raw" | tr -cd '[:print:]')
    px=$(tr -cd '0-9;' < "$dir/px.raw")
    echo "  env: $(cat "$dir/env.txt"); osc11: $osc; 14t: $px"
    [ "$(cat "$dir/env.txt")" = "nora|unset" ] || { fail reports "environment"; return; }
    case "$osc" in *rgb:1212/3434/5656*) ;; *) fail reports "OSC 11 reply is not the configured background"; return ;; esac
    case "$px" in 4\;*\;*) ;; *) fail reports "no CSI 4 t size report"; return ;; esac
    [ "$px" != "4;384;640" ] || { fail reports "nominal 8x16 pixel size reported"; return; }
    pass reports
}

# Hyperlinks: Ctrl+click on an OSC 8 link runs the opener with the URL as
# one argument (a stand-in xdg-open records it).
scenario_hyperlink() {
    local dir="$OUT/hyperlink"; mkdir -p "$dir/bin"
    printf '#!/bin/sh\nprintf "%%s" "$1" > "%s/opened.txt"\n' "$dir" >"$dir/bin/xdg-open"
    chmod +x "$dir/bin/xdg-open"
    printf '#!/bin/sh\nprintf "\\033]8;;https://example.com/nora?a=1\\033\\\\LINK TEXT\\033]8;;\\033\\\\\\r\\n"\nsleep 30\n' >"$dir/child.sh"
    chmod +x "$dir/child.sh"
    PATH="$dir/bin:$PATH" launch hyperlink "$dir/child.sh" || { fail hyperlink "no window"; return; }
    local cw ch
    read -r cw ch <<<"$(sed -n 's/^fonts: cell \([0-9]*\)x\([0-9]*\).*/\1 \2/p' "$OUT/hyperlink/nora.log" | head -1)"
    xdotool mousemove --window "$WIN" $((cw * 3)) $((ch / 2))
    xdotool keydown ctrl click 1 keyup ctrl
    wait_file "$dir/opened.txt" || { finish; fail hyperlink "opener not run"; return; }
    finish
    echo "  opened: $(cat "$dir/opened.txt")"
    [ "$(cat "$dir/opened.txt")" = "https://example.com/nora?a=1" ] || { fail hyperlink "wrong URL"; return; }
    pass hyperlink
}

# Idle: a window that is idle (also while fully or partly scrolled back) draws
# no frames; a redraw loop here once drew hundreds per second.
scenario_idle() {
    local dir="$OUT/idle"; mkdir -p "$dir"
    NORA_TRACE_FRAMES=1 launch idle bash --norc --noprofile || { fail idle "no window"; return; }
    xdotool type --delay 15 "seq 1 300"; xdotool key Return
    sleep 1
    xdotool key shift+Prior
    sleep 1
    local before after
    before=$(grep -c '^frame:' "$OUT/idle/nora.log")
    sleep 3
    after=$(grep -c '^frame:' "$OUT/idle/nora.log")
    # Partly scrolled with the cursor still on screen (audit RV1): clear
    # the screen (its lines go to history), then one wheel notch up.
    xdotool key shift+Next
    xdotool type --delay 15 "printf '\\033[H\\033[2J'"; xdotool key Return
    sleep 1
    xdotool mousemove --window "$WIN" 40 40 click 4
    sleep 1
    local before2 after2
    before2=$(grep -c '^frame:' "$OUT/idle/nora.log")
    sleep 3
    after2=$(grep -c '^frame:' "$OUT/idle/nora.log")
    finish
    echo "  frames during 3 s idle while scrolled back: $((after - before))"
    echo "  frames during 3 s idle while partly scrolled: $((after2 - before2))"
    [ $((after - before)) -le 2 ] || { fail idle "redraw loop: $((after - before)) frames while idle"; return; }
    [ $((after2 - before2)) -le 2 ] || { fail idle "redraw loop: $((after2 - before2)) frames while partly scrolled"; return; }
    pass idle
}

# Real applications. Each is skipped (not failed) when not installed.
have() { command -v "$1" >/dev/null 2>&1 || { echo "SKIP $2: $1 not installed"; return 1; }; }

# vim: insert mode, Escape and an ex command reach vim byte-exact, and
# its alternate-screen session ends cleanly (the file is written).
scenario_vim() {
    have vim vim || return
    local dir="$OUT/vim"; mkdir -p "$dir"
    printf 'second line\n' >"$dir/file.txt"
    launch vim vim -u NONE -N -n "$dir/file.txt" || { fail vim "no window"; return; }
    xdotool type --delay 30 "Ofirst line"
    xdotool key Escape
    xdotool type --delay 30 ":wq"
    xdotool key Return
    local exited=no
    for _ in $(seq 1 50); do kill -0 "$NORA_PID" 2>/dev/null || { exited=yes; break; }; sleep 0.1; done
    finish
    [ "$(cat "$dir/file.txt")" = "$(printf 'first line\nsecond line')" ] \
        || { fail vim "file content: $(tr '\n' '|' < "$dir/file.txt")"; return; }
    [ "$exited" = yes ] || { fail vim "nora did not exit with vim"; return; }
    pass vim
}

# zsh integration (ADR-013 step 2): the buffer nora receives through
# OSC 6973 follows zsh's editor through typing, completion, kill/yank and
# history recall, none of which a keystroke shadow could follow.
scenario_zshell() {
    have zsh zshell || return
    local dir="$OUT/zshell"; mkdir -p "$dir/zdot" "$dir/work"
    : >"$dir/work/uniquefile.txt"
    cat >"$dir/zdot/.zshrc" <<RC
PROMPT='%% '
HISTSIZE=100
bindkey -e
cd "$dir/work"
source "$ROOT/shell/nora-zsh.zsh"
RC
    # -d: no global rc files (a distribution's /etc/zsh may run compinit
    # or the new-user wizard, which ask questions).
    ZDOTDIR="$dir/zdot" NORA_TRACE_SHELL=1 launch zshell zsh -d -i \
        || { fail zshell "no window"; return; }
    # Type only once the editor is up (its first, empty buffer report).
    for _ in $(seq 1 100); do
        grep -q '^shell: buffer "" cursor 0' "$OUT/zshell/nora.log" && break
        sleep 0.1
    done
    xdotool type --delay 40 "echo one"; xdotool key Return; sleep 0.5
    xdotool type --delay 40 "cat uniq"; xdotool key Tab; sleep 0.5
    xdotool key ctrl+u; sleep 0.3
    xdotool key ctrl+y; sleep 0.3
    xdotool key ctrl+u; sleep 0.3
    xdotool key Up; sleep 0.5
    finish
    sed -n 's/^shell: buffer //p' "$OUT/zshell/nora.log" >"$dir/buffers.txt"
    echo "  zsh buffers: $(wc -l <"$dir/buffers.txt") reports, last: $(tail -1 "$dir/buffers.txt")"
    # Each expected state, in order (other reports may come between).
    local expect=('"echo one" cursor 8' '"cat uniquefile.txt " cursor 19' '"" cursor 0'
        '"cat uniquefile.txt " cursor 19' '"" cursor 0' '"echo one" cursor 8')
    local line i=0
    while IFS= read -r line; do
        [ "$i" -lt ${#expect[@]} ] && [ "$line" = "${expect[$i]}" ] && i=$((i + 1))
    done <"$dir/buffers.txt"
    [ "$i" -eq ${#expect[@]} ] || { fail zshell "buffer reports diverged at step $((i + 1)) (${expect[$i]}); got: $(tr '\n' '|' <"$dir/buffers.txt" | cut -c1-400)"; return; }
    pass zshell
}

# Inline suggestions (ADR-013 step 4): a command run once is suggested
# (dim ghost text) for a prefix; Right types it, and the shell runs
# exactly the suggestion. SIGTERM then closes nora in order, and the
# history is saved under the state directory.
scenario_suggest() {
    have zsh suggest || return
    local dir="$OUT/suggest"; rm -rf "$dir"; mkdir -p "$dir/zdot"
    cat >"$dir/zdot/.zshrc" <<RC
PROMPT='%% '
bindkey -e
source "$ROOT/shell/nora-zsh.zsh"
RC
    ZDOTDIR="$dir/zdot" NORA_TRACE_SHELL=1 NORA_TRACE_LATENCY=1 launch suggest zsh -d -i \
        || { fail suggest "no window"; return; }
    for _ in $(seq 1 100); do
        grep -q '^shell: buffer "" cursor 0' "$OUT/suggest/nora.log" && break
        sleep 0.1
    done
    xdotool type --delay 30 "echo ghost-alpha-1 >> $dir/out.txt"; xdotool key Return; sleep 0.5
    shot "$dir/plain.png"
    xdotool type --delay 30 "echo gho"; sleep 0.7
    shot "$dir/ghost.png"
    xdotool key Right; sleep 0.3
    xdotool key Return
    wait_lines() { [ "$(wc -l <"$dir/out.txt" 2>/dev/null)" -ge 2 ]; }
    for _ in $(seq 1 30); do wait_lines && break; sleep 0.1; done
    # No history match: the last word completes as a path (zsh's cwd is
    # the repository).
    xdotool type --delay 30 "ls scripts/e2e-x1"; sleep 0.7
    xdotool key ctrl+u; sleep 0.3
    # SIGTERM closes nora in order: sessions closed, history saved.
    kill -TERM "$NORA_PID"
    local exited=no
    for _ in $(seq 1 50); do kill -0 "$NORA_PID" 2>/dev/null || { exited=yes; break; }; sleep 0.1; done
    finish
    local ghost plain hist="$OUT/suggest/home/.local/state/nora/history"
    ghost=$(count_color "$dir/ghost.png" '#666666')
    plain=$(count_color "$dir/plain.png" '#666666')
    echo "  ghost px: $ghost (before: $plain); out: $(tr '\n' '|' <"$dir/out.txt")"
    echo "  key-to-suggestion ms: $(grep -o 'key-to-suggestion [0-9.]*' "$OUT/suggest/nora.log" | awk '{print $2}' | tr '\n' ' ')"
    [ "$ghost" -gt $((plain + 50)) ] || { fail suggest "no ghost text drawn"; return; }
    [ "$(cat "$dir/out.txt")" = "$(printf 'ghost-alpha-1\nghost-alpha-1')" ] \
        || { fail suggest "accepted command differs: $(tr '\n' '|' <"$dir/out.txt")"; return; }
    grep -q '^shell: suggestion Some("1.sh")' "$OUT/suggest/nora.log" || { fail suggest "no path completion for scripts/e2e-x1"; return; }
    [ "$exited" = yes ] || { fail suggest "nora did not exit within 5 s of SIGTERM"; return; }
    grep -q "termination signal" "$OUT/suggest/nora.log" || { fail suggest "SIGTERM not handled"; return; }
    grep -q "echo ghost-alpha-1" "$hist" 2>/dev/null || { fail suggest "history not saved at $hist"; return; }
    pass suggest
}

# Fractional and integer scale (M1.6), forced with WINIT_X11_SCALE_FACTOR:
# the grid is the configured 80x24 at every factor, the window grows by
# about the factor (cell sizes are rounded at each size, so within 10%),
# and the same text covers about factor^2 the pixels.
scenario_scale() {
    local dir="$OUT/scale"; mkdir -p "$dir"
    cat >"$dir/child.sh" <<'CHILD'
#!/bin/sh
# The window settles on its grid after startup (a resize request).
sleep 1
stty size >"$1"
printf 'SCALE TEST\r\n'
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    local f w h rows cols text base_w base_h base_rows base_cols base_text
    for f in 1 1.25 2; do
        WINIT_X11_SCALE_FACTOR=$f launch scale "$dir/child.sh" "$dir/size-$f.txt" \
            || { fail scale "no window at $f"; return; }
        wait_file "$dir/size-$f.txt" || { finish; fail scale "no size at $f"; return; }
        eval "$(xdotool getwindowgeometry --shell "$WIN" | grep -E '^(WIDTH|HEIGHT)=')"
        w=$WIDTH h=$HEIGHT
        shot "$dir/shot-$f.png"
        finish
        "${IM_CONVERT[@]}" "$dir/shot-$f.png" -crop 400x80+0+0 +repage "$dir/text-$f.png"
        text=$(count_color "$dir/text-$f.png" '#FFFFFF')
        read -r rows cols <"$dir/size-$f.txt"
        echo "  scale $f: window ${w}x${h}, grid ${rows}x${cols}, text px $text"
        if [ "$f" = 1 ]; then
            base_w=$w base_h=$h base_rows=$rows base_cols=$cols base_text=$text
            continue
        fi
        awk -v w="$w" -v h="$h" -v bw="$base_w" -v bh="$base_h" -v f="$f" \
            'BEGIN { exit !(w >= bw*f*0.9 && w <= bw*f*1.1 && h >= bh*f*0.9 && h <= bh*f*1.1) }' \
            || { fail scale "window ${w}x${h} at $f vs ${base_w}x${base_h} at 1"; return; }
        [ "$rows $cols" = "$base_rows $base_cols" ] \
            || { fail scale "grid ${rows}x${cols} at $f vs ${base_rows}x${base_cols} at 1"; return; }
        awk -v t="$text" -v b="$base_text" -v f="$f" 'BEGIN { exit !(t >= b*f*f*0.7 && t <= b*f*f*1.3) }' \
            || { fail scale "text px $text at $f vs $base_text at 1 (expected about x$f^2)"; return; }
    done
    pass scale
}

# Glyph atlas soak (audit R8): about 32 000 distinct CJK and Hangul
# glyphs, one new screenful at a time, drawn in two passes at two zoom
# levels. The atlas must trim glyphs a frame no longer uses: no frame may
# fail to draw, the last screen's text must be visible, and the second
# pass (new glyph sizes, while fonts and the shaping cache are already at
# their bounds) must not grow memory much. Untrimmed, the second pass
# grows the atlas texture, which lavapipe keeps in process memory.
scenario_atlas() {
    local dir="$OUT/atlas"; mkdir -p "$dir"
    # 40 double-width glyphs per row, 24 rows per screen.
    local cp line="" n=0
    for cp in $(seq $((0x4E00)) $((0x9FFF))) $(seq $((0xAC00)) $((0xD7A3))); do
        line+="\\U$(printf '%08x' "$cp")"
        n=$((n + 1))
        [ $((n % 40)) -eq 0 ] && { printf '%s\n' "$line"; line=""; }
    done >"$dir/glyphs.esc"
    cat >"$dir/child.sh" <<'CHILD'
#!/bin/bash
dir=$1
for pass in 1 2; do
    while [ ! -e "$dir/go$pass" ]; do sleep 0.1; done
    n=0
    while IFS= read -r line; do
        printf '%b\r\n' "$line"
        n=$((n + 1))
        [ $((n % 24)) -eq 0 ] && sleep 0.05
    done <"$dir/glyphs.esc"
    printf 'ATLAS SOAK PASS %s DONE\r\n' "$pass"
    : >"$dir/done$pass"
done
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    LC_ALL=C.UTF-8 launch atlas "$dir/child.sh" "$dir" || { fail atlas "no window"; return; }
    rss_now() { awk '/^VmRSS/{print $2}' "/proc/$NORA_PID/status" 2>/dev/null; }
    local pass rss peak start1 end1
    for pass in 1 2; do
        xdotool key ctrl+shift+equal ctrl+shift+equal
        sleep 0.3
        [ "$pass" = 2 ] && end1=$(rss_now)
        [ "$pass" = 1 ] && start1=$(rss_now)
        peak=$(rss_now)
        : >"$dir/go$pass"
        for _ in $(seq 1 600); do
            rss=$(rss_now)
            [ "${rss:-0}" -gt "$peak" ] && peak=$rss
            [ -e "$dir/done$pass" ] && break
            sleep 0.1
        done
        [ -e "$dir/done$pass" ] || { finish; fail atlas "pass $pass did not finish"; return; }
    done
    sleep 0.5
    shot "$dir/shot.png"
    finish
    local white failures growth
    white=$(count_color "$dir/shot.png" '#FFFFFF')
    failures=$(grep -c 'frame draw failed' "$OUT/atlas/nora.log")
    growth=$(( (peak - end1) / 1024 ))
    echo "  rows: $(wc -l <"$dir/glyphs.esc"); rss ${start1} kB, after pass 1 ${end1} kB, pass 2 peak ${peak} kB (+${growth} MiB); text px $white; failed frames $failures"
    [ "$failures" -eq 0 ] || { fail atlas "$failures frame(s) failed: $(grep -m1 'frame draw failed' "$OUT/atlas/nora.log")"; return; }
    [ "$white" -gt 5000 ] || { fail atlas "text missing after the soak"; return; }
    [ "$growth" -le 16 ] || { fail atlas "memory grew ${growth} MiB in the second pass"; return; }
    pass atlas
}

# Prompt navigation (OSC 133 from the zsh integration): Ctrl+Shift+Z
# brings the previous prompt to the top row (the current prompt is not a
# target, and there is nothing before the first), and Ctrl+Shift+G
# selects the previous command's output. The prompt has a red background
# so its row can be found in a screenshot.
scenario_prompts() {
    have zsh prompts || return
    local dir="$OUT/prompts"; rm -rf "$dir"; mkdir -p "$dir/zdot"
    cat >"$dir/zdot/.zshrc" <<RC
PROMPT='%K{red}PR%k %% '
bindkey -e
source "$ROOT/shell/nora-zsh.zsh"
RC
    ZDOTDIR="$dir/zdot" NORA_TRACE_SHELL=1 launch prompts zsh -d -i \
        || { fail prompts "no window"; return; }
    for _ in $(seq 1 100); do
        grep -q '^shell: buffer "" cursor 0' "$OUT/prompts/nora.log" && break
        sleep 0.1
    done
    xdotool type --delay 30 "seq 1 100"; xdotool key Return; sleep 1
    shot "$dir/bottom.png"
    xdotool key ctrl+shift+z; sleep 0.3
    shot "$dir/jump.png"
    xdotool key ctrl+shift+z; sleep 0.3
    shot "$dir/again.png"
    xdotool key shift+End ctrl+shift+g; sleep 0.3
    shot "$dir/select.png"
    finish
    # Red pixels in the top cell row and in the whole window.
    top_red() {
        "${IM_CONVERT[@]}" "$1" -crop 1024x16+0+0 +repage "$1.top.png"
        count_color "$1.top.png" '#CD0000'
    }
    local bottom jump again sel
    bottom=$(top_red "$dir/bottom.png")
    jump=$(top_red "$dir/jump.png")
    again=$(top_red "$dir/again.png")
    sel=$(count_color "$dir/select.png" '#335577')
    echo "  top-row prompt px: at bottom $bottom, after jump $jump, after second jump $again; selection px $sel"
    [ "$bottom" -eq 0 ] || { fail prompts "prompt in the top row before the jump"; return; }
    [ "$jump" -gt 20 ] || { fail prompts "previous prompt not at the top after Ctrl+Shift+Z"; return; }
    [ "$again" -eq "$jump" ] || { fail prompts "second jump moved the view (no older prompt)"; return; }
    [ "$sel" -gt 5000 ] || { fail prompts "Ctrl+Shift+G did not select the command output"; return; }
    pass prompts
}

# IME with a real input method (ibus over XIM, the simple engine's
# Ctrl+Shift+U hex entry): the preedit is drawn and hides the inline
# suggestion while composing, and the commit reaches the shell once.
scenario_ime() {
    have ibus-daemon ime || return
    have xprop ime || return
    have dbus-daemon ime || return
    have dbus-monitor ime || return
    have zsh ime || return
    local dir="$OUT/ime"; rm -rf "$dir"; mkdir -p "$dir/zdot" "$dir/ibus"
    cat >"$dir/zdot/.zshrc" <<RC
PROMPT='%% '
bindkey -e
source "$ROOT/shell/nora-zsh.zsh"
RC
    # A private session bus and ibus with an isolated configuration; the
    # abstract socket avoids the 108-byte limit on socket paths.
    local bus_info bus_pid ibus_pid
    bus_info=$(dbus-daemon --session --fork --print-address=1 --print-pid=1)
    bus_pid=$(echo "$bus_info" | sed -n 2p)
    export DBUS_SESSION_BUS_ADDRESS=$(echo "$bus_info" | sed -n 1p)
    HOME="$dir/ibus" XDG_CONFIG_HOME="$dir/ibus/.config" XDG_CACHE_HOME="$dir/ibus/.cache" \
        ibus-daemon --xim --panel=disable --replace \
        --address="unix:abstract=nora-e2e-ibus-$$" >"$dir/ibus.log" 2>&1 &
    ibus_pid=$!
    local engine=""
    for _ in $(seq 1 50); do
        engine=$(HOME="$dir/ibus" XDG_CONFIG_HOME="$dir/ibus/.config" \
            ibus engine xkb:us::eng 2>/dev/null && echo set) && break
        sleep 0.1
    done
    # The cursor area nora reports reaches ibus as SetCursorLocation.
    dbus-monitor --address "unix:abstract=nora-e2e-ibus-$$" >"$dir/monitor.txt" 2>&1 &
    local monitor_pid=$!
    stop_ime() { kill "$monitor_pid" "$ibus_pid" "$bus_pid" 2>/dev/null; unset DBUS_SESSION_BUS_ADDRESS; }
    [ "$engine" = set ] || { stop_ime; fail ime "ibus did not start"; return; }
    # The XIM frontend registers on the display after the daemon is up;
    # a client started before that falls back to no input method.
    local xim=no
    for _ in $(seq 1 100); do
        xprop -root XIM_SERVERS 2>/dev/null | grep -q '@server=ibus' && { xim=yes; break; }
        sleep 0.1
    done
    [ "$xim" = yes ] || { stop_ime; fail ime "ibus did not register its XIM server"; return; }
    XMODIFIERS=@im=ibus ZDOTDIR="$dir/zdot" NORA_TRACE_SHELL=1 launch ime zsh -d -i \
        || { stop_ime; fail ime "no window"; return; }
    for _ in $(seq 1 100); do
        grep -q '^shell: buffer "" cursor 0' "$OUT/ime/nora.log" && break
        sleep 0.1
    done
    xdotool type --delay 30 "echo ime-beta-1 >> $dir/out.txt"; xdotool key Return; sleep 0.5
    shot "$dir/plain.png"
    xdotool type --delay 30 "echo ime"; sleep 0.7
    shot "$dir/ghost.png"
    xdotool key ctrl+shift+u; xdotool type --delay 50 "00e9"; sleep 0.5
    shot "$dir/preedit.png"
    local composing_lines
    composing_lines=$(wc -l <"$dir/monitor.txt")
    eval "$(xdotool getwindowgeometry --shell "$WIN" | grep -E '^(X|Y|WIDTH|HEIGHT)=')"
    xdotool key space; sleep 0.3
    xdotool type --delay 30 " >> $dir/out.txt"; xdotool key Return
    for _ in $(seq 1 30); do [ "$(wc -l <"$dir/out.txt" 2>/dev/null)" -ge 2 ] && break; sleep 0.1; done
    finish
    stop_ime
    local plain ghost preedit
    plain=$(count_color "$dir/plain.png" '#666666')
    ghost=$(count_color "$dir/ghost.png" '#666666')
    preedit=$(count_color "$dir/preedit.png" '#666666')
    echo "  ghost px: before $plain, suggested $ghost, composing $preedit; out: $(tr '\n' '|' <"$dir/out.txt")"
    grep -q 'IME: preedit = "u00e9"' "$OUT/ime/nora.log" || { fail ime "no preedit from ibus"; return; }
    [ "$ghost" -gt $((plain + 50)) ] || { fail ime "no ghost text before composing"; return; }
    [ "$preedit" -le $((plain + 50)) ] || { fail ime "ghost text drawn while composing"; return; }
    [ "$(cat "$dir/out.txt")" = "$(printf 'ime-beta-1\nimeé')" ] \
        || { fail ime "committed text differs: $(tr '\n' '|' <"$dir/out.txt")"; return; }
    # Candidate placement: while composing at row 1, column 10 ("% echo ime"),
    # the XIM spot is the bottom-left of the cursor cell in root coordinates
    # (the window is sized to the 80x24 grid).
    local spot want
    spot=$(head -n "$composing_lines" "$dir/monitor.txt" \
        | awk '/InputContext.*member=SetCursorLocation/{getline x; getline y; split(x, a, " "); split(y, b, " "); s=a[2] "," b[2]} END{print s}')
    want="$((X + 10 * WIDTH / 80)),$((Y + 2 * HEIGHT / 24))"
    echo "  ime spot: $spot (expected $want)"
    [ "$spot" = "$want" ] || { fail ime "candidate spot $spot, expected $want (bottom-left of the cursor cell)"; return; }
    pass ime
}

# Neovim with the kitty keyboard protocol: it queries `CSI ? u` and turns
# the protocol on when answered. Ctrl+I and Tab are then distinct keys
# (both are 0x09 in legacy mode), and Escape arrives as `CSI 27u`.
scenario_nvim() {
    have nvim nvim || return
    # The protocol query arrived in Neovim 0.10.
    local version
    version=$(nvim --version | head -1 | sed -n 's/^NVIM v\([0-9]*\)\.\([0-9]*\).*/\1 \2/p')
    set -- $version
    if [ "${1:-0}" -eq 0 ] && [ "${2:-0}" -lt 10 ]; then
        echo "SKIP nvim: $(nvim --version | head -1) predates the kitty keyboard query"
        return
    fi
    local dir="$OUT/nvim"; mkdir -p "$dir"
    rm -f "$dir/out.txt"
    launch nvim nvim --clean -c 'inoremap <C-i> [ci]' "$dir/out.txt" \
        || { fail nvim "no window"; return; }
    sleep 1
    xdotool type --delay 50 "ia"
    xdotool key ctrl+i
    xdotool type --delay 50 "b"
    xdotool key Tab
    xdotool type --delay 50 "c"
    xdotool key Escape
    xdotool type --delay 50 ":wq"
    xdotool key Return
    wait_file "$dir/out.txt"
    finish
    local got
    got=$(od -An -c "$dir/out.txt" 2>/dev/null | tr -s ' \n' ' ' | sed 's/^ //; s/ $//')
    echo "  file: $got"
    [ "$(cat "$dir/out.txt" 2>/dev/null)" = "$(printf 'a[ci]b\tc')" ] \
        || { fail nvim "expected a[ci]b<Tab>c (Ctrl+I distinct from Tab)"; return; }
    pass nvim
}

# An SSH session (v0.1 acceptance 1): an unprivileged sshd on a loopback
# port with throwaway keys; an interactive remote shell (with an isolated
# HOME) runs a typed command and exits.
scenario_ssh() {
    have sshd ssh || return
    have ssh-keygen ssh || return
    local dir="$OUT/ssh"; rm -rf "$dir"; mkdir -p "$dir/rhome"
    ssh-keygen -q -t ed25519 -N '' -f "$dir/host_key"
    ssh-keygen -q -t ed25519 -N '' -f "$dir/user_key"
    cp "$dir/user_key.pub" "$dir/authorized_keys"; chmod 600 "$dir/authorized_keys"
    local port=$((22000 + $$ % 1000))
    cat >"$dir/sshd_config" <<CONF
Port $port
ListenAddress 127.0.0.1
HostKey $dir/host_key
PidFile $dir/sshd.pid
AuthorizedKeysFile $dir/authorized_keys
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
CONF
    "$(command -v sshd)" -f "$dir/sshd_config" -D -e >"$dir/sshd.log" 2>&1 &
    local sshd_pid=$!
    for _ in $(seq 1 50); do grep -q 'Server listening' "$dir/sshd.log" && break; sleep 0.1; done
    launch ssh ssh -tt -p "$port" -i "$dir/user_key" -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null 127.0.0.1 "HOME=$dir/rhome ENV= PS1='remote\$ ' sh -i" \
        || { kill "$sshd_pid"; fail ssh "no window"; return; }
    sleep 1
    xdotool type --delay 20 "echo ssh-ok \$SSH_CONNECTION > $dir/out.txt"
    xdotool key Return
    wait_file "$dir/out.txt"
    xdotool type --delay 20 "exit"
    xdotool key Return
    local exited=no
    for _ in $(seq 1 50); do kill -0 "$NORA_PID" 2>/dev/null || { exited=yes; break; }; sleep 0.1; done
    finish
    kill "$sshd_pid" 2>/dev/null
    echo "  remote: $(cat "$dir/out.txt" 2>/dev/null)"
    grep -q '^ssh-ok 127.0.0.1 ' "$dir/out.txt" 2>/dev/null \
        || { fail ssh "the remote shell did not run the command: $(tail -2 "$dir/sshd.log")"; return; }
    [ "$exited" = yes ] || { fail ssh "nora did not exit with the session"; return; }
    pass ssh
}

# less (v0.1 acceptance 1): a search highlights the match on the
# alternate screen, and quitting restores the shell's screen.
scenario_less() {
    have less less || return
    local dir="$OUT/less"; mkdir -p "$dir"
    seq -f 'line %g' 1 500 >"$dir/file.txt"
    cat >"$dir/child.sh" <<CHILD
#!/bin/sh
printf 'BEFORE LESS\n'
LESS= LESSHISTFILE=- less "$dir/file.txt"
echo \$? >"$dir/status"
sleep 30
CHILD
    chmod +x "$dir/child.sh"
    launch less "$dir/child.sh" || { fail less "no window"; return; }
    sleep 0.5
    xdotool type --delay 30 "/line 321"
    xdotool key Return
    sleep 0.5
    shot "$dir/search.png"
    xdotool key q
    wait_file "$dir/status"
    sleep 0.3
    shot "$dir/after.png"
    finish
    local hits before
    # The match is drawn in reverse video: light cells on the dark screen.
    hits=$(count_color "$dir/search.png" '#E5E5E5')
    hits=$((hits + $(count_color "$dir/search.png" '#FFFFFF')))
    before=$(count_color "$dir/after.png" '#FFFFFF')
    echo "  status: $(cat "$dir/status" 2>/dev/null); light px while searching: $hits; after quit: $before"
    [ "$(cat "$dir/status" 2>/dev/null)" = 0 ] || { fail less "less did not exit 0"; return; }
    [ "$hits" -gt 500 ] || { fail less "no highlighted match"; return; }
    [ "$before" -gt 100 ] || { fail less "the shell screen was not restored after quitting"; return; }
    pass less
}

# tmux: a full-screen multiplexer draws its status line (default green
# background) and its pane content.
scenario_tmux() {
    have tmux tmux || return
    local dir="$OUT/tmux"; mkdir -p "$dir"
    # tmux 3.8 defaults to `bg=themegreen` (not ANSI green); pin the 3.x
    # default so the check is the same on every version.
    echo "set -g status-style bg=green,fg=black" >"$dir/tmux.conf"
    launch tmux tmux -L nora-e2e -f "$dir/tmux.conf" new-session "printf 'TMUX PANE TEXT\\n'; sleep 30" \
        || { fail tmux "no window"; return; }
    sleep 1.5
    shot "$dir/shot.png"
    finish
    tmux -L nora-e2e kill-server 2>/dev/null
    local green white
    green=$(count_color "$dir/shot.png" '#00CD00')
    white=$(count_color "$dir/shot.png" '#FFFFFF')
    echo "  status-bar px: $green, text px: $white"
    [ "$green" -gt 5000 ] || { fail tmux "no status line"; return; }
    [ "$white" -gt 100 ] || { fail tmux "no pane text"; return; }
    pass tmux
}

# htop: colored meters and the process list render.
scenario_htop() {
    have htop htop || return
    local dir="$OUT/htop"; mkdir -p "$dir"
    launch htop htop || { fail htop "no window"; return; }
    sleep 2
    shot "$dir/shot.png"
    xdotool key q
    finish
    local colors
    colors=$("${IM_CONVERT[@]}" "$dir/shot.png" -crop 800x600+0+0 +repage -unique-colors -format '%w' info:)
    echo "  distinct colors: $colors"
    [ "$colors" -gt 6 ] || { fail htop "too few colors rendered"; return; }
    pass htop
}

ALL=(render tty resize keys layout kitty mouse redraw selection zoom flood latency viewlat title panes tinysplit paneexit search config reports hyperlink idle scale atlas zshell suggest prompts ime vim nvim less ssh tmux htop)
SELECTED=("$@")
[ ${#SELECTED[@]} -gt 0 ] || SELECTED=("${ALL[@]}")
for s in "${SELECTED[@]}"; do "scenario_$s"; done
echo "e2e: $FAILED failure(s); artifacts in $OUT"
[ "$FAILED" -eq 0 ]
