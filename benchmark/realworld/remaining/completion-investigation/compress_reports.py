#!/usr/bin/env python3
"""Losslessly compress capture-owned plaintext reports to gzip.

Ownership is proven from capture tooling metadata: a file is compressed only
when a sibling or ancestor `summary.json` names it as a report, or a
`runs.json` names its `{pair}-{route}.json` path. Only regular files inside
the requested root are considered; symlinks and paths escaping the root are
rejected.

Safety properties:

- dry-run lists sizes only and never compresses;
- the compressed temporary file is created exclusively and verified by
  decompressing it and comparing the logical digest and length with the source;
- an existing `.gz` is never overwritten: it is adopted only when its exact
  logical content matches the source, otherwise the file fails;
- the manifest is written atomically with `published` state before the source
  is removed and `deleted` state afterwards, so an interruption is resumable;
- the source inode, size and mtime are re-checked immediately before removal;
- any failed file produces a nonzero exit status.
"""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import stat
import sys
import tempfile


CHUNK = 4 * 1024 * 1024
REPO_ROOT = Path(__file__).resolve().parents[4]
MANIFEST_VERSION = 1


def file_sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def logical_digest(path):
    """SHA-256 and byte length of the decompressed content."""
    digest = hashlib.sha256()
    total = 0
    with gzip.open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK), b""):
            digest.update(chunk)
            total += len(chunk)
    return digest.hexdigest(), total


def within_root(path, root):
    try:
        path.relative_to(root)
    except ValueError:
        return False
    return True


