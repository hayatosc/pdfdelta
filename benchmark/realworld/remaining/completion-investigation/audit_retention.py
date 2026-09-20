#!/usr/bin/env python3
"""Linear source-retention audit between two captures.

Streams each report once and hashes the source-bound payload members:
top-level members stop only at the next two-space key (all nested content is
included), and assessment submembers stop only at the next sibling four-space
key inside the assessment object. Assessment work counters
(`work_limit`, `work_used`, `work_by_stage`, `candidates_truncated`) are the
only fields excluded.

The audit fails on a truncated report, a missing required member, or a
duplicate member key. Two reference modes are supported:

- ``baseline`` (default): every non-proxy right report must be byte-identical
  to the frozen scorecard hash, and every proxy pair must prove that its left
  capture matches the frozen baseline hash before members are compared.
- ``accepted``: the left capture is the reference. Its metadata must bind the
  frozen panel, the native route, the fixed denominator, the ``1.0`` limit
  scale, the ``180`` second timeout and a nonempty binary identity, and the
  compared report hashes must match that metadata. Proxy pairs then compare
  members against this verified left capture instead of the initial baseline.

``expected_differences`` excuses proxy differences only for the pairs listed.
When it is empty no difference is excused, so a historical audit that accepted
the FAA maintenance pair must pass that pair explicitly.
"""

import argparse
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
PANEL_SHA256 = "c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744"
BASELINE_SCORECARD = (
    ROOT / "benchmark/realworld/results/native-12-of-36-2026-09-20/baseline-scorecard.json"
)
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


def capture_bindings(capture_dir):
    """Returns (pair -> recorded logical sha, capture identity) or (None, {})."""
    summary_path = Path(capture_dir) / "summary.json"
    if not summary_path.is_file():
        return None, {}
    summary = json.loads(summary_path.read_text())
    rows = {
        row["pair"]: row["report"]["sha256"]
        for row in summary.get("rows", [])
        if isinstance(row.get("report"), dict) and "sha256" in row["report"]
    }
    identity = {
        "head": summary.get("head"),
        "binary_sha256": (summary.get("binary") or {}).get("sha256"),
        "panel_sha256": (summary.get("panel") or {}).get("sha256"),
        "fixed_denominator": summary.get("fixed_denominator"),
        "route": summary.get("route"),
        "limit_scale": summary.get("limit_scale"),
        "timeout_seconds": summary.get("timeout_seconds"),
    }
    return rows, identity


def require_capture_binding(label, recorded, pair, logical_sha, identity, required):
    if recorded is None:
        if required:
            raise AuditError(f"{label} capture metadata is missing")
        return
    expected = recorded.get(pair)
    if expected is None:
        raise AuditError(f"{label} capture metadata lacks {pair}")
    if expected != logical_sha:
        raise AuditError(
            f"{label} report for {pair} does not match its capture metadata"
        )
    if identity.get("fixed_denominator") != 36:
        raise AuditError(f"{label} capture does not carry the frozen denominator")
    panel_sha = identity.get("panel_sha256")
    if panel_sha != PANEL_SHA256:
        raise AuditError(f"{label} capture does not bind the frozen panel")
    if required:
        if identity.get("route") != "native":
            raise AuditError(f"{label} capture does not use the native route")
        if identity.get("limit_scale") != 1:
            raise AuditError(f"{label} capture does not use the fixed limit scale")
        if identity.get("timeout_seconds") != 180:
            raise AuditError(f"{label} capture does not use the fixed timeout")
        binary_sha = identity.get("binary_sha256")
        if not isinstance(binary_sha, str) or not binary_sha:
            raise AuditError(f"{label} capture has no binary identity")


