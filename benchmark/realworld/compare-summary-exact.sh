#!/usr/bin/env bash
# Require byte-for-byte equality between two immutable benchmark summaries.
set -euo pipefail

usage() {
    printf '%s\n' \
        'Usage: benchmark/realworld/compare-summary-exact.sh <baseline.json> <candidate.json>'
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ "$#" -ne 2 ]]; then
    usage >&2
    exit 2
fi

for input in "$1" "$2"; do
    if [[ ! -f "${input}" || -L "${input}" ]]; then
        printf 'FAIL: summary must be a regular, non-symlink file: %s\n' "${input}" >&2
        exit 2
    fi
done
if [[ "$1" -ef "$2" ]]; then
    printf 'FAIL: baseline and candidate must be distinct files\n' >&2
    exit 2
fi

if ! cmp -- "$1" "$2"; then
    printf 'FAIL: benchmark summaries differ\n' >&2
    exit 1
fi
printf 'PASS: benchmark summaries are byte-for-byte identical\n'