def owned_reports(root):
    """Returns regular report files proven owned by capture metadata."""
    owned = set()
    for metadata in root.rglob("summary.json"):
        try:
            payload = json.loads(metadata.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for row in payload.get("rows", []):
            report = row.get("report") or {}
            path = report.get("path")
            if path:
                candidate = Path(path)
                if not candidate.is_absolute():
                    candidate = REPO_ROOT / candidate
                owned.add(candidate)
    for metadata in root.rglob("runs.json"):
        try:
            payload = json.loads(metadata.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for run in payload.get("runs", []):
            pair = run.get("pair")
            route = run.get("route")
            if pair and route:
                owned.add(metadata.parent / f"{pair}-{route}.json")
    accepted = set()
    for path in owned:
        if path.suffix != ".json":
            continue
        resolved = path.resolve()
        if not within_root(resolved, root) or not resolved.is_file():
            continue
        if path.is_symlink() or resolved.is_symlink():
            continue
        mode = resolved.stat().st_mode
        if not stat.S_ISREG(mode):
            continue
        accepted.add(resolved)
    return accepted


def write_manifest(path, entries):
    temporary = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    payload = {"version": MANIFEST_VERSION, "entries": entries}
    with temporary.open("w") as stream:
        json.dump(payload, stream, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def load_manifest(path):
    if not path.is_file():
        return []
    payload = json.loads(path.read_text())
    if payload.get("version") != MANIFEST_VERSION:
        raise ValueError(f"unsupported manifest version in {path}")
    return payload.get("entries", [])


def upsert(entries, entry):
    for index, existing in enumerate(entries):
        if existing.get("old_path") == entry.get("old_path"):
            entries[index] = entry
            return
    entries.append(entry)


class SourceChanged(Exception):
    pass


def source_stat(path):
    info = path.stat()
    return (info.st_ino, info.st_size, info.st_mtime_ns)


def adopt_existing(target, source, before):
    """Returns a published entry when target matches the source bytes.

    `before` is the source identity captured before reading; it is compared
    again after the read so a source modified during verification is rejected.
    """
    digest, length = logical_digest(target)
    source_digest = hashlib.sha256()
    source_length = 0
    with source.open("rb") as stream:
        for chunk in iter(lambda: stream.read(CHUNK), b""):
            source_digest.update(chunk)
            source_length += len(chunk)
    if source_stat(source) != before:
        raise SourceChanged("source changed while adopting an archive")
    if digest != source_digest.hexdigest() or length != source_length:
        raise ValueError("existing compressed archive has different content")
    return {
        "old_path": str(source),
        "new_path": str(target),
        "logical_sha256": digest,
        "file_sha256": file_sha256(target),
        "logical_bytes": length,
        "stored_bytes": target.stat().st_size,
        "encoding": "gzip",
        "state": "published",
        "source_stat": list(before),
    }


def compress_one(path):
    """Publishes a verified archive without removing the source yet.

    The source identity is captured before it is read and re-checked after
    compression, after verification and again before the archive is published,
    so a file modified during the read is never treated as verified. The caller
    records the returned `published` entry before `finalize` removes the source.
    """
    before = source_stat(path)
    target = Path(f"{path}.gz")
    if target.exists():
        return adopt_existing(target, path, before)
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f"{target.name}.tmp.", dir=target.parent
    )
    temporary = Path(temporary_name)
    published = False
    logical = hashlib.sha256()
    logical_bytes = 0
    try:
        with path.open("rb") as source, os.fdopen(descriptor, "wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=6, mtime=0) as encoder:
                while True:
                    chunk = source.read(CHUNK)
                    if not chunk:
                        break
                    logical.update(chunk)
                    logical_bytes += len(chunk)
                    encoder.write(chunk)
            raw.flush()
            os.fsync(raw.fileno())
        verify, verify_bytes = logical_digest(temporary)
        if verify != logical.hexdigest() or verify_bytes != logical_bytes:
            raise ValueError("verification digest does not match the source")
        if source_stat(path) != before:
            raise SourceChanged("source changed while compressing")
        # Publish without clobbering a concurrent archive.
        os.link(temporary, target)
        published = True
        temporary.unlink()
        return {
            "old_path": str(path),
            "new_path": str(target),
            "logical_sha256": logical.hexdigest(),
            "file_sha256": file_sha256(target),
            "logical_bytes": logical_bytes,
            "stored_bytes": target.stat().st_size,
            "encoding": "gzip",
            "state": "published",
            "source_stat": list(before),
        }
    except BaseException:
        temporary.unlink(missing_ok=True)
        if published:
            # The published archive was produced from a source that changed
            # while it was read; never leave it as a verified binding.
            target.unlink(missing_ok=True)
        raise


def finalize(entry, manifest_path, entries):
    """Removes the source of a published archive and records `deleted`."""
    source = Path(entry["old_path"])
    expected = entry.get("source_stat")
    if source.exists():
        if expected is not None and list(source_stat(source)) != expected:
            raise SourceChanged("source changed after publishing its archive")
        source.unlink()
    entry["state"] = "deleted"
    upsert(entries, entry)
    write_manifest(manifest_path, entries)


def resume(entries, apply, root):
    """Finishes entries whose compressed archive is published but not removed.

    Manifest entries are re-validated against the requested root before any
    deletion: both paths must resolve inside the root and neither the source
    nor the archive may be a symlink.
    """
    failures = 0
    for entry in entries:
        if entry.get("state") != "published":
            continue
        source = Path(entry["old_path"])
        target = Path(entry["new_path"])
        paths_valid = True
        for candidate in (source, target):
            resolved = candidate.resolve()
            if not within_root(resolved, root) or candidate.is_symlink():
                paths_valid = False
                break
        if not paths_valid or not target.is_file() or target.is_symlink():
            failures += 1
            print(f"UNRESOLVED {entry['old_path']}: unsafe or missing paths", flush=True)
            continue
        digest, length = logical_digest(target)
        if digest != entry["logical_sha256"] or length != entry["logical_bytes"]:
            failures += 1
            print(f"UNRESOLVED {entry['old_path']}: archive digest mismatch", flush=True)
            continue
        expected = entry.get("source_stat")
        if not apply:
            print(f"would resume {entry['old_path']}", flush=True)
            continue
        if source.exists() and expected is not None:
            if list(source_stat(source)) != expected:
                failures += 1
                print(f"UNRESOLVED {entry['old_path']}: source changed", flush=True)
                continue
            source.unlink()
            print(f"resumed {entry['old_path']}", flush=True)
        else:
            print(f"resumed (missing source) {entry['old_path']}", flush=True)
        entry["state"] = "deleted"
    return failures


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--min-bytes", type=int, default=1_000_000)
    parser.add_argument("--manifest", type=Path, default=None)
    args = parser.parse_args()
    root = args.root.resolve()
    manifest_path = args.manifest or (root / "compression-manifest.json")
    entries = load_manifest(manifest_path)
    failures = resume(entries, args.apply, root)
    if args.apply and failures == 0:
        write_manifest(manifest_path, entries)
    done = {entry.get("old_path") for entry in entries}
    candidates = sorted(
        (
            path
            for path in owned_reports(root)
            if path.stat().st_size >= args.min_bytes and str(path) not in done
        ),
        key=lambda path: -path.stat().st_size,
    )
    total_before = sum(path.stat().st_size for path in candidates)
    print(f"{len(candidates)} owned plaintext reports, {total_before} bytes")
    if not args.apply:
        for path in candidates:
            print(f"would compress {path} ({path.stat().st_size} bytes)")
        print("dry run: no files changed; pass --apply to publish")
        return 0 if failures == 0 else 1
    saved = 0
    for path in candidates:
        before = path.stat().st_size
        try:
            entry = compress_one(path)
        except Exception as error:  # noqa: BLE001 - report and keep original
            failures += 1
            print(f"FAILED {path}: {type(error).__name__}: {error}", flush=True)
            continue
        # Record the binding while the source still exists.
        upsert(entries, entry)
        write_manifest(manifest_path, entries)
        try:
            finalize(entry, manifest_path, entries)
        except SourceChanged as error:
            failures += 1
            print(f"FAILED {path}: {error}", flush=True)
            continue
        saved += before - entry["stored_bytes"]
        print(f"compressed {path} {before} -> {entry['stored_bytes']} bytes", flush=True)
    print(f"saved {saved} bytes ({saved / (1024 ** 3):.2f} GiB)")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
