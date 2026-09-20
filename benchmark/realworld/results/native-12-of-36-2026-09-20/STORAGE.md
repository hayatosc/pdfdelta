# Compressed capture storage and bounded retention

Requirement: large benchmark artifacts are compressed from creation, their
logical content hash stays the binding, and disposable completed runs rotate
under a bounded policy while baseline/active/accepted/final evidence is
protected. This record covers the storage work only; the comparison increments
(H2) and their evidence are recorded separately.

## Measured result

| Measure | Before | After |
| --- | ---: | ---: |
| Task run cache (`benchmark/realworld/cache/native-12-of-36-2026-09-20`) | 52 GiB allocated (55,444,710,920 apparent logical report bytes) | 2,288,861,613 apparent bytes (~2.4 GiB allocated) |
| Whole benchmark cache | 52 GiB allocated | 2.4 GiB allocated |
| Filesystem free (`/dev/sdd`) | 810 GiB | 860 GiB |

Migration: 57 capture reports compressed losslessly, `53,232,736,462` bytes
saved (49.58 GiB); stored archives total `2,211,974,458` bytes.

Verification of all 57 manifest entries: state `deleted`, archive exists,
stored size equals the recorded `stored_bytes`, stored file SHA-256 equals the
recorded `file_sha256`, and the original plaintext path no longer exists.
During compression each archive was decompressed again and compared with the
source digest and length before the source was removed, so no migration path
required extra space equal to the uncompressed source.

Evidence: `storage-compression-manifest.json` (copy of the run manifest,
SHA-256 `eb9c5178670b1328f0a8a057079aac8c9ac692ffd5addb9f017a6eef7f064f51`;
the authoritative file remains at
`benchmark/realworld/cache/native-12-of-36-2026-09-20/compression-manifest.json`.
Raw migration log: `benchmark/realworld/cache/native-12-of-36-2026-09-20/compress-apply.log`.

## Implementation

- `crates/pdfdelta-cli/src/fs.rs`: the atomic output writer compresses when the
  destination ends in `.gz` (`flate2`), so the temporary file is compressed and
  no plaintext report is materialized; flush, sync, overwrite refusal and
  hard-link publication are unchanged. `args.rs` documents the behavior.
- `benchmark/realworld/next/development/capture-comparisons.py`: writes
  `{pair}-{route}.json.gz`, records `report_sha256` (logical uncompressed),
  `report_logical_bytes`, `report_file_sha256`, `report_bytes` (stored) and
  `report_encoding`; stdout/stderr are stream-compressed through concurrent
  pump threads with a bounded 4096-byte stderr prefix. The wrapper runs in its
  own session with `timeout --foreground --kill-after=5 180` so a pump failure
  or interruption terminates the whole owned tree with `os.killpg` instead of
  leaving `timeout`/`pdfdelta` descendants holding pipes; the 180 s deadline
  and 5 s kill-after contract are unchanged.
- `benchmark/realworld/remaining/completion-investigation/capture.py`: gzip-aware
  bounded reader, `report_reference` with logical and stored digests,
  `resolve_report_path` for exact/`.gz`/manifest migration resolution, a
  versioned `.capture-run.json` lifecycle marker whose on-disk pins and reasons
  survive the active→completed/failed transition, a preflight free-space
  reserve of 10 GiB, failed/interrupted capture marking, and an automatic
  retention pass after completion (`--no-rotate` opts out).
- `compress_reports.py`: verified streaming migration with a resumable manifest
  (`published` before source removal, `deleted` after), exclusive temporary
  creation, no-clobber publication, source identity re-checks, root/symlink
  validation, and nonzero status on any failure.
- `rotate_runs.py`: retention over marker-bearing direct children only; keeps
  the newest three completed disposable runs, protects pins, protected names,
  active and failed runs, and the current run; `--pin/--unpin` persist a reason;
  `--rotate-failed` is the explicit cleanup path for failed captures.

Marker schema: `{"version": 1, "kind": "panel-capture", "state":
"active|completed|failed", "pinned": bool, "reason": str|null, "created_utc",
"updated_utc", "head", "binary_sha256", "panel_sha256", "route",
"fixed_denominator", "selected_pairs"}`.

## Tests and checks

33 focused Python tests pass (ran 2026-09-20):

- `test_capture_storage.py` (9): gzip report reference digests and migrated-path
  resolution, plain/compressed open, truncated archive fails closed, marker
  pin/reason preservation across the on-disk transition, `.json`/`.gz`/manifest
  resolution, free-space preflight.
- `test_compress_reports.py` (9): dry-run listing, outside-root and symlink
  rejection, verified apply, conflicting archive failure, matching archive
  adoption, resumable published state, root validation on resume, mutation
  during read rejected, finalize rejecting a source changed after publish.
- `test_rotate_runs.py` (8): marker-only direct-child discovery, protected
  parent with nested raw metadata, active/failed protection, newest-N, pin
  persistence, unmarked legacy ignored, outside root ignored, removal failure
  nonzero.
- `next/development/test_capture_comparisons.py` (7): streams beyond pipe
  capacity to both pipes with exact gzip content and bounded prefix, exit code
  preservation, pump write failure reported without deadlock, direct child
  terminated on pump failure, process-group termination reaping a real
  grandchild, wrapper exit with a pipe-holding grandchild failing the bounded
  shutdown, and `timeout --foreground` descendant cleanup after exit 124.

Semantic preservation: rebuilding the CLI after the storage changes
(`9be5b6466b2208ec9b6fa1f3e85a335f8af641105c26523fc4d6be4b99611ed2`) and
re-running ``faa-maintenance-records-c-to-d`` with default settings produced a
report whose logical SHA-256 equals the candidate H2 capture
(`ff6a36a22f5c3ee282f9bed9adbbcd76dd4c61990a5da25d45093965dd8a8fc2`, exit 3),
so storage changes do not alter comparison content. The H2 candidate is not yet
accepted: its source-retention audit is still pending.

End-to-end smoke capture: `benchmark/realworld/cache/storage-smoke-native`
(marked `completed`, one pair `irs-schedule-se-2024-to-2025`, gzip report and
gzip stdout/stderr logs, logical/stored digest binding, retention pass
protecting the current run).

## Limits

Legacy/unmarked directories are compression-only and are never rotated
automatically. Runner paths that emit summaries (`capture-summary.sh`) remain
plaintext because they are small durable evidence; the large-report path
(`next/capture-baseline.sh`) now writes gzip and decompresses at most the
existing 128 MiB summary limit into a removed temporary file. Future captures
default to compression and retention; final acceptance runs are pinned before
the final two-run evaluation.
