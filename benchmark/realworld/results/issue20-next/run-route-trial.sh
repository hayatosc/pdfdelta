#!/bin/sh
# Usage: sh run-route-trial.sh FROZEN_DIRECTORY CACHE PAIR ROUTE SCALE
# Preserve process failures and partial reports under the frozen trial limits.
set -u
root=$1
cache=$2
pair=$3
route=$4
scale=$5
trial="$root/trials/$pair/$route"
mkdir -p "$trial"
ulimit -v 4194304
if [ "$route" = native ]; then
    /usr/bin/time -f '{"elapsed_seconds":%e,"peak_rss_kib":%M,"exit_code":%x}' -o "$trial/process.json" \
        timeout --kill-after=5s 1200 "$root/pdfbench" revisions \
        --cache-dir "$cache" --pair "$pair" --limit-scale "$scale" \
        --summary-json-output "$trial/summary.json" --evaluation-json-output "$trial/report.json" \
        > "$trial/stdout.log" 2> "$trial/stderr.log"
else
    case "$route" in
        shared_text) channels=text ;;
        shared_all) channels=text,visual,forms,relations ;;
        *) exit 2 ;;
    esac
    /usr/bin/time -f '{"elapsed_seconds":%e,"peak_rss_kib":%M,"exit_code":%x}' -o "$trial/process.json" \
        timeout --kill-after=5s 1200 "$root/pdfdelta" \
        "$cache/$pair-old.pdf" "$cache/$pair-new.pdf" \
        --channels "$channels" --limit-scale "$scale" --quiet --json "$trial/report.json" \
        > "$trial/stdout.log" 2> "$trial/stderr.log"
fi
code=$?
printf '%s\n' "$code" > "$trial/exit-code.txt"
printf '%s %s exit=%s\n' "$pair" "$route" "$code"
