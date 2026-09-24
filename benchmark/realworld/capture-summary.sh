#!/usr/bin/env bash
# Capture a release-mode real-world revision summary with reproducible metadata.
set -euo pipefail

temporary_directory=""
temporary_summary=""
temporary_raw=""
temporary_evaluation=""
temporary_metadata=""
temporary_source_manifest=""

cleanup() {
    [[ -z "${temporary_summary}" ]] || rm -f -- "${temporary_summary}"
    [[ -z "${temporary_raw}" ]] || rm -f -- "${temporary_raw}"
    [[ -z "${temporary_evaluation}" ]] || rm -f -- "${temporary_evaluation}"
    [[ -z "${temporary_metadata}" ]] || rm -f -- "${temporary_metadata}"
    [[ -z "${temporary_source_manifest}" ]] || rm -f -- "${temporary_source_manifest}"
    if [[ -n "${temporary_directory}" ]]; then
        rmdir -- "${temporary_directory}" 2>/dev/null || true
    fi
}
trap cleanup EXIT

usage() {
    printf '%s\n' \
        'Usage: benchmark/realworld/capture-summary.sh [output.json]' \
        '' \
        'Writes the legacy summary plus .raw.json, .evaluation.json,' \
        '.metadata.json, and .source-manifest.tsv sidecars. Baselines are empty' \
        'unless added to the published metadata from a verified run. Without' \
        'an output path, it writes results/YYYY-MM-DD-<commit>.json and' \
        'requires a clean worktree.'
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
source_dirty=false
if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
    source_dirty=true
fi
source_revision="${initial_commit}"
if [[ "${source_dirty}" == true ]]; then
    source_revision="${source_revision}-dirty"
fi
immutable_capture=false

if [[ "$#" -eq 0 ]]; then
    if [[ -n "$(git status --porcelain --untracked-files=normal)" ]]; then
        printf 'FAIL: default immutable capture requires a clean worktree\n' >&2
        exit 2
    fi
    capture_date="$(date +%F)"
    engine_commit="$(git rev-parse --short=7 HEAD)"
    mkdir -p -- "${repo_root}/benchmark/realworld/results"
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
summary_display="${display_output}"
raw_display="${summary_display%.json}.raw.json"
evaluation_display="${summary_display%.json}.evaluation.json"
metadata_display="${summary_display%.json}.metadata.json"
source_manifest_display="${summary_display%.json}.source-manifest.tsv"
summary_output="${output}"
raw_output="${output%.json}.raw.json"
evaluation_output="${output%.json}.evaluation.json"
metadata_output="${output%.json}.metadata.json"
source_manifest_output="${output%.json}.source-manifest.tsv"
for destination in "${summary_output}" "${raw_output}" "${evaluation_output}" "${metadata_output}" "${source_manifest_output}"; do
    if [[ -e "${destination}" || -L "${destination}" ]]; then
        printf 'FAIL: refusing to overwrite capture artifact: %s\n' "${destination}" >&2
        exit 2
    fi
done
output_parent="$(dirname -- "${output}")"
if [[ ! -d "${output_parent}" ]]; then
    printf 'FAIL: summary output parent does not exist: %s\n' "${output_parent}" >&2
    exit 2
fi

temporary_directory="$(mktemp -d "${output_parent}/.pdfdelta-capture.XXXXXX")"
temporary_summary="${temporary_directory}/summary.json"
temporary_raw="${temporary_directory}/raw.json"
temporary_evaluation="${temporary_directory}/evaluation.json"
temporary_metadata="${temporary_directory}/metadata.json"
temporary_source_manifest="${temporary_directory}/source-manifest.tsv"

{
    printf '# pdfdelta source manifest v1\n'
    printf 'base_revision\t%s\n' "${initial_commit}"
    printf 'working_tree_dirty\t%s\n' "${source_dirty}"
    while IFS= read -r source_path; do
        if [[ -f "${source_path}" ]]; then
            source_hash="$(sha256sum -- "${source_path}")"
            source_hash="${source_hash%% *}"
            printf '%s\t%s\n' "${source_hash}" "${source_path}"
        else
            printf 'deleted\t%s\n' "${source_path}"
        fi
    done < <(
        git ls-files --cached --others --exclude-standard -- \
            'Cargo.lock' 'Cargo.toml' 'crates/**/*.rs' 'crates/**/Cargo.toml' \
            | LC_ALL=C sort
    )
} >"${temporary_source_manifest}"
source_manifest_sha256="$(sha256sum -- "${temporary_source_manifest}")"
source_manifest_sha256="${source_manifest_sha256%% *}"

cargo run -p pdfdelta-bench --locked -- revisions \
    --cache-dir benchmark/realworld/cache \
    --checksums-only
set +e
cargo run -p pdfdelta-bench --release --locked -- revisions \
    --cache-dir benchmark/realworld/cache \
    --json-output "${temporary_raw}" \
    --summary-json-output "${temporary_summary}" \
    --evaluation-json-output "${temporary_evaluation}"
benchmark_status=$?
set -e
if [[ "${benchmark_status}" -gt 1 ]]; then
    printf 'FAIL: benchmark execution failed with status %s\n' "${benchmark_status}" >&2
    exit "${benchmark_status}"
fi

manifest_sha256="$(sha256sum -- benchmark/realworld/manifest.tsv | cut -d' ' -f1)"
annotation_sha256="$(sha256sum -- benchmark/realworld/expected/*.json | LC_ALL=C sort | sha256sum | cut -d' ' -f1)"
raw_result_sha256="$(sha256sum -- "${temporary_raw}" | cut -d' ' -f1)"
summary_sha256="$(sha256sum -- "${temporary_summary}" | cut -d' ' -f1)"
evaluation_sha256="$(sha256sum -- "${temporary_evaluation}" | cut -d' ' -f1)"
policy_sha256="$(printf '%s' 'pdfdelta-assessment-policy-v1' | sha256sum | cut -d' ' -f1)"
corpus_sha256="$(printf '%s\n%s\n' "${manifest_sha256}" "${annotation_sha256}" | sha256sum | cut -d' ' -f1)"
compiler="$(rustc --version)"
cat >"${temporary_metadata}" <<EOF
{
  "schema_version": 3,
  "command": [
    "cargo", "run", "-p", "pdfdelta-bench", "--release", "--locked", "--",
    "revisions", "--cache-dir", "benchmark/realworld/cache", "--json-output", "${raw_display}",
    "--summary-json-output", "${summary_display}", "--evaluation-json-output", "${evaluation_display}"
  ],
  "manifest_sha256": "${manifest_sha256}",
  "annotation_sha256": "${annotation_sha256}",
  "policy_sha256": "${policy_sha256}",
  "corpus_sha256": "${corpus_sha256}",
  "source_revision": "${source_revision}",
  "source_dirty": ${source_dirty},
  "source_manifest_path": "${source_manifest_display}",
  "source_manifest_sha256": "${source_manifest_sha256}",
  "options": {
    "set": "all",
    "limit_scale": "manifest",
    "profile": "release",
    "benchmark_status": \${benchmark_status}
  },
  "raw_result_path": "${raw_display}",
  "raw_result_sha256": "${raw_result_sha256}",
  "summary_sha256": "${summary_sha256}",
  "evaluation_result_path": "${evaluation_display}",
  "evaluation_result_sha256": "${evaluation_sha256}",
  "baselines": [],
  "compiler": "${compiler}",
  "os": "$(uname -srm)"
}
EOF

if [[ "${immutable_capture}" == true ]]; then
    if [[ "$(git rev-parse HEAD)" != "${initial_commit}" ]]; then
        printf 'FAIL: HEAD changed during immutable capture\n' >&2
        exit 2
    fi
    if ! git diff --quiet || ! git diff --cached --quiet; then
        printf 'FAIL: tracked files changed during immutable capture\n' >&2
        exit 2
    fi
    allowed_temporary="${temporary_directory#"${repo_root}/"}"
    allowed_summary="${summary_output#"${repo_root}/"}"
    allowed_raw="${raw_output#"${repo_root}/"}"
    allowed_evaluation="${evaluation_output#"${repo_root}/"}"
    allowed_metadata="${metadata_output#"${repo_root}/"}"
    allowed_source_manifest="${source_manifest_output#"${repo_root}/"}"
    while IFS= read -r untracked; do
        if [[ "${untracked}" != "${allowed_temporary}"/* \
            && "${untracked}" != "${allowed_summary}" \
            && "${untracked}" != "${allowed_raw}" \
            && "${untracked}" != "${allowed_evaluation}" \
            && "${untracked}" != "${allowed_metadata}" \
            && "${untracked}" != "${allowed_source_manifest}" ]]; then
            printf 'FAIL: untracked file appeared during immutable capture: %s\n' "${untracked}" >&2
            exit 2
        fi
    done < <(git ls-files --others --exclude-standard)
fi

if ! ln -- "${temporary_summary}" "${summary_output}"; then
    printf 'FAIL: could not publish summary without overwriting: %s\n' "${display_output}" >&2
    exit 2
fi
if ! ln -- "${temporary_raw}" "${raw_output}" \
    || ! ln -- "${temporary_evaluation}" "${evaluation_output}" \
    || ! ln -- "${temporary_metadata}" "${metadata_output}" \
    || ! ln -- "${temporary_source_manifest}" "${source_manifest_output}"; then
    printf 'FAIL: could not publish capture sidecars without overwriting\n' >&2
    exit 2
fi

size_bytes="$(wc -c <"${summary_output}")"
size_bytes="${size_bytes//[[:space:]]/}"
sha256="$(sha256sum -- "${summary_output}")"
sha256="${sha256%% *}"

printf '%s\n' \
    "capture=${display_output}" \
    "size_bytes=${size_bytes}" \
    "sha256=${sha256}" \
    "engine_commit=${source_revision}" \
    "os=$(uname -srm)" \
    "compiler=${compiler}" \
    'profile=release' \
    "raw=${raw_display}" \
    "evaluation=${evaluation_display}" \
    "metadata=${metadata_display}" \
    "source_manifest=${source_manifest_display}"

exit "${benchmark_status}"
