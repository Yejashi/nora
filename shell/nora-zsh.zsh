# nora shell integration for zsh.
#
# Source this file from your ~/.zshrc. It reports to the terminal:
#   OSC 133 A/B/C/D  prompt start, input start, command start (with the
#                    command line), command end (with the exit status)
#   OSC 7            the current directory
#   OSC 6973         the line editor's buffer and cursor on every redraw,
#                    and the end of editing (ADR-013)
#
# nora uses the buffer reports for inline suggestions; other terminals
# ignore unknown OSC sequences. Everything runs inside zsh (no external
# commands per keystroke). Sourcing it twice is a no-op; it does nothing
# in a non-interactive shell.

[[ -o interactive ]] || return 0
(( ${+__nora_zsh_loaded} )) && return 0
typeset -g __nora_zsh_loaded=1

# The terminal for the line-editor hooks, as an fd that stays valid when
# a widget runs zle under a redirection (`zle reset-prompt >/dev/null`
# would otherwise send the buffer reports there). zsh cannot mark it
# close-on-exec, so it is dup'ed from stdout (the terminal) when editing
# starts and closed when editing ends: commands, and daemons they start,
# never inherit it. precmd and preexec run outside the editor and write
# to stdout.
typeset -g __nora_fd=

__nora_osc() {
    builtin print -rnu ${__nora_fd:-1} -- $'\e]'"$1"$'\e\\'
}

