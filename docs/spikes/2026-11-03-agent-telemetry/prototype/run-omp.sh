#!/bin/sh
# run-omp.sh "prompt" — scratch HOME, mock OpenAI-compatible provider, OTLP traces/logs to the local receiver
S=$HOME/hutan-scratch/agent-telemetry
cd $S/work-omp
P="$1"; shift
exec env -i PATH=/usr/bin:/bin:/opt/homebrew/bin HOME=$S/home-omp PI_CODING_AGENT_DIR=$S/home-omp/agent TMPDIR=/tmp HTTPS_PROXY=http://127.0.0.1:9 HTTP_PROXY=http://127.0.0.1:9 NO_PROXY=127.0.0.1,localhost \
 OTEL_EXPORTER_OTLP_ENDPOINT=${OTLP_BASE:-http://127.0.0.1:4318/s/TOK123} OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf \
 OTEL_TRACES_EXPORTER=otlp OTEL_LOGS_EXPORTER=otlp OTEL_METRICS_EXPORTER=otlp \
 omp -p "$P" --model mock/mock-1 --no-session --no-title "$@"
