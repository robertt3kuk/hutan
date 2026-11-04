# Hutan shell integration for zsh: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Loaded automatically by Hutan's ZDOTDIR shim (zsh/.zshenv); it can also be sourced by hand
# from ~/.zshrc: source "$HUTAN_SHELL_INTEGRATION_DIR/zsh/hutan-integration.zsh"
# Hooks go through precmd_functions/preexec_functions; no user function is replaced.

[[ -o interactive ]] || return 0
(( ${+_hutan_integration_loaded} )) && return 0
builtin typeset -g _hutan_integration_loaded=1
# 0 = no command since the last prompt, 1 = a command is running.
builtin typeset -gi _hutan_command_running=0

_hutan_osc7() {
  builtin local path_encoded=$PWD
  if [[ $PWD == *[^A-Za-z0-9/._~-]* ]]; then
    # Percent-encode byte by byte (the C locale makes ${PWD[i]} a byte).
    builtin local LC_ALL=C ch
    builtin local -i i
    path_encoded=
    for (( i = 1; i <= ${#PWD}; ++i )); do
      ch=${PWD[i]}
      case $ch in
        [A-Za-z0-9/._~-]) path_encoded+=$ch ;;
        *) path_encoded+=$(builtin printf '%%%02X' "'$ch") ;;
      esac
    done
  fi
  builtin printf '\e]7;file://%s%s\a' "$HOST" "$path_encoded"
}

_hutan_precmd() {
  builtin local exit_status=$?
  if (( _hutan_command_running )); then
    builtin printf '\e]133;D;%d\a' "$exit_status"
    _hutan_command_running=0
  fi
  _hutan_osc7
  # Prompt frameworks rewrite PS1 in their own precmd hooks, which run before this one, so
  # the marks are (re)added here; a PS1 that already carries OSC 133 is left alone.
  if [[ $PS1 != *$'\e]133;'* ]]; then
    PS1=$'%{\e]133;A\a%}'$PS1$'%{\e]133;B\a%}'
  fi
  return $exit_status
}

_hutan_preexec() {
  _hutan_command_running=1
  builtin printf '\e]133;C\a'
}

autoload -Uz add-zsh-hook
add-zsh-hook precmd _hutan_precmd
add-zsh-hook preexec _hutan_preexec
# This is the first prompt: precmd hooks were snapshotted before we registered, so run ours
# now. (A prompt framework that rewrites PS1 later in this very cycle hides the marks once.)
_hutan_precmd

# Agents' own OpenTelemetry (TEL-2, ADR 0026). While the daemon's loopback receiver listens, a
# Hutan terminal has HUTAN_OTLP_ENDPOINT and HUTAN_OTLP_TOKEN; `claude` and `codex` then become
# functions that ask `hutan telemetry env <agent>` what to put in front of that one launch (nothing
# unless the agent is enabled and nothing of yours, a managed policy's or the agent's own is in the
# way: it never overrides an OTEL_* variable or a setting) and run the program found on PATH with
# it. A `claude` or `codex` function or alias of your own is left alone.
if [[ -n ${HUTAN_OTLP_ENDPOINT-} && -n ${HUTAN_BIN-} ]]; then
  _hutan_otlp_launch() {
    builtin local agent=$1 line
    shift
    builtin local -a launch_env launch_args
    if [[ -x $HUTAN_BIN ]] && (( ${+commands[$agent]} )); then
      for line in ${(f)"$("$HUTAN_BIN" telemetry env "$agent" 2>/dev/null)"}; do
        case $line in
          'env '*) launch_env+=("${line#env }") ;;
          'arg '*) launch_args+=("${line#arg }") ;;
        esac
      done
    fi
    if (( ${#launch_env} )); then
      builtin command env "${launch_env[@]}" "$agent" "${launch_args[@]}" "$@"
    else
      builtin command "$agent" "$@"
    fi
  }
  for _hutan_agent in claude codex; do
    if (( ! ${+functions[$_hutan_agent]} && ! ${+aliases[$_hutan_agent]} )); then
      eval "$_hutan_agent() { _hutan_otlp_launch $_hutan_agent \"\$@\"; }"
    fi
  done
  builtin unset _hutan_agent
fi
