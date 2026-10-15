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
# PROMPT_COMMAND and installs a DEBUG trap, and it does not source
# anything else. Sourcing it twice in the same shell is a no-op.

# Emit an OSC 133 marker: `ESC ] 133 ; <kind> ST`.
__nora_osc133() {
    printf '\033]133;%s\033\\' "$1"
}

# Runs before each prompt is drawn (via PROMPT_COMMAND).
# Closes the previous command's output (D) and opens the new prompt (A).
__nora_prompt_start() {
    __nora_osc133 D
    __nora_osc133 A
}

# Runs before each command executes (via the DEBUG trap).
# Marks the command start (B) and the output start (C).
__nora_command_start() {
    __nora_osc133 B
    __nora_osc133 C
}

# Append our hook to the existing PROMPT_COMMAND (preserving any the user
# already set). Guard against double-install.
case "${PROMPT_COMMAND:-}" in
    *__nora_prompt_start*) ;;
    *) PROMPT_COMMAND="${PROMPT_COMMAND:+$PROMPT_COMMAND; }__nora_prompt_start" ;;
esac

# Install the DEBUG trap (guard against double-install).
case "$(trap -p DEBUG)" in
    *__nora_command_start*) ;;
    *) trap '__nora_command_start' DEBUG ;;
esac
