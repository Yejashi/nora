#!/usr/bin/env bash
# Reproducible performance baseline for the release binary, on whatever
# display is available (a real session, or Xvfb with --xvfb).
#
# Measures:
#   startup     wall time of `nora -- true` (window, GPU init, fonts, first
#               frame, child spawn and exit), median of 5
#   throughput  `nora -- cat FILE` on a 32 MiB text corpus, minus startup
#               and teardown (the `nora -- true` median)
#   frames      per-phase frame cost from NORA_TRACE_FRAMES during the
#               throughput run (median / p95)
#   memory      peak RSS (VmHWM) during the throughput run
#
# Usage: scripts/perf.sh [--xvfb] [--size WxH] [--font-size PT]
#   --font-size runs with a temporary config at that size (a small font
#   gives a large grid where a window cannot be resized, e.g. Wayland).
# Env:   NORA_BIN (default target/release/nora)
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NORA_BIN=${NORA_BIN:-$ROOT/target/release/nora}
OUT=${PERF_OUT:-$ROOT/target/perf}
mkdir -p "$OUT"
XVFB=no SIZE="" FONT_SIZE=""
while [ $# -gt 0 ]; do
    case $1 in
        --xvfb) XVFB=yes ;;
        --size) SIZE=$2; shift ;;
        --font-size) FONT_SIZE=$2; shift ;;
    esac
    shift
done
if [ "$XVFB" = yes ]; then
    Xvfb :95 -screen 0 3000x2000x24 -nolisten tcp >/dev/null 2>&1 &
    XVFB_PID=$!
    trap 'kill $XVFB_PID 2>/dev/null' EXIT
    export DISPLAY=:95
    unset WAYLAND_DISPLAY
    sleep 1
fi

if [ -n "$FONT_SIZE" ]; then
    mkdir -p "$OUT/config/nora"
    printf '[font]\nsize = %s\n' "$FONT_SIZE" > "$OUT/config/nora/config.toml"
    export XDG_CONFIG_HOME=$OUT/config
fi

median() { sort -n | awk '{a[NR]=$1} END{if(NR==0){print "n/a"; exit} m=int((NR+1)/2); print a[m]}'; }
p95() { sort -n | awk '{a[NR]=$1} END{if(NR==0){print "n/a"; exit} i=int(NR*0.95+0.5); if(i<1)i=1; print a[i]}'; }
now_ms() { date +%s%3N; }

# Corpus: 32 MiB of printable lines with some SGR color, deterministic.
CORPUS=$OUT/corpus.txt
if [ ! -s "$CORPUS" ]; then
    awk 'BEGIN{
        for (i = 0; i < 335000; i++) {
            printf "%07d \033[3%dmcolor\033[0m ", i, i % 8
            for (j = 0; j < 8; j++) printf "lorem%02d ", (i + j) % 97
            printf "\n"
        }
    }' > "$CORPUS"
fi
BYTES=$(stat -c %s "$CORPUS")

# Optional window size (X11 only): resize the window right after it maps.
resize_window() {
    [ -n "$SIZE" ] && command -v xdotool >/dev/null || return 0
    local w h win
    w=${SIZE%x*}; h=${SIZE#*x}
    for _ in $(seq 1 50); do
        win=$(xdotool search --name '^nora$' 2>/dev/null | head -1)
        [ -n "$win" ] && break
        sleep 0.05
    done
    [ -n "$win" ] && xdotool windowsize "$win" "$w" "$h" 2>/dev/null
}

startups=()
for _ in 1 2 3 4 5; do
    t0=$(now_ms)
    "$NORA_BIN" -- true >/dev/null 2>&1
    startups+=($(( $(now_ms) - t0 )))
done
STARTUP=$(printf '%s\n' "${startups[@]}" | median)

# The child sleeps first so the optional resize lands before the stream.
NORA_TRACE_FRAMES=1 "$NORA_BIN" -- sh -c "sleep 1; cat '$CORPUS'" >"$OUT/run.log" 2>&1 &
PID=$!
resize_window
t0=$(now_ms)
PEAK=0
while kill -0 $PID 2>/dev/null; do
    hwm=$(awk '/^VmHWM/{print $2}' /proc/$PID/status 2>/dev/null)
    [ -n "$hwm" ] && [ "$hwm" -gt "$PEAK" ] && PEAK=$hwm
    sleep 0.1
done
TOTAL=$(( $(now_ms) - t0 ))
# Minus the child's 1 s sleep and nora's own start and teardown (the
# `nora -- true` median), leaving the time to consume the stream.
STREAM_MS=$(( TOTAL - 1000 - STARTUP ))
MBPS=$(awk -v b="$BYTES" -v ms="$STREAM_MS" 'BEGIN{printf "%.1f", b / 1048576 / (ms / 1000)}')

grep '^frame: ' "$OUT/run.log" > "$OUT/frames.txt"
FRAMES=$(wc -l < "$OUT/frames.txt")
GRID=$(awk '{print $NF}' "$OUT/frames.txt" | sort | uniq -c | sort -rn | head -1 | awk '{print $2}')
col() { awk -v k="$1" '{for(i=1;i<=NF;i++) if($i==k){print $(i+1)}}' "$OUT/frames.txt"; }
ADAPTER=$(grep -o 'adapter: [^|]*' "$OUT/run.log" | head -1 | sed 's/adapter: //; s/ *$//')

cat <<TABLE
| Metric | Value |
| --- | --- |
| Adapter | ${ADAPTER:-unknown} |
| Grid (most frames) | ${GRID:-n/a} |
| Startup, median of 5 (\`nora -- true\`) | ${STARTUP} ms |
| Throughput (32 MiB, \`cat\`) | ${MBPS} MiB/s (${STREAM_MS} ms) |
| Frames during the stream | ${FRAMES} |
| Frame total, median / p95 | $(col total | median) / $(col total | p95) ms |
| Scene build, median / p95 | $(col scene | median) / $(col scene | p95) ms |
| GPU encode, median / p95 | $(col encode | median) / $(col encode | p95) ms |
| Present, median / p95 | $(col present | median) / $(col present | p95) ms |
| Cells drawn per frame, median | $(col cells | median) |
| Peak RSS during the stream | $(( PEAK / 1024 )) MiB |
TABLE
