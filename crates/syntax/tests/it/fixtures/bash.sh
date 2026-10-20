#!/usr/bin/env bash
# Build script
set -euo pipefail

NAME="hutan"
count=3

build() {
  local target="$1"
  if [[ -z "$target" ]]; then
    echo "usage: $0 <target>" >&2
    return 1
  fi
  for i in 1 2 3; do
    cargo build --release -p "$NAME-$i" || exit "$?"
  done
}

build "${NAME}" | tee out.log
