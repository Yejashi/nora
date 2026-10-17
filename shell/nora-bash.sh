# nora OSC 133 shell integration (N35a).
#
# Source this file from your .bashrc to enable OSC 133 prompt markers for
# the nora terminal. It emits four markers around the prompt and command
# output:
#   A — prompt start (before the prompt is drawn)
#   B — command start (after the prompt, before the command)
#   C — output start (after the command, before the output)
#   D — output end (after the output, before the next prompt)
#
# Safe to source in a normal bash session: it only appends to
# PROMPT_COMMAND and, at the first prompt, adds to the DEBUG trap (a DEBUG
# trap set before that keeps running first), and it does not source
# anything else. Sourcing it twice in the same shell is a no-op; it does
# nothing in a non-interactive shell.
#
# Keep `__nora_prompt_start` the last PROMPT_COMMAND entry (hooks such as
# direnv, zoxide and starship prepend themselves, which is fine): the first
# command after it is taken as the user's command.

[[ $- == *i* ]] || return 0
[[ -n ${__nora_bash_loaded-} ]] && return 0
__nora_bash_loaded=1

# The terminal, as the fd the markers are written to. The DEBUG trap runs
# with the command's redirections in effect (`{ echo a; } > out.txt`), so
# B and C written to stdout would land in the file. The fd is dup'ed from
# stdout (the terminal) at each prompt and closed as the command line's
# first command starts, so commands never inherit it (a background daemon
# holding it would keep the terminal open).
__nora_fd=

# Open the terminal fd afresh (stdout at prompt time is the terminal).
__nora_open_fd() {
    [[ -n $__nora_fd ]] && exec {__nora_fd}>&-
    exec {__nora_fd}>&1
}

# Close the terminal fd before the user's commands run.
__nora_close_fd() {
    [[ -n $__nora_fd ]] && exec {__nora_fd}>&-
    __nora_fd=
}

# Emit an OSC 133 marker: `ESC ] 133 ; <kind> ST`.
__nora_osc133() {
    printf '\033]133;%s\033\\' "$1" >&"${__nora_fd:-1}"
}

# Report the working directory: `ESC ] 7 ; file://<host><path> ST`, the
# path percent-encoded byte by byte. The host names the machine the path
# is on (over ssh, the remote).
__nora_osc7() {
    local LC_ALL=C s=$PWD out= ch hex i
    for (( i = 0; i < ${#s}; i++ )); do
        ch=${s:i:1}
        case $ch in
            [A-Za-z0-9._~/-]) out+=$ch ;;
            *) printf -v hex '%%%02X' "'$ch"; out+=$hex ;;
        esac
    done
    printf '\033]7;file://%s%s\033\\' "${HOSTNAME-}" "$out" >&"${__nora_fd:-1}"
}

# Set once a prompt is drawn, cleared by the first command after it.
__nora_at_prompt=

# Runs before each prompt is drawn (via PROMPT_COMMAND).
# Closes the previous command's output (D) and opens the new prompt (A).
# Until the DEBUG trap is installed, `$1` is the `trap -p DEBUG` output
# taken by the PROMPT_COMMAND entry itself (a function cannot see it).
__nora_prompt_start() {
    [[ -n ${__nora_debug_installed-} ]] || __nora_install_debug_trap "$1"
    __nora_open_fd
    __nora_osc133 D
    __nora_osc7
    __nora_osc133 A
    __nora_at_prompt=1
}

# Runs before each simple command (via the DEBUG trap); `$1` is the
# status to return (the chained DEBUG trap's, so `extdebug` users keep
# its verdict) and `$2` the command's `$_`: as the call's last argument
# it is what `$_` holds after the trap (`mkdir -p d && cd $_`).
#
# Marks the command start (B) and the output start (C) once per command
# line: only the first command after a prompt counts, so loops and
# pipelines do not repeat the pair mid-output, and the PROMPT_COMMAND
# entries that run before `__nora_prompt_start` (also traced) emit
# nothing. Completion functions and `bind -x` widgets run while editing
# and are skipped.
__nora_command_start() {
    if [[ -n $__nora_at_prompt && -z ${COMP_LINE-} && -z ${READLINE_POINT+set} ]]; then
        __nora_at_prompt=
        __nora_osc133 B
        __nora_osc133 C
        __nora_close_fd
    fi
    return "${1:-0}"
}

# Set __nora_prev_debug to the command text in `trap -p DEBUG` output
# `$1` (`trap -- '<text>' DEBUG`, quoted for reuse), or nothing.
__nora_trap_text() {
    __nora_prev_debug=
    [[ -n $1 ]] || return 0
    eval "set -- $1"
    __nora_prev_debug=${3-}
}

# Keep the command's `$_` in __nora_underscore for after a chained trap
# (which may change it), returning `$1` so that trap still sees the
# command's `$?`; as the call's last argument, `$_` is unchanged for it.
__nora_save_underscore() {
    __nora_underscore=$2
    return "$1"
}

# Chain onto the DEBUG trap `$1` (`trap -p DEBUG` output) instead of
# replacing it: it runs first (it sees the command's `$_` and `$?`), then
# ours. Done at the first prompt with the trap read at the top level:
# without `set -T`, both a sourced file and a function see no DEBUG trap.
__nora_install_debug_trap() {
    __nora_debug_installed=1
    __nora_trap_text "$1"
    case "$__nora_prev_debug" in
        *__nora_command_start*) ;;
        '') trap '__nora_command_start 0 "$_"' DEBUG ;;
        *) trap '__nora_save_underscore "$?" "$_"'$'\n'"$__nora_prev_debug"$'\n''__nora_command_start "$?" "$__nora_underscore"' DEBUG ;;
    esac
    unset __nora_prev_debug
}

# Append our hook to the existing PROMPT_COMMAND (preserving any the user
# already set). The trap is read only while not yet installed (one
# command substitution, at the first prompt). An array PROMPT_COMMAND
# (bash 5.1+) gets a new last element; a string one gets a new line, which
# is valid after any text (`history -a;` or `cmd &` would make a `; `
# join a syntax error).
__nora_prompt_hook='__nora_prompt_start "${__nora_debug_installed:-$(trap -p DEBUG)}"'
case "${PROMPT_COMMAND[*]-}" in
    *__nora_prompt_start*) ;;
    *)
        if [[ $(declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a'* ]]; then
            PROMPT_COMMAND+=("$__nora_prompt_hook")
        else
            PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}$__nora_prompt_hook"
        fi
        ;;
esac
unset __nora_prompt_hook
