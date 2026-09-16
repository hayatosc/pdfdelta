#!/usr/bin/env bash
# Compare fixed-input routes; raw reports and process logs remain outside Git.
set -euo pipefail
if [[ $# -ne 6 ]]; then
    echo 'Usage: measure-indexing.sh <baseline-binary> <candidate-binary> <old.pdf> <new.pdf> <new-output-dir> <repeats>' >&2
    exit 2
fi
baseline=$(realpath "$1")
candidate=$(realpath "$2")
old=$(realpath "$3")
new=$(realpath "$4")
output=$5
repeats=$6
[[ "$repeats" =~ ^[1-9][0-9]*$ && "$repeats" -le 20 ]]
mkdir -- "$output"
output=$(realpath "$output")
sha256sum "$baseline" "$candidate" "$old" "$new" > "$output/inputs.sha256"
for ((repeat=0; repeat<repeats; repeat++)); do
    order=(baseline candidate)
    if ((repeat % 2)); then order=(candidate baseline); fi
    for route in text all; do
        channels=text
        [[ "$route" != all ]] || channels=text,visual,forms,relations
        for engine in "${order[@]}"; do
            binary=${!engine}
            stem="$output/$repeat-$route-$engine"
            status=0
            /usr/bin/time -f '%e\t%M' -o "$stem.time" \
                timeout --signal=TERM --kill-after=5s 180s \
                "$binary" "$old" "$new" --channels "$channels" --limit-scale 1 --quiet \
                --json "$stem.json" > "$stem.stdout" 2> "$stem.stderr" || status=$?
            IFS=$'\t' read -r seconds rss < <(tail -1 "$stem.time")
            if [[ "$status" != 0 && "$status" != 1 && "$status" != 3 ]]; then
                jq -cn --argjson repeat "$repeat" --arg route "$route" --arg engine "$engine" \
                    --argjson exit_code "$status" '{repeat:$repeat,route:$route,engine:$engine,exit_code:$exit_code,status:"process_failed"}' >> "$output/runs.jsonl"
                continue
            fi
            # Omit only execution counters and wall time; compare the full result contract.
            jq -S 'del(.comparison_wall_time_ms) |
                .comparison.scopes[].result.text_search |= del(.examined_pairs,.token_visits,.index_entries)' \
                "$stem.json" > "$stem.contract.json"
            contract_hash=$(sha256sum "$stem.contract.json" | cut -d ' ' -f1)
            jq -c --argjson repeat "$repeat" --arg route "$route" --arg engine "$engine" \
                --argjson exit_code "$status" --arg seconds "$seconds" --arg rss "$rss" --arg contract_hash "$contract_hash" \
                '{repeat:$repeat,route:$route,engine:$engine,exit_code:$exit_code,
                  elapsed_seconds:($seconds|tonumber),peak_rss_kib:($rss|tonumber),contract_sha256:$contract_hash,
                  scopes:[.comparison.scopes[].result | {source_complete:.candidates.exhaustive,
                    text_complete:.text_search.exhaustive,text_pairs:.text_search.examined_pairs,
                    token_visits:.text_search.token_visits,feature_entries:.text_search.feature_entries,
                    index_entries:(.text_search.index_entries // 0),proposals:(.candidates.proposals|length)}]}' \
                "$stem.json" >> "$output/runs.jsonl"
        done
    done
done
jq -s . "$output/runs.jsonl" > "$output/runs.json"
