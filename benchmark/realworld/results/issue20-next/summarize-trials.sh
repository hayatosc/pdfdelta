#!/bin/sh
# Usage: sh summarize-trials.sh FROZEN_DIRECTORY PDFBENCH > routes.json
# Every inventory entry retains all routes, including failed or unfinished trials.
set -eu
root=$1
bench=$2
scratch=$(mktemp -d "${TMPDIR:-/tmp}/pdfdelta-route-summary.XXXXXX")
jq -r '.[].pair_id' "$root/inventory.json" > "$scratch/pairs.txt"
while IFS= read -r pair; do
    for route in native shared_text shared_all; do
        trial="$root/trials/$pair/$route"
        jq --arg pair "$pair" '.[] | select(.pair_id == $pair) | {provenance_verified, failure}' "$root/inventory.json" > "$scratch/inventory.json"
        printf 'null\n' > "$scratch/process.json"
        status=not_run
        if [ -f "$trial/exit-code.txt" ]; then
            status=finished
            tail -n 1 "$trial/process.json" | jq --argjson code "$(cat "$trial/exit-code.txt")" \
                '. + {timing_exit_code: .exit_code, exit_code: $code}' > "$scratch/process.json"
        fi
        printf 'null\n' > "$scratch/observations.json"
        printf 'null\n' > "$scratch/projection-error.json"
        if [ -f "$trial/report.json" ]; then
            if [ "$route" = native ]; then
                jq '.records[0] | {
                    trial_status, compared, extraction_complete, comparison_complete,
                    coverage_old, coverage_new, accepted_changes, candidate_changes,
                    reviewed_recall, scoped_event, scoped_tokens, expected_matches,
                    quality_skipped_reason
                }' "$trial/report.json" > "$scratch/observations.json"
            elif "$bench" summarize-document --report "$trial/report.json" \
                > "$trial/operational.json" 2> "$trial/projection.log"; then
                jq 'def issue_groups:
                    group_by([.channel, .kind, .reason]) | map({
                        channel: .[0].channel, kind: .[0].kind, reason: .[0].reason,
                        count: length, pages: (map(.page) | unique)
                    });
                    .old_issue_groups = (.old_issues | issue_groups) |
                    .new_issue_groups = (.new_issues | issue_groups) |
                    del(.old_issues, .new_issues)' "$trial/operational.json" > "$scratch/observations.json"
            else
                jq -Rs '.' "$trial/projection.log" > "$scratch/projection-error.json"
            fi
        fi
        printf 'null\n' > "$scratch/diagnostics.json"
        if [ "$route" = native ] && [ -f "$trial/summary.json" ]; then
            jq '.records[0].expected_change_diagnostics | if . == null then null else {
                complete, failures,
                final_assessment: (.final_assessment | if . == null then null else {
                    complete, records: [.records[]? | {
                        expected_id, old, new, scan_complete,
                        candidate_count: (.candidates | length),
                        accepted_count: (.accepted_changes | length),
                        relation_reasons: [.relations[]?.reasons[]?] | unique
                    }]
                } end)
            } end' "$trial/summary.json" > "$scratch/diagnostics.json"
        fi
        printf 'null\n' > "$scratch/errors.json"
        if [ -f "$trial/stderr.log" ]; then
            jq -Rs '.' "$trial/stderr.log" > "$scratch/errors.json"
        fi
        jq -n --arg pair "$pair" --arg route "$route" --arg status "$status" \
            --slurpfile inventory "$scratch/inventory.json" --slurpfile process "$scratch/process.json" \
            --slurpfile observations "$scratch/observations.json" --slurpfile diagnostics "$scratch/diagnostics.json" \
            --slurpfile projection_error "$scratch/projection-error.json" --slurpfile errors "$scratch/errors.json" \
            '{pair: $pair, route: $route, process_status: $status, inventory: $inventory[0],
              process: $process[0], observations: $observations[0], diagnostics: $diagnostics[0],
              projection_error: $projection_error[0], stderr: $errors[0]}' >> "$scratch/records.jsonl"
    done
done < "$scratch/pairs.txt"
jq -s '{schema_version: 1, routes: .}' "$scratch/records.jsonl"
