# Hutan shell integration for bash: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Hutan starts bash as `bash --rcfile <this file> ...`, so an interactive bash sources this
# file instead of ~/.bashrc. The trampoline below replays the startup sequence bash would have
# run (for a login shell, /etc/profile and the first of ~/.bash_profile, ~/.bash_login,
# ~/.profile; otherwise ~/.bashrc or the user's own --rcfile) and only then installs the
# hooks, so the user's files run exactly once and before the integration. (`--posix` + $ENV,
# the other usual route, is not used: bash 3.2, macOS's /bin/bash, ignores it for startup.)
# The file can also be sourced by hand from ~/.bashrc:
#   source "$HUTAN_SHELL_INTEGRATION_DIR/bash/hutan.bash"
#
# The user's files are sourced here at the top level, never inside a function, so their
# `declare`s behave as in a normal startup.

case $- in
  *i*) ;;
  *) return 0 2>/dev/null || exit 0 ;;
esac

if [ -n "${HUTAN_BASH_INJECT+set}" ]; then
  # Flags of the original command line: login, norc, noprofile.
  __hutan_inject=" $HUTAN_BASH_INJECT "
  __hutan_rcfile=${HUTAN_BASH_RCFILE-}
  unset HUTAN_BASH_INJECT HUTAN_BASH_RCFILE

  case $__hutan_inject in
    *" login "*)
      case $__hutan_inject in
        *" noprofile "*) ;;
        *)
          [ -r /etc/profile ] && . /etc/profile
          for __hutan_file in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
            if [ -r "$__hutan_file" ]; then
              . "$__hutan_file"
              break
            fi
          done
          ;;
      esac
      ;;
    *)
      case $__hutan_inject in
        *" norc "*) ;;
        *)
          if [ -n "$__hutan_rcfile" ]; then
            [ -r "$__hutan_rcfile" ] && . "$__hutan_rcfile"
          else
            [ -r "$HOME/.bashrc" ] && . "$HOME/.bashrc"
          fi
          ;;
      esac
      ;;
  esac
  unset __hutan_inject __hutan_rcfile __hutan_file
fi

# Hooks are installed once per shell; the rest of the file only defines functions.
if [ -n "${__hutan_integration_loaded+set}" ]; then
  return 0
fi
__hutan_integration_loaded=1
# 0 = no command since the last prompt, 1 = a command is running.
__hutan_command_running=0
# DEBUG-trap mode only (bash < 4.4): 1 between the end of PROMPT_COMMAND and the next command.
__hutan_armed=0

__hutan_osc7() {
  local path_encoded=$PWD
  case $PWD in
    *[!A-Za-z0-9/._~-]*)
      # Percent-encode byte by byte (the C locale makes ${PWD:i:1} a byte).
      local LC_ALL=C ch i code hex
      path_encoded=
      for ((i = 0; i < ${#PWD}; i++)); do
        ch=${PWD:i:1}
        case $ch in
          [A-Za-z0-9/._~-]) path_encoded+=$ch ;;
          *)
            # printf reads a byte >= 0x80 as a negative char; keep the low byte.
            printf -v code '%d' "'$ch"
            printf -v hex '%%%02X' $((code & 255))
            path_encoded+=$hex
            ;;
        esac
      done
      ;;
  esac
  printf '\e]7;file://%s%s\a' "${HOSTNAME-}" "$path_encoded"
}

# First in PROMPT_COMMAND: the previous command is over, so report it. $? is passed on
# untouched to whatever PROMPT_COMMAND entries follow.
__hutan_precmd() {
  local exit_status=$?
  if [ "$__hutan_command_running" = 1 ]; then
    printf '\e]133;D;%s\a' "$exit_status"
    __hutan_command_running=0
  fi
  __hutan_osc7
  return "$exit_status"
}

# Last in PROMPT_COMMAND: prompt frameworks rebuild PS1 in their own entries, so the marks are
# (re)added here; a PS1 that already carries OSC 133 is left alone.
__hutan_postprompt() {
  local exit_status=$?
  case $PS1 in
    *'\e]133;'*) ;;
    *) PS1='\[\e]133;A\a\]'$PS1'\[\e]133;B\a\]' ;;
  esac
  __hutan_armed=1
  return "$exit_status"
}

