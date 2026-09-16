#!/usr/bin/env bash
# Record three comparison routes without changing their result contracts.
set -euo pipefail

if [[ $# -ne 5 ]]; then
    echo 'Usage: capture-baseline.sh <pdfdelta> <pdfbench> <cache> <new-output-directory> <implementation-commit>' >&2
    exit 2
fi
pdfdelta=$(realpath "$1")
pdfbench=$(realpath "$2")
cache=$(realpath "$3")
output=$4
commit=$(git rev-parse --verify "$5^{commit}")
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
manifest="$script_dir/../manifest.tsv"
[[ -x "$pdfdelta" && -x "$pdfbench" ]]
mkdir -- "$output"
output=$(realpath "$output")
mkdir -- "$output/raw"
sha256sum "$pdfdelta" "$pdfbench" > "$output/binaries.sha256"
git show "$commit:Cargo.lock" | sha256sum > "$output/cargo-lock.sha256"
{
    rustc --version
    uname -srm
    /usr/bin/time --version | head -1
} > "$output/environment.txt"
jq -n --arg commit "$commit" \
    '{schema_version:1, implementation_commit:$commit, limit_scale:1,
      timeout_seconds:180, summary_input_limit_bytes:134217728,
      timing_unit:"seconds", peak_rss_unit:"KiB",
      routes:["native","text","all"], annotation_scoring:false}' > "$output/config.json"
awk -F '\t' '$1 == "pair_id" {print}' "$manifest" > "$output/inputs.tsv"

while IFS= read -r pair; do
    [[ "$pair" == pair_id ]] && continue
    row=$(awk -F '\t' -v pair="$pair" '$1 == pair {print}' "$manifest")
    [[ $(printf '%s\n' "$row" | wc -l) -eq 1 && -n "$row" ]]
    printf '%s\n' "$row" >> "$output/inputs.tsv"
    old_hash=$(printf '%s\n' "$row" | cut -f14)
    new_hash=$(printf '%s\n' "$row" | cut -f17)
    old="$cache/$pair-old.pdf"
    new="$cache/$pair-new.pdf"
    if ! printf '%s  %s\n%s  %s\n' "$old_hash" "$old" "$new_hash" "$new" | sha256sum --check --status; then
        jq -cn --arg pair "$pair" '{pair:$pair, stage:"acquisition", status:"missing_or_hash_mismatch"}' >> "$output/runs.jsonl"
        continue
    fi
    for route in native text all; do
        case "$route" in
            native) args=(--native-text-only) ;;
            text) args=(--channels text) ;;
            all) args=(--channels text,visual,forms,relations) ;;
        esac
        stem="$output/raw/$pair-$route"
        status=0
        /usr/bin/time -f '%e\t%M' -o "$stem.time" \
            timeout --signal=TERM --kill-after=5s 180s \
            "$pdfdelta" "$old" "$new" "${args[@]}" --limit-scale 1 --quiet \
            --json "$stem.json" > "$stem.stdout" 2> "$stem.stderr" || status=$?
        IFS=$'\t' read -r seconds rss < <(tail -1 "$stem.time")
        summary="$output/$pair-$route.json"
        report_hash=""
        bytes=0
        evaluation_status=report_unavailable
        if [[ -f "$stem.json" ]]; then
            report_hash=$(sha256sum "$stem.json" | cut -d ' ' -f1)
            bytes=$(wc -c < "$stem.json")
            if [[ "$status" -ne 0 && "$status" -ne 1 && "$status" -ne 3 ]]; then
                evaluation_status=process_failed_or_timed_out
            elif [[ "$bytes" -gt 134217728 ]]; then
                evaluation_status=summary_input_limit
            elif [[ "$route" == native ]]; then
                if jq '{schema_version,summary,extraction:{old_complete:.extraction.old_complete,
                    new_complete:.extraction.new_complete,issues:(.extraction.issues|length)}}' "$stem.json" > "$summary"; then
                    evaluation_status=operational_summary_only
                else
                    evaluation_status=summary_failed
                fi
            elif "$pdfbench" summarize-document --report "$stem.json" > "$stem.summary.json" 2> "$stem.summary.stderr"; then
                jq 'def issues: group_by([.channel,.kind,.reason]) | map({channel:.[0].channel,
                    kind:.[0].kind,reason:.[0].reason,count:length});
                    .old_issues |= issues | .new_issues |= issues' "$stem.summary.json" > "$summary"
                evaluation_status=operational_summary_only
            else
                evaluation_status=summary_failed
            fi
        fi
        jq -cn --arg pair "$pair" --arg route "$route" --arg old "$old_hash" --arg new "$new_hash" \
            --argjson exit_code "$status" --arg seconds "$seconds" --arg rss "$rss" \
            --argjson report_bytes "$bytes" --arg report_sha256 "$report_hash" --arg evaluation "$evaluation_status" \
            '{pair:$pair,route:$route,old_sha256:$old,new_sha256:$new,exit_code:$exit_code,
              elapsed_seconds:($seconds|tonumber),peak_rss_kib:($rss|tonumber),report_bytes:$report_bytes,
              report_sha256:$report_sha256,evaluation:$evaluation}' >> "$output/runs.jsonl"
    done
done < "$script_dir/baseline-pairs.tsv"
jq -s . "$output/runs.jsonl" > "$output/runs.json"
