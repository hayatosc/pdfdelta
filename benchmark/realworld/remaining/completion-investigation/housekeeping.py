#!/usr/bin/env python3
"""Recurring housekeeping for heavy jobs: managed rotation, caps and protections.

All decisions are explicit: only marked capture runs under the managed cache
root are rotated, unknown directories are never touched, and pinned/active
runs stay. Caps fail closed before a job starts.
"""

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

import rotate_runs

GIB = 1024**3


def managed_cache_bytes(root):
    total = 0
    root = Path(root).resolve()
    for directory, _marker in rotate_runs.discover(root):
        total += sum(path.stat().st_size for path in directory.rglob("*") if path.is_file())
    return total


def free_bytes(path):
    return shutil.disk_usage(path).free


def check_caps(root, free_min_bytes, cache_max_bytes, free_path):
    free = free_bytes(free_path)
    cache = managed_cache_bytes(root)
    if free < free_min_bytes:
        return False, f"free space {free} below minimum {free_min_bytes}"
    if cache > cache_max_bytes:
        return False, f"managed cache {cache} above maximum {cache_max_bytes}"
    return True, ""


def cleanup(root, keep, protect):
    import contextlib
    import io
    buffer = io.StringIO()
    with contextlib.redirect_stdout(buffer):
        failures = rotate_runs.run_retention(
            root,
            keep=keep,
            protect=protect,
            rotate_failed=True,
            apply=True,
            log=buffer.write,
        )
    return failures, buffer.getvalue().splitlines()


def target_tmp_bytes(target_dir, tmp_dir):
    def size(path):
        if not path or not Path(path).exists():
            return 0
        return sum(item.stat().st_size for item in Path(path).rglob("*") if item.is_file())
    return size(target_dir), size(tmp_dir)


TASK_TMP = Path("/tmp/opencode/pdfdelta-task")


def cargo_active():
    for name in ("cargo", "rustc"):
        if subprocess.run(["pgrep", "-x", name], capture_output=True).returncode == 0:
            return True
    return False


def cleanup_reproducible(target_dir, tmp_dir, over_target, over_tmp):
    """Deletes only reproducible growth we own, with verified deletions."""
    removed = 0
    if Path(target_dir).is_symlink():
        return removed
    target_dir = Path(target_dir).resolve()
    if target_dir.name != "target":
        return removed
    if over_target and not cargo_active():
        debug = target_dir / "debug"
        if debug.is_dir() and not debug.is_symlink():
            before = sum(item.stat().st_size for item in debug.rglob("*") if item.is_file())
            shutil.rmtree(debug)
            after = sum(item.stat().st_size for item in debug.rglob("*") if item.is_file()) if debug.exists() else 0
            if not debug.exists():
                removed += before - after
    if Path(tmp_dir).is_symlink():
        return removed
    tmp_dir = Path(tmp_dir).resolve()
    marker = tmp_dir / ".task-owned"
    if over_tmp and tmp_dir.is_dir() and marker.is_file():
        for item in tmp_dir.iterdir():
            if item.name in (".task-owned", "heavy.lock") or item.is_symlink():
                continue
            disposable = item.is_file() and item.name.endswith(".disposable")
            completed_dir = False
            if item.is_dir():
                marker_data = rotate_runs.load_marker(item)
                protected = rotate_runs.protected_by_name(item, [])
                completed_dir = (
                    marker_data is not None
                    and marker_data.get("state") in ("completed", "failed")
                    and not marker_data.get("pinned")
                    and not protected
                )
            if not (disposable or completed_dir):
                continue
            size = sum(entry.stat().st_size for entry in item.rglob("*") if entry.is_file()) if item.is_dir() else item.stat().st_size
            if item.is_dir():
                shutil.rmtree(item)
            else:
                item.unlink()
            if not item.exists():
                removed += size
    return removed


