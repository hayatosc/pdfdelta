#!/usr/bin/env python3
"""Share identical immutable compressed reports inside completed owned runs.

Report paths come only from each run's bounded summary rows: a row's `report`
object must declare encoding "gzip", a stored `file_sha256`, and integer
`bytes`. The logical `sha256` of the uncompressed report is never used for
storage sharing. Every component of a report path must be a real directory
inside the run (no symlinks), and the file itself must be a regular file.

Identical immutable reports are shared with an atomic hard link on the same
device, preserving every path and every byte. Both sides are re-verified
immediately before replacement; a unique temporary link owned by this
invocation is used and only ever removed after this invocation created it.
Verified shared reports become read-only. Dry-run by default.
"""

import argparse
import hashlib
import json
import os
import secrets
import stat as stat_module
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))

import rotate_runs  # noqa: E402

CHUNK = 1 << 20
MAX_SUMMARY_BYTES = 8 * 1024 * 1024
MAX_SUMMARY_ROWS = 20000


def sha256_and_size(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(CHUNK), b""):
            digest.update(chunk)
    stat = os.lstat(path)
    return digest.hexdigest(), stat.st_size


def fingerprint(path):
    value = os.lstat(path)
    return (
        value.st_dev,
        value.st_ino,
        value.st_size,
        value.st_mtime_ns,
        value.st_nlink,
    )


def completed_runs(root):
    for directory, marker in rotate_runs.discover(Path(root)):
        if marker.get("state") != "completed":
            continue
        yield directory


def has_symlink_component(path, stop):
    current = path
    while True:
        if os.path.islink(current):
            return True
        if current == stop or current.parent == current:
            return False
        current = current.parent


def expected_reports(run_dir, errors):
    """Report paths and stored digests from the run's bounded summary rows."""
    summary = run_dir / "summary.json"
    if summary.is_symlink() or not summary.is_file():
        return []
    if os.lstat(summary).st_size > MAX_SUMMARY_BYTES:
        errors.append(f"{summary}: summary exceeds {MAX_SUMMARY_BYTES} bytes")
        return []
    try:
        data = json.loads(summary.read_text())
    except (OSError, json.JSONDecodeError) as error:
        errors.append(f"{summary}: unreadable summary: {error}")
        return []
    rows = data.get("rows") if isinstance(data, dict) else None
    if not isinstance(rows, list) or len(rows) > MAX_SUMMARY_ROWS:
        errors.append(f"{summary}: summary rows missing or over {MAX_SUMMARY_ROWS}")
        return []
    root = run_dir.resolve()
    reports = []
    for row in rows:
        if not isinstance(row, dict) or "report" not in row:
            continue
        report = row.get("report")
        if not isinstance(report, dict):
            errors.append(f"{summary}: malformed report row")
            continue
        if report.get("encoding") != "gzip":
            continue
        path = report.get("path")
        stored = report.get("file_sha256")
        size = report.get("bytes")
        if (
            not isinstance(path, str)
            or not path.endswith(".json.gz")
            or not isinstance(stored, str)
            or len(stored) != 64
            or not isinstance(size, int)
            or isinstance(size, bool)
            or size < 0
        ):
            errors.append(f"{summary}: malformed claimed report metadata")
            continue
        candidate = Path(path)
        if not candidate.is_absolute():
            candidate = Path.cwd() / candidate
        if has_symlink_component(candidate, root):
            errors.append(f"{candidate}: symlink component refused")
            continue
        try:
            resolved = candidate.resolve()
        except OSError as error:
            errors.append(f"{candidate}: {error}")
            continue
        if not resolved.is_relative_to(root):
            errors.append(f"{candidate}: outside its run")
            continue
        reports.append((candidate, stored, size))
    return reports


def discover_groups(root):
    root = Path(root).resolve()
    groups = {}
    binding = {}
    errors = []
    for run_dir in completed_runs(root):
        for path, stored, size in expected_reports(run_dir, errors):
            try:
                stat = os.lstat(path)
            except OSError as error:
                errors.append(f"{path}: {error}")
                continue
            if not stat_module.S_ISREG(stat.st_mode):
                errors.append(f"{path}: not a regular report")
                continue
            before = fingerprint(path)
            actual_digest, actual_size = sha256_and_size(path)
            after = fingerprint(path)
            if before != after:
                errors.append(f"{path}: unstable during discovery")
                continue
            if actual_digest != stored or actual_size != size:
                errors.append(f"{path}: stored hash/size mismatch")
                continue
            groups.setdefault((actual_size, actual_digest), []).append(path)
            binding[path] = run_dir
    shared = {}
    for key, paths in groups.items():
        distinct = list(dict.fromkeys(paths))
        if len(distinct) > 1:
            shared[key] = distinct
    return shared, errors, binding


