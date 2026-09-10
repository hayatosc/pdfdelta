#!/bin/sh
# Usage: sh score-source-trial.sh FROZEN_DIRECTORY CACHE ANNOTATION ROUTE SOURCE_VIEW
# Source-only validation precedes scoring; missing reports remain unavailable.
set -u
root=$1
cache=$2
annotation=$3
route=$4
view=$5
pair=$(jq -r '.pair' "$annotation")
trial="$root/source-evaluation-$view/$pair/$route"
report="$root/trials/$pair/$route/report.json"
mkdir -p "$trial"
if [ ! -s "$report" ]; then
    printf '%s\n' 'comparison report unavailable' > "$trial/stderr.log"
    printf '%s\n' 'unavailable' > "$trial/exit-code.txt"
    exit 0
fi
ulimit -v 4194304
/usr/bin/time -f '{"elapsed_seconds":%e,"peak_rss_kib":%M,"exit_code":%x}' -o "$trial/process.json" \
    timeout --kill-after=5s 180 "$root/pdfbench" validate-revision-selectors \
    --expected "$annotation" --old "$cache/$pair-old.pdf" --new "$cache/$pair-new.pdf" \
    --source-view "$view" --report "$report" > "$trial/report.json" 2> "$trial/stderr.log"
code=$?
printf '%s\n' "$code" > "$trial/exit-code.txt"
printf '%s %s %s exit=%s\n' "$pair" "$route" "$view" "$code"