def run_job(root, argv, keep, protect, free_min_bytes, cache_max_bytes, free_path,
            target_dir, tmp_dir, target_max_bytes, tmp_max_bytes, log=print):
    """Cleanup+check, run argv, cleanup+check again, preserve the command exit."""
    if Path(target_dir).is_symlink() or Path(tmp_dir).is_symlink():
        log("target/tmp symlinks are refused")
        return 75
    root = Path(root).resolve()
    target_dir = Path(target_dir).resolve()
    tmp_dir = Path(tmp_dir).resolve()
    if not argv:
        log("empty command refused")
        return 75
    import os
    os.environ["TMPDIR"] = str(tmp_dir)
    failures, lines = cleanup(root, keep, protect)
    for line in lines:
        log(line)
    if failures:
        log(f"pre cleanup failures: {failures}")
        return 1
    ok, reason = check_caps(root, free_min_bytes, cache_max_bytes, free_path)
    if not ok:
        log(f"pre cap refusal: {reason}")
        return 75
    target_size, tmp_size = target_tmp_bytes(target_dir, tmp_dir)
    over_target = target_size > target_max_bytes
    over_tmp = tmp_size > tmp_max_bytes
    if over_target or over_tmp:
        cleanup_reproducible(target_dir, tmp_dir, over_target, over_tmp)
        target_size, tmp_size = target_tmp_bytes(target_dir, tmp_dir)
        if target_size > target_max_bytes or tmp_size > tmp_max_bytes:
            log(f"target/tmp cap refusal: target={target_size} tmp={tmp_size}")
            return 75
    status = 1
    try:
        completed = subprocess.run(argv, shell=False)
        status = completed.returncode
    finally:
        failures, lines = cleanup(root, keep, protect)
        for line in lines:
            log(line)
        ok, reason = check_caps(root, free_min_bytes, cache_max_bytes, free_path)
        if not ok:
            log(f"post cap refusal: {reason}")
            if status == 0:
                status = 1
        if failures:
            log(f"post cleanup failures: {failures}")
            if status == 0:
                status = 1
        target_size, tmp_size = target_tmp_bytes(target_dir, tmp_dir)
        over_target = target_size > target_max_bytes
        over_tmp = tmp_size > tmp_max_bytes
        if over_target or over_tmp:
            cleanup_reproducible(target_dir, tmp_dir, over_target, over_tmp)
            target_size, tmp_size = target_tmp_bytes(target_dir, tmp_dir)
            if target_size > target_max_bytes or tmp_size > tmp_max_bytes:
                log(f"post target/tmp cap refusal: target={target_size} tmp={tmp_size}")
                if status == 0:
                    status = 1
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--keep", type=int, default=3)
    parser.add_argument("--free-min-gib", type=float, default=10.0)
    parser.add_argument("--cache-max-gib", type=float, default=20.0)
    parser.add_argument("--free-path", type=Path, default=None)
    parser.add_argument("--protect", action="append", default=[])
    parser.add_argument("--check-only", action="store_true")
    parser.add_argument("--target", type=Path, default=None)
    parser.add_argument("--tmp", type=Path, default=None)
    raw = sys.argv[1:]
    child = None
    if "--run" in raw:
        index = raw.index("--run")
        child = raw[index + 1 :]
        if child and child[0] == "--":
            child = child[1:]
        raw = raw[:index]
    args = parser.parse_args(raw)
    if args.check_only and child is not None:
        parser.error("--check-only and --run are mutually exclusive")
    args.child = child

    if args.child is not None:
        argv = args.child
        target = args.target or Path(args.root).resolve().parents[3] / "target"
        tmp = args.tmp or TASK_TMP
        return run_job(
            args.root,
            argv,
            args.keep,
            args.protect,
            int(args.free_min_gib * GIB),
            int(args.cache_max_gib * GIB),
            args.free_path or args.root,
            target,
            tmp,
            4 * GIB,
            512 * 1024**2,
            log=print,
        )
    if args.check_only:
        free_path = args.free_path or args.root
        ok, reason = check_caps(
            args.root,
            int(args.free_min_gib * GIB),
            int(args.cache_max_gib * GIB),
            free_path,
        )
        if not ok:
            print(f"cap refusal: {reason}", file=sys.stderr)
            return 1
        print(f"housekeeping check ok free={free_bytes(free_path)} cache={managed_cache_bytes(args.root)}")
        return 0
    failures, lines = cleanup(args.root, args.keep, args.protect)
    for line in lines:
        print(line)
    if failures:
        print(f"cleanup failures: {failures}", file=sys.stderr)
        return 1
    free_path = args.free_path or args.root
    ok, reason = check_caps(
        args.root,
        int(args.free_min_gib * GIB),
        int(args.cache_max_gib * GIB),
        free_path,
    )
    if not ok:
        print(f"cap refusal: {reason}", file=sys.stderr)
        return 1
    print(f"housekeeping ok free={free_bytes(free_path)} cache={managed_cache_bytes(args.root)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
