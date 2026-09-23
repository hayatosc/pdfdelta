#!/usr/bin/env python3
"""Bounded retention for capture runs that carry a lifecycle marker.

Rotation only considers **direct children** of the requested root that contain
a valid `.capture-run.json` marker written by the capture entry point. Runs
without a marker (legacy or unrelated directories) are never rotated, so
unrelated or nested raw metadata cannot make a directory deletable. A run is
rotated only when its marker state is `completed`, it is not pinned, its name
does not match a protected marker, and it is not named by `--protect`.

`--pin`/`--unpin` edit the marker persistently (with a reason). Active and
failed runs are never rotated automatically; `--rotate-failed` adds failed
runs to the disposable set for explicit cleanup. The default policy keeps the
newest `--keep` disposable completed runs; `--max-age-days` optionally keeps
younger runs in addition. Dry run is the default and every removal failure
produces a nonzero exit status.
"""

import argparse
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import shutil
import sys
import types


MARKER = ".capture-run.json"
STATES = ("active", "completed", "failed")
PROTECTED_MARKERS = ("baseline", "accepted", "final", "pinned", "control")


def load_marker(directory):
    path = directory / MARKER
    if path.is_symlink() or not path.is_file():
        return None
    try:
        marker = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError):
        return None
    if (
        isinstance(marker, dict)
        and marker.get("version") == 1
        and marker.get("state") in STATES
        and isinstance(marker.get("pinned"), bool)
    ):
        return marker
    return None


def write_marker(directory, marker):
    path = directory / MARKER
    temporary = directory / f"{MARKER}.tmp"
    temporary.write_text(json.dumps(marker, indent=2) + "\n")
    os.replace(temporary, path)


def discover(root):
    """Returns marked direct-child run directories inside the root."""
    runs = []
    for child in sorted(root.iterdir()):
        if not child.is_dir() or child.is_symlink():
            continue
        resolved = child.resolve()
        try:
            resolved.relative_to(root)
        except ValueError:
            continue
        marker = load_marker(child)
        if marker is not None:
            runs.append((child, marker))
    return runs


def marker_timestamp(marker, directory):
    for key in ("updated_utc", "created_utc"):
        value = marker.get(key)
        if isinstance(value, str):
            try:
                return datetime.fromisoformat(value).timestamp()
            except ValueError:
                continue
    return directory.stat().st_mtime


def protected_by_name(directory, protect_names):
    name = directory.name.lower()
    if any(marker in name for marker in PROTECTED_MARKERS):
        return True
    resolved = directory.resolve()
    for entry in protect_names:
        candidate = Path(entry)
        if not candidate.is_absolute():
            candidate = Path.cwd() / candidate
        candidate = candidate.resolve()
        # Only the named run itself is protected; a path never protects or
        # endangers nested children that are not discovered anyway.
        if resolved == candidate:
            return True
    return False


def rotate(args, parser):
    root = args.root.resolve()
    if not root.is_dir():
        parser.error(f"root is not a directory: {root}")
    runs = discover(root)
    keep_count = max(args.keep, 0)
    age_cutoff = (
        datetime.now(timezone.utc) - timedelta(days=args.max_age_days)
    ).timestamp() if args.max_age_days is not None else None
    disposable = []
    for directory, marker in runs:
        state = marker["state"]
        if marker["pinned"]:
            print(f"pinned    {directory} ({state})")
            continue
        if protected_by_name(directory, args.protect):
            print(f"protected {directory} ({state})")
            continue
        if state == "active":
            print(f"active    {directory}")
            continue
        if state == "failed" and not args.rotate_failed:
            print(f"failed    {directory} (use --rotate-failed to clean up)")
            continue
        if state == "failed":
            disposable.append((marker_timestamp(marker, directory), directory, state))
            continue
        disposable.append((marker_timestamp(marker, directory), directory, state))
    disposable.sort(reverse=True)
    keep = []
    for _, directory, state in disposable:
        if len(keep) < keep_count:
            keep.append(directory)
            print(f"keep      {directory} ({state})")
        elif age_cutoff is not None and directory.stat().st_mtime >= age_cutoff:
            keep.append(directory)
            print(f"young     {directory} ({state})")
    rotate_set = [
        (directory, state)
        for _, directory, state in disposable
        if directory not in keep
    ]
    failures = 0
    reclaimed = 0
    for directory, state in rotate_set:
        size = reclaimable_bytes([directory])
        if not args.apply:
            print(f"would remove {directory} ({state}, {size} bytes)")
            continue
        try:
            shutil.rmtree(directory)
        except OSError as error:
            failures += 1
            print(f"FAILED {directory}: {error}", flush=True)
            continue
        # `size` was computed before removal, when the shared-link counts were
        # still observable; a failed removal never reaches this line.
        reclaimed += size
        print(f"removed {directory} ({state}, {size} bytes)", flush=True)
    if not args.apply:
        print("dry run: no runs removed; pass --apply to rotate")
    else:
        print(f"reclaimed {reclaimed} bytes ({reclaimed / (1024 ** 3):.2f} GiB)")
    return 0 if failures == 0 else 1


