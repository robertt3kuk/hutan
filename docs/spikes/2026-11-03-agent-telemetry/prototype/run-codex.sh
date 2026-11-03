#!/bin/sh
# run-codex.sh "prompt" [extra codex args] — scratch CODEX_HOME, mock Responses API, OTLP over HTTP to local receiver
S=$HOME/hutan-scratch/agent-telemetry
cd $S/work-codex
P="$1"; shift
exec env -i PATH=/usr/bin:/bin:/opt/homebrew/bin HOME=$S/home-codex CODEX_HOME=$S/home-codex/.codex TMPDIR=/tmp HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost MOCK_API_KEY=mock-key \
 codex exec --skip-git-repo-check \
  -c 'otel.environment="hutan-spike"' \
  -c "otel.exporter={otlp-http={endpoint=\"${OTLP_BASE:-http://127.0.0.1:4318/s/TOK123}/v1/logs\",protocol=\"binary\"}}" \
  -c "otel.trace_exporter={otlp-http={endpoint=\"${OTLP_BASE:-http://127.0.0.1:4318/s/TOK123}/v1/traces\",protocol=\"binary\"}}" \
  -c "otel.metrics_exporter={otlp-http={endpoint=\"${OTLP_BASE:-http://127.0.0.1:4318/s/TOK123}/v1/metrics\",protocol=\"binary\"}}" \
  "$@" "$P"
