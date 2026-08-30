#!/usr/bin/env bash
# Compare immutable summaries while excluding explicitly named schema additions.
set -euo pipefail

baseline_tmp=""
candidate_tmp=""

usage() {
    printf '%s\n' \
        'Usage: benchmark/realworld/compare-summary-parity.sh <baseline.json> <candidate.json> <field>...' \
        '' \
        'Removes schema_version from both summaries and the named fields from each' \
        'candidate record sentence_recovery_metrics object before comparison.'
}

cleanup() {
    [[ -z "${baseline_tmp}" ]] || rm -f -- "${baseline_tmp}"
    [[ -z "${candidate_tmp}" ]] || rm -f -- "${candidate_tmp}"
}
trap cleanup EXIT

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ "$#" -lt 3 ]]; then
    usage >&2
    exit 2
fi

baseline="$1"
candidate="$2"
shift 2
fields=("$@")

for input in "${baseline}" "${candidate}"; do
    if [[ ! -f "${input}" || -L "${input}" ]]; then
        printf 'FAIL: summary must be a regular, non-symlink file: %s\n' "${input}" >&2
        exit 2
    fi
    if ! jq -e '
        type == "object"
        and (.records | type == "array")
        and all(
            .records[];
            (.sentence_recovery_metrics? == null)
            or (.sentence_recovery_metrics | type == "object")
        )
    ' "${input}" >/dev/null; then
        printf 'FAIL: invalid benchmark summary shape: %s\n' "${input}" >&2
        exit 2
    fi
done

for field in "${fields[@]}"; do
    if [[ ! "${field}" =~ ^[a-z][a-z0-9_]*$ ]]; then
        printf 'FAIL: invalid recovery metric field: %s\n' "${field}" >&2
        exit 2
    fi
    if ! jq -e --arg field "${field}" '
        all(
            .records[];
            ((.sentence_recovery_metrics? // {}) | has($field) | not)
        )
    ' "${baseline}" >/dev/null; then
        printf 'FAIL: baseline already contains recovery metric field: %s\n' "${field}" >&2
        exit 2
    fi
    if ! jq -e --arg field "${field}" '
        any(
            .records[];
            ((.sentence_recovery_metrics? // {}) | has($field))
        )
    ' "${candidate}" >/dev/null; then
        printf 'FAIL: candidate does not contain recovery metric field: %s\n' "${field}" >&2
        exit 2
    fi
done

fields_json="$(jq -cn '$ARGS.positional' --args -- "${fields[@]}")"
baseline_tmp="$(mktemp)"
candidate_tmp="$(mktemp)"

jq -S -c 'del(.schema_version)' "${baseline}" >"${baseline_tmp}"
jq -S -c --argjson fields "${fields_json}" '
    del(.schema_version)
    | .records |= map(
        if (.sentence_recovery_metrics? | type) == "object" then
            .sentence_recovery_metrics |= delpaths($fields | map([.]))
        else
            .
        end
    )
' "${candidate}" >"${candidate_tmp}"

if ! cmp -s -- "${baseline_tmp}" "${candidate_tmp}"; then
    printf 'FAIL: summaries differ after excluding schema-only fields\n' >&2
    printf '  baseline: %s\n' "${baseline}" >&2
    printf '  candidate: %s\n' "${candidate}" >&2
    exit 1
fi

printf 'PASS: benchmark summaries have behavior parity\n'
