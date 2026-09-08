#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 OUTPUT_JSON" >&2
  exit 2
fi

# Keep the output path explicit so reruns are reproducible and publication
# refuses to overwrite an existing artifact.
cargo run --locked -p pdfdelta-bench -- sensitivity --json-output "$1"
