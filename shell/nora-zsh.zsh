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

# The terminal, as an fd that stays valid inside line-editor widgets.
exec {__nora_fd}>&1

__nora_osc() {
    builtin print -rnu $__nora_fd -- $'\e]'"$1"$'\e\\'
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

__nora_precmd() {
    local -i status_=$?
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
    # A command HISTORY_IGNORE keeps out of the history file is not
    # reported either, so nora does not record it. The match runs before
    # `emulate`, under the user's options, as zsh's own match does when it
    # writes the history file (`(#i)*secret*` needs the user's
    # EXTENDED_GLOB).
    local -i __nora_ignored=0
    [[ -n ${HISTORY_IGNORE-} && $1 == ${~HISTORY_IGNORE} ]] && __nora_ignored=1
    emulate -L zsh
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
