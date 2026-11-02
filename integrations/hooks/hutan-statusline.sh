#!/bin/sh
# hutan-managed: Claude Code status-line tee.
# Installed by `hutan integration install claude --statusline` (opt-in); reinstalling overwrites
# this file. `hutan integration uninstall claude` puts the status line you had back.
#
# Claude Code runs one status-line command per user and writes the session's JSON (context
# window, cost, durations) to its standard input. This script hands that JSON to Hutan
# (`hutan agent statusline`, in the background, bounded to 200 ms) and then runs the command you
# had before, with the same input, passing its output and exit status through. That command is
# kept in hutan-statusline.command beside this file; when there was none, nothing is printed.
# Outside a Hutan session, or without the CLI, only your command runs. Nothing is sent but the
# context size, the cost and the API time.

dir=$(dirname "$0")
input=$(cat)

if [ "${HUTAN_ENV:-}" = "1" ] && [ -n "${HUTAN_SESSION_ID:-}" ]; then
  bin="${HUTAN_BIN:-hutan}"
  if command -v "$bin" >/dev/null 2>&1; then
    printf '%s' "$input" | "$bin" agent statusline >/dev/null 2>&1 &
  fi
fi

original="$dir/hutan-statusline.command"
if [ -s "$original" ]; then
  command=$(cat "$original")
  printf '%s' "$input" | sh -c "$command"
  exit $?
fi
exit 0
