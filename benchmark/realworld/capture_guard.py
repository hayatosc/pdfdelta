"""Local capture admission and restart checks; never deletes previous evidence."""

from contextlib import contextmanager
import fcntl
import gzip
import hashlib
import os
from pathlib import Path
import shutil
import tempfile


DEFAULT_RESERVE = 2_000_000_000


@contextmanager
def capture_lock(root):
    """Serialize captures across output directories, including orphaned children."""
    directory = root / "benchmark/realworld/results"
    directory.mkdir(parents=True, exist_ok=True)
    with (directory / ".native-capture.lock").open("a+b") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("another capture or its child still owns the capture lock") from error
        # Do not explicitly unlock: a live child inherits this open description.
        yield lock.fileno()


def preflight(destination, reserve=DEFAULT_RESERVE, report_budget=0):
    """Check space and real create/read/fsync/delete access as the running user."""
    usage = shutil.disk_usage(destination)
    required = reserve + report_budget
    if usage.free < required:
        raise ValueError(f"insufficient free bytes: {usage.free}; need {required}")
    probe = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination, prefix=".capture-probe-", delete=False) as stream:
            probe = Path(stream.name)
            stream.write(b"pdfdelta capture probe\n")
            stream.flush()
            os.fsync(stream.fileno())
        if probe.read_bytes() != b"pdfdelta capture probe\n":
            raise OSError("capture probe read did not match")
    finally:
        if probe is not None:
            probe.unlink()
    return {"free_bytes": usage.free, "reserve_bytes": reserve, "uid": os.getuid(), "gid": os.getgid()}


def file_sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def verify_saved_row(row, pair, root, destination, check_source, read_summary):
    """Reuse only complete records whose input and report identity still match."""
    if row["status"] != "captured":
        return False
    for side in ("old", "new"):
        if check_source(root, pair[side])["status"] != "verified":
            raise ValueError("saved capture input is missing or changed: " + pair["id"])
    report = destination / (pair["id"] + ".json.gz")
    if (not report.is_file() or report.stat().st_size != row["report_bytes"]
            or file_sha256(report) != row["report_sha256"]):
        raise ValueError("saved report is missing or changed: " + pair["id"])
    summary = read_summary(report)
    expected_exit = 3 if not summary["comparison_complete"] else (1 if summary["difference_status"] == "detected" else 0)
    if summary != row["summary"] or row["exit_code"] != expected_exit:
        raise ValueError("saved report contract changed: " + pair["id"])
    with gzip.open(destination / (pair["id"] + ".log.gz"), "rb") as stream:
        while stream.read(65536):
            pass
    return True


def retain_attempt(destination, pair_id):
    """Move interrupted outputs aside after obtaining the shared capture lock."""
    paths = [destination / (pair_id + suffix) for suffix in (".json.gz", ".log", ".log.gz")]
    existing = [path for path in paths if path.exists()]
    if not existing:
        return
    archive = destination / "attempts"
    archive.mkdir(exist_ok=True)
    attempt = Path(tempfile.mkdtemp(prefix=pair_id + "-", dir=archive))
    for path in existing:
        path.rename(attempt / path.name)


def retained_bytes(destination):
    return sum(path.stat().st_size for path in destination.rglob("*") if path.is_file())
