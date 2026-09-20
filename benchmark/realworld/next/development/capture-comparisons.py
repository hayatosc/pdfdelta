#!/usr/bin/env python3
"""Capture three separate routes for explicitly selected registered pairs."""

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
from threading import Thread
import time


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def pump_compressed(source, target, errors, prefix=None, prefix_limit=4096):
    """Streams one process pipe into gzip without a plaintext copy.

    The optional `prefix` bytearray keeps only the bounded leading bytes for a
    diagnostic message; the rest is compressed and never buffered whole. A
    failure is recorded in `errors` so the caller fails the capture instead of
    silently accepting a missing or truncated log.
    """
    try:
        with target.open("wb") as raw:
            with gzip.GzipFile(
                filename="", mode="wb", fileobj=raw, compresslevel=6, mtime=0
            ) as sink:
                while True:
                    chunk = source.read(65536)
                    if not chunk:
                        break
                    if prefix is not None and len(prefix) < prefix_limit:
                        prefix.extend(chunk[: prefix_limit - len(prefix)])
                    sink.write(chunk)
    except BaseException as error:  # noqa: BLE001 - reported to the caller
        errors.append(f"{target}: {type(error).__name__}: {error}")
    finally:
        try:
            source.close()
        except OSError:
            pass


def terminate_process_group(process):
    """Kills the whole owned session, not only the direct wrapper child.

    `start_new_session=True` makes the child a session and process-group
    leader, so the group id is the child pid even after the wrapper exits or is
    reaped while descendants still hold the inherited pipes.
    """
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        try:
            process.kill()
        except ProcessLookupError:
            pass


def run_with_compressed_logs(
    command, stdout_path, stderr_path, prefix_limit=4096, shutdown_seconds=10.0
):
    """Runs one command with both streams compressed by concurrent pumps.

    The wrapper starts a new session (`start_new_session=True`) and the timeout
    wrapper keeps descendants inside it (`--foreground`), so a pump failure can
    terminate the whole owned tree instead of leaving `timeout`/`pdfdelta`
    descendants holding the pipes. Pump failures are returned so the caller
    marks the capture failed.
    """
    process = subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True
    )
    errors = []
    stderr_prefix = bytearray()
    threads = [
        Thread(
            target=pump_compressed,
            args=(process.stdout, stdout_path, errors),
            daemon=True,
        ),
        Thread(
            target=pump_compressed,
            args=(process.stderr, stderr_path, errors, stderr_prefix, prefix_limit),
            daemon=True,
        ),
    ]
    for thread in threads:
        thread.start()
    try:
        while process.poll() is None:
            if errors:
                terminate_process_group(process)
                break
            time.sleep(0.05)
        exit_code = process.wait()
        if exit_code in (124, 137, 143):
            # The wrapper timed out or was signalled; `--foreground` keeps its
            # descendants inside the owned group, so release their pipes too.
            terminate_process_group(process)
    except BaseException:
        terminate_process_group(process)
        process.wait()
        raise
    finally:
        # Never report a closed log while a pump is still writing: give the
        # pumps a bounded window, then force the owned group and record the
        # unfinished pump as a capture failure.
        deadline = time.monotonic() + shutdown_seconds
        for thread in threads:
            thread.join(timeout=max(0.0, deadline - time.monotonic()))
        if any(thread.is_alive() for thread in threads):
            terminate_process_group(process)
            errors.append("log pump did not finish after group termination")
    return exit_code, errors, bytes(stderr_prefix)