# bash < 4.4 has no PS0: the DEBUG trap fires before every simple command, and the first one
# after PROMPT_COMMAND ran is the user's command.
__hutan_preexec() {
  if [ "$__hutan_armed" = 1 ] && [ -z "${COMP_POINT-}" ] && [ -z "${READLINE_POINT-}" ]; then
    __hutan_armed=0
    __hutan_command_running=1
    printf '\e]133;C\a'
  fi
}

__hutan_install() {
  # PROMPT_COMMAND: an array from bash 5.1, a string before.
  case $(declare -p PROMPT_COMMAND 2>/dev/null) in
    "declare -a"*)
      PROMPT_COMMAND=(__hutan_precmd "${PROMPT_COMMAND[@]}" __hutan_postprompt)
      ;;
    *)
      PROMPT_COMMAND=$'__hutan_precmd\n'${PROMPT_COMMAND:+$PROMPT_COMMAND$'\n'}'__hutan_postprompt'
      ;;
  esac

  if ((BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4))); then
    # PS0 is expanded just before the command runs, in the shell itself, so the arithmetic in
    # the substring offset (of length 0, so it expands to nothing) records that a command started.
    PS0=${PS0-}'\e]133;C\a${__hutan_command_running:$((__hutan_command_running=1)):0}'
  fi
}

# Splits the output of `trap -p DEBUG` (`trap -- 'body' DEBUG`) into its body.
__hutan_trap_body() {
  eval "set -- $1"
  __hutan_previous_body=${3-}
}

__hutan_install
unset -f __hutan_install

# bash < 4.4 has no PS0: install the DEBUG trap. It is set here, not inside a function: bash
# 3.2 undoes a trap set in a function when the function returns. A trap the user already has
# is kept and runs after ours; `trap -p` inside $(...) sees no traps in bash 3.2 (command
# substitutions reset them), so its output goes through a file.
if ((BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4))); then
  __hutan_trap_file=$(mktemp "${TMPDIR:-/tmp}/hutan-trap.XXXXXX" 2>/dev/null) || __hutan_trap_file=
  __hutan_previous_body=
  if [ -n "$__hutan_trap_file" ]; then
    trap -p DEBUG >"$__hutan_trap_file"
    __hutan_trap_body "$(<"$__hutan_trap_file")"
    rm -f "$__hutan_trap_file"
  fi
  if [ -n "$__hutan_previous_body" ]; then
    eval "__hutan_previous_debug_trap() { $__hutan_previous_body
}"
    trap '__hutan_preexec; __hutan_previous_debug_trap' DEBUG
  else
    trap '__hutan_preexec' DEBUG
  fi
  unset __hutan_trap_file __hutan_previous_body
fi
unset -f __hutan_trap_body

# Agents' own OpenTelemetry (TEL-2, ADR 0026). While the daemon's loopback receiver listens, a
# Hutan terminal has HUTAN_OTLP_ENDPOINT and HUTAN_OTLP_TOKEN; `claude` and `codex` then become
# functions that ask `hutan telemetry env <agent>` what to put in front of that one launch (nothing
# unless the agent is enabled and nothing of yours, a managed policy's or the agent's own is in the
# way: it never overrides an OTEL_* variable or a setting) and run the program found on PATH with
# it. A `claude` or `codex` function or alias of your own is left alone.
if [ -n "${HUTAN_OTLP_ENDPOINT-}" ] && [ -n "${HUTAN_BIN-}" ]; then
  __hutan_otlp_launch() {
    local agent=$1 line
    shift
    local -a launch_env launch_args
    launch_env=()
    launch_args=()
    if [ -x "$HUTAN_BIN" ] && [ -n "$(type -P "$agent" 2>/dev/null)" ]; then
      while IFS= read -r line; do
        case $line in
          "env "*) launch_env[${#launch_env[@]}]=${line#env } ;;
          "arg "*) launch_args[${#launch_args[@]}]=${line#arg } ;;
        esac
      done <<<"$("$HUTAN_BIN" telemetry env "$agent" 2>/dev/null)"
    fi
    if [ "${#launch_env[@]}" -gt 0 ]; then
      command env "${launch_env[@]}" "$agent" ${launch_args[@]+"${launch_args[@]}"} "$@"
    else
      command "$agent" "$@"
    fi
  }
  for __hutan_agent in claude codex; do
    if ! declare -F "$__hutan_agent" >/dev/null 2>&1 && ! alias "$__hutan_agent" >/dev/null 2>&1; then
      eval "$__hutan_agent() { __hutan_otlp_launch $__hutan_agent \"\$@\"; }"
    fi
  done
  unset __hutan_agent
fi