def run_audit(
    proxy_pairs,
    left_dir,
    right_dir,
    reference_mode,
    baseline_path=None,
    expected_differences=(),
):
    if reference_mode not in ("baseline", "accepted"):
        raise AuditError(f"unsupported reference mode {reference_mode}")
    left_dir = Path(left_dir)
    right_dir = Path(right_dir)
    baseline = {
        row["pair"]: row
        for row in json.loads(
            (baseline_path or BASELINE_SCORECARD).read_text()
        )["records"]
    }
    left_recorded, left_identity = capture_bindings(left_dir)
    right_recorded, right_identity = capture_bindings(right_dir)
    all_pairs = [row["pair"] for row in baseline.values()]
    report = {
        "reference_mode": reference_mode,
        "left_capture": left_identity,
        "right_capture": right_identity,
        "whole_report_identical": [],
        "proxy_member_compared": [],
        "different": [],
        "pairs": {},
    }
    for pair in all_pairs:
        right = capture_module.resolve_report_path(right_dir / pair / f"{pair}-native.json")
        right_sha = logical_sha256(right)
        require_capture_binding(
            "right",
            right_recorded,
            pair,
            right_sha,
            right_identity,
            required=reference_mode == "accepted",
        )
        left = None
        try:
            left = capture_module.resolve_report_path(left_dir / pair / f"{pair}-native.json")
        except FileNotFoundError:
            left = None
        left_sha = logical_sha256(left) if left is not None else None
        if left_sha is not None:
            require_capture_binding(
                "left",
                left_recorded,
                pair,
                left_sha,
                left_identity,
                required=reference_mode == "accepted",
            )
        baseline_sha = baseline[pair]["report_sha256"]
        entry = {
            "baseline_report_sha256": baseline_sha,
            "left_report_sha256": left_sha,
            "right_report_sha256": right_sha,
        }
        if pair not in proxy_pairs:
            if reference_mode == "accepted":
                if left_sha is None:
                    raise AuditError(f"{pair}: accepted reference capture is missing")
                if right_sha != left_sha:
                    raise AuditError(
                        f"{pair}: changed but not listed as a proxy pair"
                    )
                entry["verification"] = "accepted_capture"
            else:
                if right_sha != baseline_sha or (
                    left_sha is not None and left_sha != baseline_sha
                ):
                    raise AuditError(
                        f"{pair}: non-proxy report is not byte-identical to baseline"
                    )
                entry["verification"] = (
                    "baseline_and_left" if left_sha is not None else "baseline_hash"
                )
            report["whole_report_identical"].append(pair)
            report["pairs"][pair] = entry
            continue
        if left is None:
            raise AuditError(f"{pair}: proxy capture is missing")
        if reference_mode == "baseline" and left_sha != baseline_sha:
            raise AuditError(f"{pair}: proxy capture does not match the baseline hash")
        members = audit_pair(left, right)
        entry.update(members)
        entry["verification"] = "member_compare"
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
    expected = set(expected_differences)
    report["expected_differences"] = sorted(expected)
    report["expected_different"] = [
        item for item in report["different"] if item["pair"] in expected
    ]
    report["unexpected_different"] = [
        item for item in report["different"] if item["pair"] not in expected
    ]
    print(
        "whole reports identical:",
        len(report["whole_report_identical"]),
        "proxy pairs compared:",
        len(report["proxy_member_compared"]),
        "expected different:",
        sorted(item["pair"] for item in report["expected_different"]),
        "unexpected different:",
        sorted(item["pair"] for item in report["unexpected_different"]),
    )
    return report


def parse_cli(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("proxy_pairs", type=Path)
    parser.add_argument("left_dir", type=Path)
    parser.add_argument("right_dir", type=Path)
    parser.add_argument("--reference", choices=("baseline", "accepted"), default="baseline")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path(
            "benchmark/realworld/cache/native-12-of-36-2026-09-20/retention-audit.json"
        ),
    )
    parser.add_argument("--expected", type=Path, default=None)
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_cli(sys.argv[1:] if argv is None else argv)
    proxy_pairs = json.loads(args.proxy_pairs.read_text())
    expected = json.loads(args.expected.read_text()) if args.expected else []
    report = run_audit(
        proxy_pairs,
        ROOT / args.left_dir,
        ROOT / args.right_dir,
        args.reference,
        expected_differences=expected,
    )
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 0 if not report["unexpected_different"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