def links_accounted_for(paths, path):
    stat = os.lstat(path)
    accounted = 0
    for candidate in paths:
        try:
            candidate_stat = os.lstat(candidate)
        except OSError:
            continue
        if (candidate_stat.st_dev, candidate_stat.st_ino) == (
            stat.st_dev,
            stat.st_ino,
        ):
            accounted += 1
    return stat.st_nlink == accounted


def run_binding_valid(binding, path):
    run_dir = binding.get(path)
    if run_dir is None:
        return "no owning completed run binding"
    marker = rotate_runs.load_marker(run_dir)
    if marker is None or marker.get("state") != "completed":
        return "owning run is not a completed marked run"
    if has_symlink_component(path, run_dir.resolve()):
        return "symlink component refused"
    if not path.name.endswith(".json.gz"):
        return "not a report path"
    return None


def preflight(paths, size, digest, binding):
    """Whole-group preflight; returns a per-path error map without mutating."""
    if binding is None:
        raise ValueError("binding is required; None would bypass ownership validation")
    errors = {}
    canonical = paths[0]
    try:
        canonical_stat = os.lstat(canonical)
    except OSError as error:
        return {canonical: str(error)}
    if not stat_module.S_ISREG(canonical_stat.st_mode):
        return {canonical: "not a regular report"}
    if binding is not None:
        binding_error = run_binding_valid(binding, canonical)
        if binding_error is not None:
            return {canonical: binding_error}
    if not links_accounted_for(paths, canonical):
        errors[canonical] = "external hard link on canonical"
    canonical_before = fingerprint(canonical)
    canonical_digest, canonical_size = sha256_and_size(canonical)
    if canonical_digest != digest or canonical_size != size:
        errors[canonical] = "canonical content does not match the group digest"
    elif fingerprint(canonical) != canonical_before:
        errors[canonical] = "canonical unstable during preflight"
    for path in paths[1:]:
        if binding is not None:
            binding_error = run_binding_valid(binding, path)
            if binding_error is not None:
                errors[path] = binding_error
                continue
        try:
            stat = os.lstat(path)
        except OSError as error:
            errors[path] = str(error)
            continue
        if not stat_module.S_ISREG(stat.st_mode):
            errors[path] = "not a regular report"
            continue
        if stat.st_dev != canonical_stat.st_dev:
            errors[path] = "cross-device share refused"
            continue
        if (stat.st_dev, stat.st_ino) == (
            canonical_stat.st_dev,
            canonical_stat.st_ino,
        ):
            continue
        if stat.st_nlink > 1 and not links_accounted_for(paths, path):
            errors[path] = "external hard link present"
            continue
        before = fingerprint(path)
        actual_digest, actual_size = sha256_and_size(path)
        if actual_digest != digest or actual_size != size:
            errors[path] = "content changed before replacement"
            continue
        if fingerprint(path) != before:
            errors[path] = "unstable before replacement"
    return errors


def make_readonly(path):
    stat = os.lstat(path)
    if stat.st_mode & 0o222:
        os.chmod(path, stat.st_mode & ~0o222)


