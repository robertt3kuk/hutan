#!/bin/sh
# hutan-managed: header helper of Claude Code's OpenTelemetry export to Hutan.
# Installed by `hutan integration install claude --telemetry`; `hutan integration uninstall
# claude` removes it. `otelHeadersHelper` in settings.json names this file: Claude Code runs it and
# sends what it prints as headers with every OpenTelemetry request.
#
# The daemon's receiver takes the session's token in the `x-hutan-token` header. The token lives
# in the environment of a Hutan terminal (`HUTAN_OTLP_TOKEN`, only while the receiver listens)
# and in no file: this script reads it from the process Claude Code runs in. Outside such a
# terminal it prints no header, and the receiver refuses the request (nothing is lost but the
# report).
token=${HUTAN_OTLP_TOKEN-}
case $token in
  '' | *[!0-9a-f]*) printf '{}\n' ;;
  *) printf '{"x-hutan-token":"%s"}\n' "$token" ;;
esac
