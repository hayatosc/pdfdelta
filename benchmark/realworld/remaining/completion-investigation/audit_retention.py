#!/usr/bin/env python3
"""Linear source-retention audit between two captures.

Streams each report once and hashes the source-bound payload members:
top-level members stop only at the next two-space key (all nested content is
included), and assessment submembers stop only at the next sibling four-space
key inside the assessment object. Assessment work counters
(`work_limit`, `work_used`, `work_by_stage`, `candidates_truncated`) are the
only fields excluded.

The audit fails on a truncated report, a missing required member, or a
duplicate member key. Proxy pairs must prove that the left capture's logical
report hash equals the frozen baseline hash before their members are compared.
"""

import hashlib
import json
from pathlib import Path
import re
import sys

ROOT = Path("/home/hayato/ghq/github.com/hayatosc/pdfdelta")
sys.path.insert(
    0,
    str(ROOT / "benchmark/realworld/remaining/completion-investigation"),
)
import capture as capture_module  # noqa: E402

CHUNK = 8 * 1024 * 1024
TOP_LEVEL = (
    "summary",
    "changes",
    "change_candidates",
    "proven_changed_regions",
    "formatting_only_changes",
    "unresolved_regions",
    "extraction",
)
NESTED = ("old_resolution", "new_resolution", "relations", "review_units")
KEY_LINE = re.compile(rb'\n( {2,8})"([A-Za-z_]+)": ')
MAX_KEY_BYTES = 32
SUFFIX_CAP = 4096


class AuditError(Exception):
    """Raised when a report is truncated, malformed or incomplete."""


def logical_sha256(path):
    digest = hashlib.sha256()
    with capture_module.open_report(path) as stream:
        for chunk in iter(lambda: stream.read(CHUNK), b""):
            digest.update(chunk)
    return digest.hexdigest()


def scan_member_digests(path):
    """Returns per-member digests plus completeness evidence.

    Returns `(digests, seen_counts, complete)` where `digests` maps a member
    key to its SHA-256, `seen_counts` counts key starts, and `complete` is
    false when the stream does not end at the root object close.
    """
    hashers = {}
    seen = {}
    current = None
    in_assessment = False
    pending = b""
    suffix = bytearray()
    with capture_module.open_report(path) as stream:
        while True:
            try:
                chunk = stream.read(CHUNK)
            except (OSError, EOFError) as error:
                raise AuditError(f"report read failed: {error}") from error
            if not chunk:
                break
            suffix.extend(chunk)
            if len(suffix) > SUFFIX_CAP:
                del suffix[: len(suffix) - SUFFIX_CAP]
            data = pending + chunk
            safe = max(0, len(data) - MAX_KEY_BYTES)
            cursor = 0
            for match in KEY_LINE.finditer(data):
                if match.start() >= safe:
                    break
                if current is not None:
                    hashers[current].update(data[cursor:match.start()])
                indent = len(match.group(1))
                key = match.group(2).decode()
                if indent == 2:
                    in_assessment = key == "assessment"
                    current = key if key in TOP_LEVEL else None
                    if current is not None:
                        seen[current] = seen.get(current, 0) + 1
                elif indent == 4 and in_assessment:
                    current = key if key in NESTED else None
                    if current is not None:
                        seen[current] = seen.get(current, 0) + 1
                # Other indentation levels are payload of the current member.
                if current is not None:
                    hashers.setdefault(current, hashlib.sha256())
                cursor = match.start()
            if current is not None:
                hashers[current].update(data[cursor:safe])
            pending = data[safe:]
    if current is not None:
        hashers[current].update(pending)
    # The root object must close with an unindented `}` line; an indented
    # closing brace left by a removed root close is not a complete report.
    trimmed = bytes(suffix).rstrip(b" \t\r\n")
    complete = trimmed == b"}" or trimmed.endswith(b"\n}")
    return (
        {key: digest.hexdigest() for key, digest in hashers.items()},
        seen,
        complete,
    )


