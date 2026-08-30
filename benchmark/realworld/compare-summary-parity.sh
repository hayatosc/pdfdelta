#!/usr/bin/env bash
# Compare immutable summaries while excluding explicitly named schema additions.
set -euo pipefail

baseline_tmp=""
candidate_tmp=""

usage() {
    printf '%s\n' \
        'Usage: benchmark/realworld/compare-summary-parity.sh <baseline.json> <candidate.json> [--exclude-pair <pair-id>]... [--ignore-field <field-path>]... [<candidate-only-field-path>...]' \
        '' \
        'Removes schema_version from both summaries, --ignore-field recovery paths' \
        'from both sides, and candidate-only recovery paths from the candidate.' \
        'Explicitly excluded pairs must occur exactly once in both summaries.'
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
if [[ "$#" -lt 2 ]]; then
    usage >&2
    exit 2
fi

baseline="$1"
candidate="$2"
shift 2
exclude_pairs=()
ignored_fields=()
fields=()
while [[ "$#" -gt 0 ]]; do
    case "$1" in
        --exclude-pair)
            if [[ "$#" -lt 2 ]]; then
                printf 'FAIL: --exclude-pair requires a pair id\n' >&2
                exit 2
            fi
            for existing in "${exclude_pairs[@]}"; do
                if [[ "${existing}" == "$2" ]]; then
                    printf 'FAIL: duplicate excluded pair id: %s\n' "$2" >&2
                    exit 2
                fi
            done
            exclude_pairs+=("$2")
            shift 2
            ;;
        --ignore-field)
            if [[ "$#" -lt 2 ]]; then
                printf 'FAIL: --ignore-field requires a recovery metric field path\n' >&2
                exit 2
            fi
            for existing in "${ignored_fields[@]}"; do
                if [[ "${existing}" == "$2" ]]; then
                    printf 'FAIL: duplicate ignored recovery metric field path: %s\n' "$2" >&2
                    exit 2
                fi
            done
            ignored_fields+=("$2")
            shift 2
            ;;
        --*)
            printf 'FAIL: unknown option: %s\n' "$1" >&2
            exit 2
            ;;
        *)
            for existing in "${fields[@]}"; do
                if [[ "${existing}" == "$1" ]]; then
                    printf 'FAIL: duplicate recovery metric field path: %s\n' "$1" >&2
                    exit 2
                fi
            done
            fields+=("$1")
            shift
            ;;
    esac
done
if [[ "${#fields[@]}" -eq 0 && "${#ignored_fields[@]}" -eq 0 ]]; then
    usage >&2
    exit 2
fi

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

for pair in "${exclude_pairs[@]}"; do
    if [[ ! "${pair}" =~ ^[a-z0-9][a-z0-9-]*$ ]]; then
        printf 'FAIL: invalid excluded pair id: %s\n' "${pair}" >&2
        exit 2
    fi
    for input in "${baseline}" "${candidate}"; do
        if ! jq -e --arg pair "${pair}" '
            [.records[] | select(.pair_id == $pair)] | length == 1
        ' "${input}" >/dev/null; then
            printf 'FAIL: excluded pair must occur exactly once in %s: %s\n' "${input}" "${pair}" >&2
            exit 2
        fi
    done
done

for field_path in "${ignored_fields[@]}"; do
    if [[ ! "${field_path}" =~ ^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$ ]]; then
        printf 'FAIL: invalid ignored recovery metric field path: %s\n' "${field_path}" >&2
        exit 2
    fi
    for input in "${baseline}" "${candidate}"; do
        if ! jq -e --arg field_path "${field_path}" '
            def has_path($path):
                reduce $path[] as $key (
                    {found: true, value: .};
                    if .found
                        and (.value | type) == "object"
                        and (.value | has($key))
                    then
                        {found: true, value: .value[$key]}
                    else
                        {found: false, value: null}
                    end
                )
                | .found;

            ($field_path | split(".")) as $path
            |
            any(
                .records[];
                ((.sentence_recovery_metrics? // {}) | has_path($path))
            )
        ' "${input}" >/dev/null; then
            printf 'FAIL: ignored recovery metric field path is absent from %s: %s\n' \
                "${input}" "${field_path}" >&2
            exit 2
        fi
    done
done

for field_path in "${fields[@]}"; do
    if [[ ! "${field_path}" =~ ^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$ ]]; then
        printf 'FAIL: invalid recovery metric field path: %s\n' "${field_path}" >&2
        exit 2
    fi
    if ! jq -e --arg field_path "${field_path}" '
        def has_path($path):
            reduce $path[] as $key (
                {found: true, value: .};
                if .found
                    and (.value | type) == "object"
                    and (.value | has($key))
                then
                    {found: true, value: .value[$key]}
                else
                    {found: false, value: null}
                end
            )
            | .found;

        ($field_path | split(".")) as $path
        |
        all(
            .records[];
            ((.sentence_recovery_metrics? // {}) | has_path($path) | not)
        )
    ' "${baseline}" >/dev/null; then
        printf 'FAIL: baseline already contains recovery metric field path: %s\n' "${field_path}" >&2
        exit 2
    fi
    if ! jq -e --arg field_path "${field_path}" '
        def has_path($path):
            reduce $path[] as $key (
                {found: true, value: .};
                if .found
                    and (.value | type) == "object"
                    and (.value | has($key))
                then
                    {found: true, value: .value[$key]}
                else
                    {found: false, value: null}
                end
            )
            | .found;

        ($field_path | split(".")) as $path
        |
        any(
            .records[];
            ((.sentence_recovery_metrics? // {}) | has_path($path))
        )
    ' "${candidate}" >/dev/null; then
        printf 'FAIL: candidate does not contain recovery metric field path: %s\n' "${field_path}" >&2
        exit 2
    fi
done

fields_json="$(jq -cn '$ARGS.positional | map(split("."))' --args -- "${fields[@]}")"
ignored_fields_json="$(jq -cn '$ARGS.positional | map(split("."))' --args -- "${ignored_fields[@]}")"
exclude_pairs_json="$(jq -cn '$ARGS.positional' --args -- "${exclude_pairs[@]}")"
baseline_tmp="$(mktemp)"
candidate_tmp="$(mktemp)"

jq -S -c --argjson field_paths "${ignored_fields_json}" --argjson excluded "${exclude_pairs_json}" '
    del(.schema_version)
    | .records |= map(
        . as $record
        | select(($excluded | index($record.pair_id)) == null)
        | if (.sentence_recovery_metrics? | type) == "object" then
              .sentence_recovery_metrics |= delpaths($field_paths)
          else
              .
          end
    )
' "${baseline}" >"${baseline_tmp}"
jq -S -c \
    --argjson field_paths "${fields_json}" \
    --argjson ignored_field_paths "${ignored_fields_json}" \
    --argjson excluded "${exclude_pairs_json}" '
    del(.schema_version)
    | .records |= map(
        . as $record
        | select(($excluded | index($record.pair_id)) == null)
        | if (.sentence_recovery_metrics? | type) == "object" then
              .sentence_recovery_metrics |= delpaths($field_paths + $ignored_field_paths)
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
