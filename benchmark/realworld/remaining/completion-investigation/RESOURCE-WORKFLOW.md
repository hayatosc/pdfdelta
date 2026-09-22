# Resource containment and recurring cleanup

All heavy jobs run through `run-bounded.sh`, which owns the shared cap and lock
and then delegates to `housekeeping.py --run` inside the same slice.

- Shared cgroup: `pdfdelta-heavy.slice` (MemoryMax requested 6000000000; the
  kernel rounds to 5999996928, verified as 0 < memory.max <= 6000000000,
  memory.swap.max == 0). Missing/unbounded values fail closed.
- Single-job lock: flock on the owner-only task tmp `heavy.lock`; no nested
  bypass, no stale lock directory.
- Cargo defaults: `CARGO_BUILD_JOBS=1`, `CARGO_INCREMENTAL=0`,
  `CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0`; `TMPDIR` points at
  the task-owned tmp directory.
- Pre and post (finally) cleanup: managed rotation of marked capture runs
  (newest 3, failed included, pins/active/unknown protected) plus caps:
  free >= 10 GiB, managed cache <= 20 GiB, target <= 4 GiB, task tmp <= 512 MiB.
  Over-cap cleanup deletes only reproducible growth (target/debug when no cargo
  is active, `.disposable` files, and marked completed/failed unpinned runs in
  the task tmp); anything unproven is kept and the job refuses.
- Check-only is read-only. Empty commands and check-only+run are rejected.

Tests: `test_housekeeping.py` (8 cases: protections, failed rotation, caps,
check-only read-only, relative root sizing, run_job exit preservation with
pre/post rotation, tmp marker validation, target/tmp refusal) and
`test_native_retention_audit.py` (31 cases, including the streaming-projection
loader regressions and candidate-source accounting).

## Startup from a cold boot

`run-bounded.sh` recreates the runtime slice if its cgroup is missing: it sets
`MemoryMax`/`MemorySwapMax` at runtime, and if no control group exists it
creates one with an empty scope, then reads and verifies the kernel values
before any heavy child runs. Verified with an isolated `pdfdelta-smoke.slice`
(same 6e9/swap0 properties): cgroup created, `memory.max` and
`memory.swap.max` read back correctly, then stopped. The shared slice is never
stopped for this test.

## Reproducible ijson dependency

The audit tooling needs `ijson` (one j). A machine-independent invocation is:

```
uv run --with ijson python3 native_retention_audit.py ...
```

Do not commit machine-specific uv cache paths; the wrapper environment only
needs a Python with `ijson` importable.

## Test inventory

- `test_housekeeping.py`: 9 cases.
- `test_native_retention_audit.py`: 31 cases.
- Full-capture memory: kernel `memory.peak` reached the 6e9 cap; `memory.events`
  recorded max=24088 with oom=0, oom_kill=0, oom_group_kill=0 (reclaim, not
  failure). Earlier all-zero events describe the NIST audit/gate phase only.
