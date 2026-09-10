#!/bin/sh
# Usage: sh run-unseen-trial.sh INPUT_DIRECTORY BINARY PAIR COMPARISON ROUTE
set -u
root=$1
binary=$2
pair=$3
comparison=$4
route=$5
old="$root/$pair-old.pdf"
new="$root/$pair-new.pdf"
case "$comparison" in
    old-control) new=$old ;;
    new-control) old=$new ;;
    revision) ;;
    *) exit 2 ;;
esac
case "$route" in
    native) set -- --native-text-only ;;
    shared_text) set -- --channels text ;;
    shared_all) set -- --channels text,visual,forms,relations ;;
    *) exit 2 ;;
esac
trial="$root/trials/$pair/$comparison/$route"
mkdir -p "$trial"
ulimit -v 4194304
/usr/bin/time -f '{"elapsed_seconds":%e,"peak_rss_kib":%M,"exit_code":%x}' -o "$trial/process.json" \
    timeout --kill-after=5s 1200 "$binary" "$old" "$new" "$@" --limit-scale 1.0 \
    --quiet --json "$trial/report.json" > "$trial/stdout.log" 2> "$trial/stderr.log"
code=$?
printf '%s\n' "$code" > "$trial/exit-code.txt"
printf '%s %s %s exit=%s\n' "$pair" "$comparison" "$route" "$code"