def logical_report_sha256(path):
    """SHA-256 of the uncompressed report content.

    Reports are written as gzip streams; the logical digest keeps every
    binding comparable with plaintext-era records.
    """
    digest = hashlib.sha256()
    with gzip.open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def logical_report_bytes(path):
    """Uncompressed report byte length."""
    total = 0
    with gzip.open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            total += len(chunk)
    return total


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pdfdelta", type=Path)
    parser.add_argument("cache", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--manifest", type=Path, default=Path(__file__).with_name("inputs.json"))
    parser.add_argument("--pair", action="append", required=True)
    parser.add_argument("--implementation", required=True)
    parser.add_argument("--route", action="append", choices=("native", "text", "all"),
                        help="Capture only these routes; defaults to all three")
    args = parser.parse_args()
    binary = args.pdfdelta.resolve(strict=True)
    manifest = args.manifest
    inputs = json.loads(manifest.read_text())
    if not inputs["frozen"]:
        parser.error("comparison requires frozen input selection")
    pairs = {pair["id"]: pair for pair in inputs["pairs"]}
    if len(set(args.pair)) != len(args.pair) or any(id not in pairs for id in args.pair):
        parser.error("pair IDs must be unique registered IDs")
    references = {}
    for id in args.pair:
        reference = manifest.parent / "annotations" / f"{id}.expectations.json"
        if not reference.is_file():
            parser.error(f"fix a source reference or unresolved annotation record first: {reference}")
        contract = json.loads(reference.read_text())
        if contract.get("pair") != id or contract.get("frozen_before_comparison") is not True:
            parser.error(f"source reference is not frozen for {id}")
        annotation = reference.with_name(f"{id}.json")
        if contract.get("annotation_status") != "unresolved" and (
            not annotation.is_file() or sha256(annotation) != contract.get("annotation_sha256")
        ):
            parser.error(f"source annotation hash mismatch for {id}")
        references[id] = sha256(reference)
    args.output.mkdir(parents=True, exist_ok=False)
    records = {
        "version": 1,
        "implementation_commit": args.implementation,
        "binary_sha256": sha256(binary),
        "inputs_sha256": sha256(manifest),
        "timeout_seconds": 180,
        "limit_scale": 1,
        "annotation_scoring": False,
        "provenance_note": "Caller identifies the build commit; the driver binds the executable hash but does not independently prove its build provenance.",
        "runs": [],
    }
    routes = {
        "native": ["--native-text-only"],
        "text": ["--channels", "text"],
        "all": ["--channels", "text,visual,forms,relations"],
    }
    if args.route:
        if len(set(args.route)) != len(args.route):
            parser.error("routes must be unique")
        routes = {route: routes[route] for route in args.route}
    for id in args.pair:
        pair = pairs[id]
        paths = [args.cache / f"{id}-{side}.pdf" for side in ("old", "new")]
        hashes = [pair[side].get("sha256") for side in ("old", "new")]
        acquisition_valid = all(path.is_file() and sha256(path) == hash for path, hash in zip(paths, hashes))
        for route, flags in routes.items():
            stem = args.output / f"{id}-{route}"
            record = dict(pair=id, route=route, old_sha256=hashes[0], new_sha256=hashes[1],
                          reference_sha256=references[id])
            if not acquisition_valid:
                record["status"] = "missing_input_or_hash_mismatch"
            else:
                report = Path(f"{stem}.json.gz")
                command = [str(binary), *map(str, paths), *flags, "--limit-scale", "1",
                           "--quiet", "--json", str(report)]
                stdout_path = Path(f"{stem}.stdout.gz")
                stderr_path = Path(f"{stem}.stderr.gz")
                exit_code, log_errors, stderr_prefix = run_with_compressed_logs(
                    ["/usr/bin/time", "-f", "%e %M", "-o", str(stem.with_suffix(".time")),
                     "timeout", "--foreground", "--kill-after=5", "180", *command],
                    stdout_path,
                    stderr_path,
                )
                time = stem.with_suffix(".time").read_text().splitlines()[-1].split()
                record.update(exit_code=exit_code, wall_seconds=float(time[0]), peak_rss_kib=int(time[1]),
                              status="captured" if not log_errors and exit_code in (0, 1, 3) and report.is_file() else "failed")
                record.update(report_bytes=report.stat().st_size if report.is_file() else None,
                              report_sha256=logical_report_sha256(report) if report.is_file() else None,
                              report_logical_bytes=logical_report_bytes(report) if report.is_file() else None,
                              report_file_sha256=sha256(report) if report.is_file() else None,
                              report_encoding="gzip" if report.is_file() else None,
                              stdout_file=str(stdout_path),
                              stdout_sha256=sha256(stdout_path) if stdout_path.is_file() else None,
                              stderr_file=str(stderr_path),
                              stderr_sha256=sha256(stderr_path) if stderr_path.is_file() else None,
                              log_errors=log_errors)
                record["stderr"] = stderr_prefix.decode("utf-8", "replace")
            records["runs"].append(record)
            (args.output / "runs.json").write_text(json.dumps(records, indent=2) + "\n")
            print(id, route, record["status"], flush=True)


if __name__ == "__main__":
    main()
