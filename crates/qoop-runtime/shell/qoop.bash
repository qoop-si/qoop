# qoop's bash integration. The daemon starts a tab's bash as `bash --rcfile <this> -i`, so
# this file runs in place of ~/.bashrc, and no file of the person's is edited for it.
if [ -r ~/.bashrc ]; then
  . ~/.bashrc
fi

# OSC 133 C as a command starts and D with its status at the next prompt: the daemon times
# the command from these, and nothing else is read from them. Only bash 4.4 and newer has
# PS0, which is printed after a line is read and before it runs; older bash gets no marks
# rather than a DEBUG trap, which fights bash-preexec and prompt themes that use it.
# QOOP_SHELL_INTEGRATION=0, set in ~/.bashrc or by hand, turns both off; read at each
# prompt, so it can come later.
# PS0 expands the variable only with promptvars on (the default); with it off it would print
# the variable's name before every command, so then there are no marks either.
if shopt -q promptvars && { [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; }; }; then
  __qoop_prompt() {
    local qoop_status=$?
    if [ "${QOOP_SHELL_INTEGRATION-}" = 0 ]; then
      __qoop_start=
    else
      printf '\e]133;D;%s\a' "$qoop_status"
      __qoop_start=$'\e]133;C\a'
    fi
    # The status goes back as it was, for the person's own PROMPT_COMMAND that runs next.
    return "$qoop_status"
  }
  # First, so the status it reads is the command's and not that of something before it.
  PROMPT_COMMAND="__qoop_prompt${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
  # A variable rather than the mark itself, so the switch above can empty it; PS0 expands
  # it at each command.
  PS0="${PS0-}"'${__qoop_start-}'
fi