def audit_pair(left_path, right_path):
    left_digests, left_seen, left_complete = scan_member_digests(left_path)
    right_digests, right_seen, right_complete = scan_member_digests(right_path)
    if not left_complete or not right_complete:
        raise AuditError("report is truncated")
    required = TOP_LEVEL + NESTED
    for key in required:
        if left_seen.get(key) != 1 or right_seen.get(key) != 1:
            raise AuditError(
                f"member {key} occurs {left_seen.get(key)}/{right_seen.get(key)} times"
            )
    differences = [
        key
        for key in required
        if left_digests.get(key) != right_digests.get(key)
    ]
    return {
        "left_members": left_digests,
        "right_members": right_digests,
        "different_members": differences,
    }


def main():
    args = sys.argv[1:]
    if len(args) != 3:
        raise SystemExit("usage: audit_retention.py PROXY_PAIRS LEFT_DIR RIGHT_DIR")
    proxy_pairs = json.loads(Path(args[0]).read_text())
    left_dir = ROOT / args[1]
    right_dir = ROOT / args[2]
    baseline = {
        row["pair"]: row
        for row in json.loads(
            (
                ROOT
                / "benchmark/realworld/results/native-12-of-36-2026-09-20/baseline-scorecard.json"
            ).read_text()
        )["records"]
    }
    all_pairs = [row["pair"] for row in baseline.values()]
    report = {
        "whole_report_identical": [],
        "proxy_member_compared": [],
        "different": [],
        "pairs": {},
    }
    for pair in all_pairs:
        right = capture_module.resolve_report_path(right_dir / pair / f"{pair}-native.json")
        right_sha = logical_sha256(right)
        baseline_sha = baseline[pair]["report_sha256"]
        left = None
        try:
            left = capture_module.resolve_report_path(left_dir / pair / f"{pair}-native.json")
        except FileNotFoundError:
            left = None
        left_sha = logical_sha256(left) if left is not None else None
        entry = {
            "baseline_report_sha256": baseline_sha,
            "left_report_sha256": left_sha,
            "right_report_sha256": right_sha,
        }
        if pair not in proxy_pairs:
            # A non-proxy pair is retained when the candidate report content
            # hash equals the frozen baseline hash (and the proxy copy, when it
            # exists, matches it too).
            if right_sha != baseline_sha or (
                left_sha is not None and left_sha != baseline_sha
            ):
                raise AuditError(
                    f"{pair}: non-proxy report is not byte-identical to baseline"
                )
            entry["verification"] = (
                "baseline_and_proxy" if left_sha is not None else "baseline_hash"
            )
            report["whole_report_identical"].append(pair)
            report["pairs"][pair] = entry
            continue
        if left is None:
            raise AuditError(f"{pair}: proxy capture is missing")
        if left_sha != baseline_sha:
            raise AuditError(f"{pair}: proxy capture does not match the baseline hash")
        entry["verification"] = "member_compare"
        try:
            members = audit_pair(left, right)
        except AuditError as error:
            raise AuditError(f"{pair}: {error}") from error
        entry.update(members)
        report["pairs"][pair] = entry
        report["proxy_member_compared"].append(pair)
        if members["different_members"]:
            report["different"].append(
                {"pair": pair, "members": members["different_members"]}
            )
        print(
            f"{pair}: {len(members['left_members'])} members compared, "
            f"{len(members['different_members'])} different",
            flush=True,
        )
    out = ROOT / "benchmark/realworld/cache/native-12-of-36-2026-09-20/retention-audit.json"
    out.write_text(json.dumps(report, indent=2) + "\n")
    print(
        "whole reports identical:",
        len(report["whole_report_identical"]),
        "proxy pairs compared:",
        len(report["proxy_member_compared"]),
    )
    unexpected = [
        item for item in report["different"] if item["pair"] not in ("faa-maintenance-records-c-to-d",)
    ]
    return 0 if not unexpected else 1


if __name__ == "__main__":
    raise SystemExit(main())