def update_pin(target, pinned, reason, parser):
    target = target.resolve()
    if not target.is_dir() or target.is_symlink():
        parser.error(f"not a marked run directory: {target}")
    marker = load_marker(target)
    if marker is None:
        parser.error(f"missing valid {MARKER} in {target}")
    marker["pinned"] = pinned
    marker["reason"] = reason if pinned else None
    marker["updated_utc"] = datetime.now(timezone.utc).isoformat()
    write_marker(target, marker)
    print(f"{'pinned' if pinned else 'unpinned'} {target}")
    return 0


class _LoggingParser:
    """Non-fatal error channel for programmatic retention."""

    def __init__(self, log):
        self._log = log

    def error(self, message):
        raise RuntimeError(message)


def reclaimable_bytes(directories):
    """Bytes physically freed when all links inside the given directories go.

    An inode is counted once, and only when every existing hard link to it
    lives inside the directories being removed. Symlinks are never followed.
    """
    inodes = {}
    for directory in directories:
        for path in Path(directory).rglob("*"):
            if path.is_symlink() or not path.is_file():
                continue
            stat = path.stat()
            key = (stat.st_dev, stat.st_ino)
            entry = inodes.setdefault(key, {"size": stat.st_size, "links": stat.st_nlink, "count": 0})
            entry["count"] += 1
    return sum(
        entry["size"]
        for entry in inodes.values()
        if entry["count"] >= entry["links"]
    )


def run_retention(
    root,
    keep=3,
    max_age_days=None,
    protect=(),
    rotate_failed=False,
    apply=True,
    log=print,
):
    """Runs the same retention policy the CLI uses. Returns the failure count."""
    args = types.SimpleNamespace(
        root=Path(root),
        keep=keep,
        max_age_days=max_age_days,
        protect=list(protect),
        rotate_failed=rotate_failed,
        apply=apply,
    )
    return rotate(args, _LoggingParser(log))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path("."))
    parser.add_argument("--keep", type=int, default=3,
                        help="newest disposable completed runs to retain")
    parser.add_argument("--max-age-days", type=float, default=None,
                        help="also keep disposable runs younger than this")
    parser.add_argument("--protect", action="append", default=[],
                        help="exact run directory to protect; repeatable")
    parser.add_argument("--rotate-failed", action="store_true",
                        help="include failed runs in explicit cleanup")
    parser.add_argument("--pin", type=Path, help="pin one marked run")
    parser.add_argument("--unpin", type=Path, help="unpin one marked run")
    parser.add_argument("--reason", default=None, help="pin reason")
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if args.pin is not None and args.unpin is not None:
        parser.error("--pin and --unpin are mutually exclusive")
    if args.pin is not None:
        return update_pin(args.pin, True, args.reason, parser)
    if args.unpin is not None:
        return update_pin(args.unpin, False, None, parser)
    return rotate(args, parser)


if __name__ == "__main__":
    raise SystemExit(main())