# Percent-encode $1 (UTF-8 bytes) into REPLY, keeping unreserved and `/`.
__nora_urlencode() {
    emulate -L zsh
    setopt nomultibyte
    local s=$1 ch hex
    local -i i
    REPLY=
    for (( i = 1; i <= ${#s}; i++ )); do
        ch=${s[i]}
        if [[ $ch == [A-Za-z0-9._~/-] ]]; then
            REPLY+=$ch
        else
            builtin printf -v hex '%%%02X' "'$ch"
            REPLY+=$hex
        fi
    done
}

# Base64-encode $1 (UTF-8 bytes) into REPLY.
__nora_base64() {
    emulate -L zsh
    setopt nomultibyte
    local s=$1 t='ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
    local c1 c2 c3
    local -i i n=${#s} a b c v
    REPLY=
    for (( i = 1; i <= n; i += 3 )); do
        c1=${s[i]} c2=${s[i+1]} c3=${s[i+2]}
        a=$(( #c1 ))
        b=0 c=0
        (( i + 1 <= n )) && b=$(( #c2 ))
        (( i + 2 <= n )) && c=$(( #c3 ))
        v=$(( (a << 16) | (b << 8) | c ))
        REPLY+=${t[(v >> 18) + 1]}${t[((v >> 12) & 63) + 1]}
        if (( i + 1 <= n )); then REPLY+=${t[((v >> 6) & 63) + 1]}; else REPLY+='='; fi
        if (( i + 2 <= n )); then REPLY+=${t[(v & 63) + 1]}; else REPLY+='='; fi
    done
}

# The zshaddhistory verdict. zsh runs the `zshaddhistory` function and
# every `zshaddhistory_functions` entry and keeps the first non-zero
# status to itself: a hook cannot see the others' statuses, and a
# rejected line still sits in $history until the next line. So when
# editing ends (line-finish, after which no user code runs before the
# hooks), the user's hooks are moved aside and nora's dispatcher is the
# only one; it puts them back first, runs each once in zsh's order with
# zsh's rule, and records the status for preexec. precmd and preexec
# put them back too, should a line not reach the hooks. vared and select
# lines are left alone.
# Moving the function needs `functions -c` (zsh 5.8): an older zsh with
# a `zshaddhistory` function keeps its hooks in place, unseen.
typeset -gi __nora_hist_verdict=0
typeset -g __nora_hist_hidden=

__nora_hist_hide() {
    emulate -L zsh
    [[ -z $__nora_hist_hidden && -n $BUFFER && $CONTEXT == (start|cont) ]] || return 0
    local -i has_fn=${+functions[zshaddhistory]}
    (( has_fn || ${#zshaddhistory_functions[@]} )) || return 0
    if (( has_fn )); then
        functions -c zshaddhistory __nora_hist_fn 2>/dev/null || return 0
        unfunction zshaddhistory
    fi
    if (( ${+zshaddhistory_functions} )); then
        typeset -ga __nora_hist_fns=("${zshaddhistory_functions[@]}")
        __nora_hist_hidden=set$has_fn
    else
        __nora_hist_hidden=unset$has_fn
    fi
    typeset -ga zshaddhistory_functions=(__nora_addhistory)
}

__nora_hist_restore() {
    emulate -L zsh
    [[ -n $__nora_hist_hidden ]] || return 0
    if [[ $__nora_hist_hidden == set* ]]; then
        zshaddhistory_functions=("${__nora_hist_fns[@]}")
    else
        unset zshaddhistory_functions
    fi
    unset __nora_hist_fns
    if [[ $__nora_hist_hidden == *1 ]]; then
        functions -c __nora_hist_fn zshaddhistory
        unfunction __nora_hist_fn
    fi
    __nora_hist_hidden=
}

# Runs under the user's options, as zsh runs the hooks: only quoted
# expansions, and `||` so ERR_EXIT/ERR_RETURN cannot stop it midway.
__nora_addhistory() {
    __nora_hist_restore
    local -i __nora_ret=0 __nora_r=0
    local __nora_f
    if (( ${+functions[zshaddhistory]} )); then
        zshaddhistory "$@" || __nora_ret=$?
    fi
    if (( ${+zshaddhistory_functions} )); then
        for __nora_f in "${zshaddhistory_functions[@]}"; do
            (( ${+functions[$__nora_f]} )) || continue
            __nora_r=0
            "$__nora_f" "$@" || __nora_r=$?
            (( __nora_ret )) || __nora_ret=$__nora_r
        done
    fi
    __nora_hist_verdict=$__nora_ret
    return $__nora_ret
}

__nora_precmd() {
    local -i status_=$?
    __nora_hist_restore
    if (( ${+__nora_running} )); then
        __nora_osc "133;D;$status_"
        unset __nora_running
    fi
    local REPLY
    __nora_urlencode "$PWD"
    # The host names the machine the path is on (over ssh, the remote).
    __nora_osc "7;file://$HOST$REPLY"
    __nora_osc "133;A"
}

__nora_preexec() {
    # A command the user keeps out of the history file is not reported
    # either, so nora does not record it: one HISTORY_IGNORE matches, one
    # HIST_IGNORE_SPACE drops (a leading space), or one a zshaddhistory
    # hook rejected (any non-zero status, 2 included: nora's history is
    # persistent). The HISTORY_IGNORE match runs before `emulate`, under
    # the user's options, as zsh's own match does when it writes the
    # history file (`(#i)*secret*` needs the user's EXTENDED_GLOB).
    local -i __nora_ignored=0
    [[ -n ${HISTORY_IGNORE-} && $1 == ${~HISTORY_IGNORE} ]] && __nora_ignored=1
    [[ -o histignorespace && $1 == ' '* ]] && __nora_ignored=1
    emulate -L zsh
    __nora_hist_restore
    (( ${__nora_hist_verdict:-0} )) && __nora_ignored=1
    __nora_hist_verdict=0
    typeset -g __nora_running=1
    if (( __nora_ignored )); then
        __nora_osc "133;C"
        return
    fi
    local REPLY
    __nora_urlencode "$1"
    __nora_osc "133;C;cmdline_url=$REPLY"
}

typeset -g __nora_last_report=

__nora_line_init() {
    [[ -n $__nora_fd ]] || exec {__nora_fd}>&1
    __nora_osc "133;B"
    __nora_last_report=
    __nora_line_report
}

# Report the buffer when it or the cursor changed since the last report.
__nora_line_report() {
    local key="$CURSOR:$BUFFER" REPLY
    [[ $key == "$__nora_last_report" ]] && return
    __nora_last_report=$key
    __nora_base64 "$BUFFER"
    __nora_osc "6973;buffer;$CURSOR;$REPLY"
}

__nora_line_finish() {
    __nora_osc "6973;end"
    __nora_last_report=
    __nora_hist_verdict=0
    __nora_hist_hide
    if [[ -n $__nora_fd ]]; then
        exec {__nora_fd}>&-
        __nora_fd=
    fi
}

autoload -Uz add-zsh-hook add-zle-hook-widget
add-zsh-hook precmd __nora_precmd
add-zsh-hook preexec __nora_preexec
# Hook widgets must exist as widgets before they are attached.
zle -N __nora_line_init
zle -N __nora_line_report
zle -N __nora_line_finish
# add-zle-hook-widget runs under the caller's options; with the user's
# KSH_ARRAYS the line-init hook (133;B) never ran, so attach the hooks
# under zsh defaults.
() {
    emulate -L zsh
    add-zle-hook-widget line-init __nora_line_init
    add-zle-hook-widget line-pre-redraw __nora_line_report
    add-zle-hook-widget line-finish __nora_line_finish
}
