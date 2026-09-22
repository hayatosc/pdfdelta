#!/usr/bin/env bash
# Shared 6GB containment + single-job lock; the Python driver owns cleanup/run/cleanup.
set -uo pipefail
LIMIT=6000000000
SLICE=pdfdelta-heavy.slice
SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/../../../.." && pwd)
CACHE="$ROOT/benchmark/realworld/cache/native-12-of-36-2026-09-20"
TASK_TMP=/tmp/opencode/pdfdelta-task
HK="$SCRIPT_DIR/housekeeping.py"
PROTECT=(--protect "$CACHE/h24b-page-index-nist-native"
  --protect "$CACHE/h24-page-exclusion-targeted-native"
  --protect "$CACHE/h24b-page-index-regression-native"
  --protect "$CACHE/h23q-nist-trace")

umask 077
if [ -e "$TASK_TMP" ] && { [ ! -d "$TASK_TMP" ] || [ -L "$TASK_TMP" ] || [ "$(stat -c %u "$TASK_TMP")" != "$(id -u)" ]; }; then
  echo "task tmp is not an owned directory" >&2; exit 75
fi
mkdir -p "$TASK_TMP" || exit 75
touch "$TASK_TMP/.task-owned" || exit 75
LOCK="$TASK_TMP/heavy.lock"
if ! exec 9>"$LOCK"; then echo "cannot open lock" >&2; exit 75; fi
if ! flock -n 9; then echo "another heavy job is running" >&2; exit 75; fi
systemctl --user set-property --runtime "$SLICE" MemoryMax=$LIMIT MemorySwapMax=0 2>/dev/null || true
CG=$(systemctl --user show "$SLICE" -p ControlGroup --value 2>/dev/null || true)
if [ -z "$CG" ]; then
  systemctl --user start "$SLICE" >/dev/null 2>&1 || true
  systemctl --user set-property --runtime "$SLICE" MemoryMax=$LIMIT MemorySwapMax=0 >/dev/null 2>&1 || true
  CG=$(systemctl --user show "$SLICE" -p ControlGroup --value 2>/dev/null || true)
fi
[ -n "$CG" ] || { echo "slice control group missing" >&2; exit 75; }
MAX=$(cat "/sys/fs/cgroup$CG/memory.max" 2>/dev/null || echo max)
SWAP=$(cat "/sys/fs/cgroup$CG/memory.swap.max" 2>/dev/null || echo max)
case "$MAX" in ''|max) echo "memory.max missing/unbounded" >&2; exit 75;; esac
[ "$MAX" -gt 0 ] && [ "$MAX" -le "$LIMIT" ] || { echo "memory.max $MAX out of range" >&2; exit 75; }
[ "$SWAP" = "0" ] || { echo "memory.swap.max $SWAP not 0" >&2; exit 75; }
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-1}
export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
export CARGO_PROFILE_DEV_DEBUG=${CARGO_PROFILE_DEV_DEBUG:-0}
export CARGO_PROFILE_TEST_DEBUG=${CARGO_PROFILE_TEST_DEBUG:-0}
systemd-run --user --scope --slice="$SLICE" \
  -p MemoryMax=$LIMIT -p MemorySwapMax=0 \
  -E CARGO_BUILD_JOBS="$CARGO_BUILD_JOBS" -E CARGO_INCREMENTAL="$CARGO_INCREMENTAL" \
  -E CARGO_PROFILE_DEV_DEBUG="$CARGO_PROFILE_DEV_DEBUG" \
  -E CARGO_PROFILE_TEST_DEBUG="$CARGO_PROFILE_TEST_DEBUG" \
  -- python3 "$HK" "$CACHE" --target "$ROOT/target" --tmp "$TASK_TMP" "${PROTECT[@]}" --run "$@"
exit $?
