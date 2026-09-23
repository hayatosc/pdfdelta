# H33 storage dedup (hard-link sharing of identical completed reports)

Read-only dry-run first, then reviewed `--apply`, both under
`run-bounded.sh` (6 GB memory, swap 0, exclusive lock). Root scope:
`benchmark/realworld/cache/native-12-of-36-2026-09-20` only.

## Measured result
- Dry-run: 46 groups, 210 replacement paths, candidate_bytes 12,185,907,614,
  errors [].
- Apply rc=0: errors [], 46 groups, 210 paths atomically replaced with hard
  links to a verified identical canonical report.
- Verification: all 75 report paths re-hashed (SHA256+size) with mismatches [];
  post-apply dry-run candidate_bytes 0 / replaced 0 / already_shared 210.
- unique inode bytes 19,501,613,781 -> 7,315,706,167 (freed 12,185,907,614).
- du -sb 20,124,938,020 -> 7,939,030,406; df free 906,244,460,544 ->
  918,429,405,184.
- Parent cgroup `pdfdelta-heavy.slice/memory.events`: low 0, high 0, max
  319213 (max increased during the run); the oom and oom_kill counters are
  unchanged at 5 and 4.

## Limits and contracts
- Only valid marked completed runs are scanned; active, unmarked, unknown and
  external hard links are never touched. No run is deleted or unpinned.
- Report paths come only from bounded `summary.rows[].report` metadata
  (encoding gzip, stored `file_sha256`, integer `bytes`); the logical
  uncompressed `sha256` is never used for storage sharing.
- Verified shared reports become read-only. Completed reports are immutable:
  changing content requires a fresh capture or an atomic replacement, never an
  in-place write to a shared inode.
- `--apply` is idempotent; a second run reports candidate_bytes 0.

## Files
- `dedup-dry-run-readonly.json.gz`, `readonly-inventory.jsonl.gz`,
  `readonly-snapshot.json.gz`: pre-apply read-only audit (46 groups, 75 report
  hashes, binary/summary/source bindings).
- `apply.log.gz`, `post-apply-verify.json.gz`: apply output and post-apply
  re-hash plus counters.
- `binding-verify.json.gz`: summary/production.tar.gz/pdfdelta SHA256 re-checked
  against the pre-apply snapshot.
- `readonly-audit.py.gz`, `post-apply-verify.py.gz`: the verification scripts.
- `logs/`, `gates/`: storage test and quality gate logs.