def apply_group(key, paths, apply_changes, errors, binding):
    if binding is None:
        raise ValueError("binding is required; None would bypass ownership validation")
    size, digest = key
    canonical = paths[0]
    replaced = 0
    skipped = 0
    preflight_errors = preflight(paths, size, digest, binding)
    if preflight_errors:
        for path, message in preflight_errors.items():
            errors.append(f"{path}: {message}")
        return 0, 0
    if not apply_changes:
        canonical_inode = (
            os.lstat(canonical).st_dev,
            os.lstat(canonical).st_ino,
        )
        to_replace = 0
        shared = 0
        for path in paths[1:]:
            stat = os.lstat(path)
            if (stat.st_dev, stat.st_ino) == canonical_inode:
                shared += 1
            else:
                to_replace += 1
        return to_replace, shared
    try:
        make_readonly(canonical)
    except OSError as error:
        errors.append(f"{canonical}: readonly failed: {error}")
        return 0, 0
    for path in paths[1:]:
        if binding is not None:
            canonical_binding_error = run_binding_valid(binding, canonical)
            if canonical_binding_error is not None:
                errors.append(f"{canonical}: {canonical_binding_error}")
                return replaced, skipped
            path_binding_error = run_binding_valid(binding, path)
            if path_binding_error is not None:
                errors.append(f"{path}: {path_binding_error}")
                continue
        current_stat = os.lstat(path)
        if (current_stat.st_dev, current_stat.st_ino) == (
            os.lstat(canonical).st_dev,
            os.lstat(canonical).st_ino,
        ):
            skipped += 1
            continue
        before_canonical = fingerprint(canonical)
        canonical_digest, canonical_size = sha256_and_size(canonical)
        if canonical_digest != digest or canonical_size != size:
            errors.append(f"{canonical}: canonical changed before replacement")
            continue
        if fingerprint(canonical) != before_canonical:
            errors.append(f"{canonical}: canonical unstable before replacement")
            continue
        before_destination = fingerprint(path)
        destination_digest, destination_size = sha256_and_size(path)
        if destination_digest != digest or destination_size != size:
            errors.append(f"{path}: content changed before replacement")
            continue
        if fingerprint(path) != before_destination:
            errors.append(f"{path}: unstable before replacement")
            continue
        if fingerprint(canonical) != before_canonical:
            errors.append(f"{canonical}: canonical changed during replacement")
            continue
        temporary = path.with_name(f"{path.name}.dedup-{os.getpid()}-{secrets.token_hex(8)}")
        created_by_us = False
        try:
            os.link(canonical, temporary)
            created_by_us = True
        except OSError as error:
            errors.append(f"{path}: {error}")
            continue
        link_error = None
        linked = fingerprint(temporary)
        if binding is not None and run_binding_valid(binding, canonical) is not None:
            link_error = f"{canonical}: binding changed at link time"
        elif binding is not None and run_binding_valid(binding, path) is not None:
            link_error = f"{path}: binding changed at link time"
        elif linked[:4] != before_canonical[:4]:
            link_error = f"{path}: linked inode differs from verified canonical"
        elif linked[4] != before_canonical[4] + 1:
            link_error = f"{path}: linked inode link count is not one more"
        elif fingerprint(path) != before_destination:
            link_error = f"{path}: destination changed at link time"
        if link_error is not None:
            errors.append(link_error)
            if created_by_us:
                try:
                    temporary.unlink()
                except OSError:
                    pass
            continue
        try:
            make_readonly(temporary)
            os.replace(temporary, path)
            created_by_us = False
        except OSError as error:
            errors.append(f"{path}: {error}")
            if created_by_us:
                try:
                    temporary.unlink()
                except OSError:
                    pass
            continue
        verify_digest, verify_size = sha256_and_size(path)
        if verify_digest != digest or verify_size != size:
            errors.append(f"{path}: post-replacement verification failed")
            continue
        replaced += 1
    return replaced, skipped


def candidate_savings(groups):
    """Bytes that would actually be freed, counting shared inodes once."""
    total = 0
    for key, paths in groups.items():
        inodes = set()
        for path in paths:
            stat = os.lstat(path)
            inodes.add((stat.st_dev, stat.st_ino))
        total += key[0] * (len(inodes) - 1)
    return total


def run(root, apply_changes):
    groups, errors, binding = discover_groups(root)
    total_groups = len(groups)
    candidate_bytes = candidate_savings(groups)
    replaced = 0
    skipped = 0
    for key, paths in sorted(groups.items(), key=lambda item: -item[0][0]):
        group_replaced, group_skipped = apply_group(
            key, paths, apply_changes, errors, binding
        )
        replaced += group_replaced
        skipped += group_skipped
    result = {
        "root": str(root),
        "apply": apply_changes,
        "groups": total_groups,
        "candidate_bytes": candidate_bytes,
        "replaced": replaced,
        "already_shared": skipped,
        "errors": errors,
    }
    print(json.dumps(result, sort_keys=True))
    return 0 if not errors else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    return run(args.root, args.apply)


if __name__ == "__main__":
    raise SystemExit(main())
