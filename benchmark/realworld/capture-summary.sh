#!/usr/bin/env bash
# Capture a release-mode real-world revision summary with reproducible metadata.
set -euo pipefail

temporary_directory=""
temporary_output=""

cleanup() {
    if [[ -n "${temporary_output}" ]]; then
        rm -f -- "${temporary_output}"
    fi
    if [[ -n "${temporary_directory}" ]]; then
        rmdir -- "${temporary_directory}" 2>/dev/null || true
    fi
}
trap cleanup EXIT

usage() {
    printf '%s\n' \
        'Usage: benchmark/realworld/capture-summary.sh [output.json]' \
        '' \
        'Without an output path, writes results/YYYY-MM-DD-<commit>.json and' \
        'requires a clean worktree. An explicit path is intended for reproduction.'
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ "$#" -gt 1 ]]; then
    usage >&2
    exit 2
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "${repo_root}"
initial_commit="$(git rev-parse HEAD)"
immutable_capture=false

if [[ "$#" -eq 0 ]]; then
    if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
        printf 'FAIL: default immutable capture requires a clean worktree\n' >&2
        exit 2
    fi
    capture_date="$(date +%F)"
    engine_commit="$(git rev-parse --short=7 HEAD)"
    output="benchmark/realworld/results/${capture_date}-${engine_commit}.json"
    immutable_capture=true
else
    output="$1"
fi
display_output="${output}"
if [[ "${output}" != /* ]]; then
    output="${repo_root}/${output}"
fi

if [[ "${output}" != *.json ]]; then
    printf 'FAIL: summary output must end in .json: %s\n' "${output}" >&2
    exit 2
fi
if [[ -e "${output}" || -L "${output}" ]]; then
    printf 'FAIL: refusing to overwrite summary output: %s\n' "${output}" >&2
    exit 2
fi
output_parent="$(dirname -- "${output}")"
if [[ ! -d "${output_parent}" ]]; then
    printf 'FAIL: summary output parent does not exist: %s\n' "${output_parent}" >&2
    exit 2
fi

temporary_directory="$(mktemp -d "${output_parent}/.pdfdelta-capture.XXXXXX")"
temporary_output="${temporary_directory}/summary.json"

cargo run -p pdfdelta-bench --locked -- revisions \
    --cache-dir benchmark/realworld/cache \
    --checksums-only
cargo run -p pdfdelta-bench --release --locked -- revisions \
    --cache-dir benchmark/realworld/cache \
    --summary-json-output "${temporary_output}"

if [[ "${immutable_capture}" == true ]]; then
    if [[ "$(git rev-parse HEAD)" != "${initial_commit}" ]]; then
        printf 'FAIL: HEAD changed during immutable capture\n' >&2
        exit 2
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
        printf 'FAIL: tracked files changed during immutable capture\n' >&2
        exit 2
    fi
    allowed_untracked="${temporary_output#"${repo_root}/"}"
    while IFS= read -r untracked; do
        if [[ "${untracked}" != "${allowed_untracked}" ]]; then
            printf 'FAIL: untracked file appeared during immutable capture: %s\n' "${untracked}" >&2
            exit 2
        fi
    done < <(git ls-files --others --exclude-standard)
fi

if ! ln -- "${temporary_output}" "${output}"; then
    printf 'FAIL: could not publish summary without overwriting: %s\n' "${display_output}" >&2
    exit 2
fi

size_bytes="$(wc -c <"${output}")"
size_bytes="${size_bytes//[[:space:]]/}"
sha256="$(sha256sum -- "${output}")"
sha256="${sha256%% *}"

printf '%s\n' \
    "capture=${display_output}" \
    "size_bytes=${size_bytes}" \
    "sha256=${sha256}" \
    "engine_commit=${initial_commit}" \
    "os=$(uname -srm)" \
    "compiler=$(rustc --version)" \
    'profile=release'
