#!/bin/sh
# run-claude.sh "prompt" [extra claude args...]  — scratch HOME, mock API, OTLP to the local receiver
S=$HOME/hutan-scratch/agent-telemetry
cd $S/work-claude
P="$1"; shift
MOCK_CWD=$PWD exec env -i PATH=/usr/bin:/bin:$HOME/.local/bin HOME=$S/home-claude CLAUDE_CONFIG_DIR=$S/home-claude/.claude TMPDIR=/tmp HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost \
 ANTHROPIC_BASE_URL=http://127.0.0.1:4400 ANTHROPIC_API_KEY=$(printf 'sk-ant-%s' 'api03-mockmockmockmockmockmock') \
 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 DISABLE_AUTOUPDATER=1 CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
 CLAUDE_CODE_ENABLE_TELEMETRY=1 CLAUDE_CODE_ENHANCED_TELEMETRY_BETA=1 OTEL_METRICS_EXPORTER=otlp OTEL_LOGS_EXPORTER=otlp OTEL_TRACES_EXPORTER=otlp \
 OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf OTEL_EXPORTER_OTLP_ENDPOINT=${OTLP_BASE:-http://127.0.0.1:4318/s/TOK123} \
 OTEL_METRIC_EXPORT_INTERVAL=2000 OTEL_LOGS_EXPORT_INTERVAL=1000 OTEL_TRACES_EXPORT_INTERVAL=1000 \
 claude -p "$P" --output-format json "$@"
