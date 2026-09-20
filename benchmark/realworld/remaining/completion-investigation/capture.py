#!/usr/bin/env python3
"""Replay the frozen natural panel and separate acquisition from search failures."""

import argparse
from collections import Counter
from datetime import datetime, timezone
import gzip
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile


PANEL = Path("benchmark/realworld/followup/panel.json")
DRIVER = Path("benchmark/realworld/next/development/capture-comparisons.py")
# A capture run must start with room for compressed reports and the source
# archive; the reserve is intentionally far above the compressed size of a
# full panel capture.
MIN_FREE_BYTES = 10 * 1024 * 1024 * 1024
COMPRESSION_MANIFEST = "compression-manifest.json"


def resolve_report_path(path):
    """Resolves a report path that may have been migrated to `.gz`.

    Resolution order: the exact path, `<path>.gz`, then an ancestor
    `compression-manifest.json` mapping an old plaintext path to its verified
    archive. The logical content hash is unchanged by migration.
    """
    path = Path(path)
    if path.is_file():
        return path
    compressed = Path(f"{path}.gz")
    if compressed.is_file():
        return compressed
    for directory in [path.parent, *path.parents]:
        manifest = directory / COMPRESSION_MANIFEST
        if not manifest.is_file():
            continue
        try:
            payload = json.loads(manifest.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        for entry in payload.get("entries", []):
            if entry.get("old_path") == str(path):
                candidate = Path(entry["new_path"])
                if candidate.is_file():
                    return candidate
    raise FileNotFoundError(f"report not found: {path}")


def require_free_space(path):
    """Fails closed before a capture that could exhaust the filesystem."""
    free = shutil.disk_usage(path).free
    if free < MIN_FREE_BYTES:
        raise RuntimeError(
            f"refusing to start capture: only {free} bytes free, "
            f"reserve is {MIN_FREE_BYTES} bytes"
        )


def open_report(path):
    """Opens a native report, decompressing gzip content transparently."""
    path = Path(path)
    if path.suffix == ".gz":
        return gzip.open(path, "rb")
    return path.open("rb")


def reference(path):
    path = Path(path)
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "sha256": digest}


def write_run_marker(path, fields):
    """Writes the versioned capture-run lifecycle marker atomically."""
    temporary = path.with_name(f"{path.name}.tmp")
    temporary.write_text(json.dumps(fields, indent=2) + "\n")
    os.replace(temporary, path)


def run_marker_fields(record, args, selected_pairs, state, created, existing=None):
    """Builds marker fields, preserving pins and reasons across transitions."""
    existing = existing or {}
    return {
        "version": 1,
        "kind": "panel-capture",
        "state": state,
        "pinned": bool(existing.get("pinned", False)),
        "reason": existing.get("reason"),
        "created_utc": existing.get("created_utc", created),
        "updated_utc": datetime.now(timezone.utc).isoformat(),
        "head": record["head"],
        "binary_sha256": record["binary"]["sha256"],
        "panel_sha256": record["panel"]["sha256"],
        "route": args.route,
        "fixed_denominator": 36,
        "selected_pairs": selected_pairs,
    }


def transition_run_marker(marker_path, record, args, selected_pairs, state, created):
    """Moves the on-disk lifecycle marker to `state`, preserving pins.

    The previous marker is read first so a pin or reason set while the capture
    was active survives the completed/failed transition.
    """
    marker_path = Path(marker_path)
    existing = None
    if marker_path.is_file():
        try:
            existing = json.loads(marker_path.read_text())
        except (OSError, json.JSONDecodeError):
            existing = None
    fields = run_marker_fields(
        record, args, selected_pairs, state, created, existing=existing
    )
    write_run_marker(marker_path, fields)
    return fields


def report_reference(path):
    """Binds a report by its logical content digest and its stored file.

    The logical digest hashes the uncompressed content, so bindings stay
    comparable with plaintext-era records while the stored file is compressed.
    A migrated plaintext path resolves to its verified archive first.
    """
    path = resolve_report_path(path)
    digest = hashlib.sha256()
    logical_bytes = 0
    with open_report(path) as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
            logical_bytes += len(chunk)
    return {
        "path": str(path),
        "sha256": digest.hexdigest(),
        "logical_bytes": logical_bytes,
        "file_sha256": reference(path)["sha256"],
        "bytes": path.stat().st_size,
        "encoding": "gzip" if path.suffix == ".gz" else "identity",
    }


def summarize(report):
    if report.get("schema_version") != 2:
        raise ValueError("shared-channel report schema is not 2")
    components = []
    reasons = Counter()
    comparisons = Counter()
    for scope in report["comparison"]["scopes"]:
        result = scope["result"]
        proposals = result["candidates"]["proposals"]
        reasons.update(result["unresolved"])
        for comparison in result["comparisons"]:
            if comparison["compared"]:
                comparisons[comparison["interpretation"]] += 1
        for component in result["matching"]["components"]:
            if component["exhaustive"]:
                continue
            selected = [proposals[index] for index in component["proposals"]]
            components.append({
                "algorithm": component["algorithm"],
                "proposals": len(selected),
                "grouped_proposals": sum(
                    len(proposal["old"]) != 1 or len(proposal["new"]) != 1
                    for proposal in selected
                ),
                "mandatory": len(component["mandatory"]),
                "explored_states": component["explored_states"],
                "assignment_work": component["assignment_work"],
            })
    return {
        "comparison_complete": report["comparison_complete"],
        "coverage": report["coverage"],
        "typed_changes": report["typed_changes"],
        "inferred_changes": report["inferred_changes"],
        "scope_content_changes": report["scope_content_changes"],
        "inferred_scope_changes": report["inferred_scope_changes"],
        "successful_comparisons": dict(comparisons),
        "incomplete_components": components,
        "unresolved_reasons": dict(reasons),
        "acquisition": {
            side: {
                "pages": report[side]["pages"],
                "native_glyphs": report[side]["native_glyphs"],
                "non_text_paint_pages": len(report[side]["non_text_paint_pages"]),
                "text_issues": dict(Counter(
                    issue["reason"] for issue in report[side]["issues"]
                    if issue["channel"] == "text"
                )),
            }
            for side in ("old", "new")
        },
    }


class NativeReportError(ValueError):
    """Raised when a native report is malformed, truncated or out of contract."""


_JSON_SPECIAL = re.compile(rb'["{}\\[\\]]')
_STRING_TAIL = re.compile(rb'(?<!\\)(?:\\\\)*"')
_WHITESPACE = b" \t\r\n"
_INTEGER = re.compile(rb"-?\d+")
_NATIVE_STATUSES = ("no_content_change", "detected", "indeterminate")
_NATIVE_REQUIRED = (
    "comparison_complete",
    "difference_status",
    "established_changes",
    "content_changes",
    "proven_changed_regions",
    "formatting_only_changes",
    "uncertain_changes",
    "unresolved_regions",
    "unsupported_extraction_issues",
    "unresolved_extraction_issues",
    "tentative_candidates",
    "old_alignment_coverage",
    "new_alignment_coverage",
)


class _NativeReportReader:
    """Reads the pretty-printed native report with bounded memory.

    The report is serde-style pretty JSON: every top-level member starts on a
    line indented by two spaces, and a nested value at that indentation ends at
    a line holding its closing `}` or `]`. Raw newlines cannot appear inside
    JSON strings, so searching for those terminators is structure-safe, and
    multi-gigabyte members are skipped at byte-search speed instead of being
    parsed. Unexpected formatting, truncation or trailing data fail closed
    rather than being misread.
    """

    _MEMBER_PREFIX = b"\n  "
    _ROOT_CLOSE = b"\n}"
    _ASSESSMENT_KEY = b'\n    "candidates_truncated": '
    _SMALL_CAP = 8 * 1024 * 1024

    def __init__(self, path, chunk=8 * 1024 * 1024):
        self._path = Path(path)
        self._chunk = chunk
        self._stream = open_report(self._path)
        self._buffer = b""
        self._offset = 0
        self._position = 0
        self._eof = False

    def __enter__(self):
        return self

    def __exit__(self, *_exception):
        self._stream.close()

    def _compact(self):
        if self._position:
            self._offset += self._position
            self._buffer = self._buffer[self._position:]
            self._position = 0

    def _fill(self):
        self._compact()
        if self._eof:
            return False
        try:
            data = self._stream.read(self._chunk)
        except (OSError, EOFError, gzip.BadGzipFile) as error:
            raise NativeReportError(f"native report decompression failed: {error}") from error
        if not data:
            self._eof = True
            return False
        self._buffer += data
        return True

    def _peek(self, count=1):
        while len(self._buffer) - self._position < count:
            if not self._fill():
                break
        return self._buffer[self._position:self._position + count]

    def _read(self, count):
        data = self._peek(count)
        if len(data) != count:
            raise NativeReportError("native report is truncated")
        self._position += count
        return data

    def _seek(self, offset):
        if self._offset <= offset <= self._offset + len(self._buffer):
            self._position = offset - self._offset
            return
        self._stream.seek(offset)
        self._buffer = b""
        self._offset = offset
        self._position = 0
        self._eof = False

    def _find(self, needle, start):
        """Returns the absolute offset of `needle` at or after `start`."""
        self._seek(start)
        while True:
            index = self._buffer.find(needle, self._position)
            if index != -1:
                self._position = index
                return self._offset + index
            keep = len(needle) - 1
            if keep > 0 and len(self._buffer) > keep:
                self._position = len(self._buffer) - keep
            else:
                self._position = len(self._buffer)
            if not self._fill():
                return None

    def _read_at(self, start, end):
        if end < start:
            raise NativeReportError("native report member range is inverted")
        if end - start > self._SMALL_CAP:
            raise NativeReportError("native report member exceeds its byte cap")
        # Reuse the already decompressed window when the range is still
        # buffered; reopening a gzip stream would decompress from the start.
        if self._offset <= start and end <= self._offset + len(self._buffer):
            return self._buffer[start - self._offset:end - self._offset]
        try:
            with open_report(self._path) as stream:
                stream.seek(start)
                return stream.read(end - start)
        except (OSError, EOFError, gzip.BadGzipFile) as error:
            raise NativeReportError(f"native report decompression failed: {error}") from error

    def _read_string(self):
        """Returns the raw bytes of one JSON string, including its quotes."""
        if self._peek(1) != b'"':
            raise NativeReportError("native report string is malformed")
        start = self._position
        while True:
            match = _STRING_TAIL.search(self._buffer, start + 1)
            if match is not None:
                raw = self._buffer[start:match.end()]
                self._position = match.end()
                return raw
            self._position = start
            if not self._fill():
                raise NativeReportError("native report string is unterminated")
            start = self._position

    def _read_line_value(self, start):
        """Reads one scalar line value, leaving its comma for the separator."""
        end = self._find(b"\n", start)
        if end is None:
            raise NativeReportError("native report member line is truncated")
        raw = self._read_at(start, end).rstrip()
        value_end = start + len(raw)
        if raw.endswith(b","):
            value_end -= 1
            raw = raw[:-1].rstrip()
        if not raw:
            raise NativeReportError("native report member value is empty")
        return raw, value_end

    def _read_assessment(self, value_start, value_end, members):
        members["assessment_present"] = True
        members["assessment_null"] = False
        marker = self._find(self._ASSESSMENT_KEY, value_start)
        if marker is None or marker >= value_end:
            raise NativeReportError("native report assessment lacks candidates_truncated")
        raw, line_end = self._read_line_value(marker + len(self._ASSESSMENT_KEY))
        if line_end > value_end:
            raise NativeReportError("native report assessment member escapes its object")
        if raw == b"true":
            members["candidates_truncated"] = True
        elif raw == b"false":
            members["candidates_truncated"] = False
        else:
            raise NativeReportError("native report candidates_truncated is not a boolean")
        self._seek(line_end)

    def read_report(self):
        """Returns bounded members of the top-level native report object."""
        if self._read(1) != b"{":
            raise NativeReportError("native report is not a JSON object")
        if self._peek(1) == b"}":
            raise NativeReportError("native report has no members")
        if self._peek(2) != b"\n ":
            raise NativeReportError("native report is not in the expected pretty format")
        members = {
            "assessment_present": False,
            "assessment_null": False,
            "candidates_truncated": None,
        }
        seen_keys = set()
        while True:
            marker = self._peek(3)
            if marker.startswith(self._ROOT_CLOSE):
                self._position += len(self._ROOT_CLOSE)
                break
            if marker != self._MEMBER_PREFIX:
                raise NativeReportError("native report member line is malformed")
            self._position += len(self._MEMBER_PREFIX)
            key = json.loads(self._read_string())
            if key in seen_keys:
                raise NativeReportError(f"native report repeats the member {key}")
            seen_keys.add(key)
            if self._read(1) != b":" or self._read(1) != b" ":
                raise NativeReportError("native report member key is malformed")
            value_start = self._offset + self._position
            first = self._peek(1)
            if first in (b"{", b"["):
                empty = b"{}" if first == b"{" else b"[]"
                if self._peek(2) == empty:
                    # serde pretty prints an empty container on one line; it has
                    # no indented closing line to search for.
                    value_end = value_start + len(empty)
                else:
                    terminator = b"\n  }" if first == b"{" else b"\n  ]"
                    close = self._find(terminator, value_start)
                    if close is None:
                        raise NativeReportError("native report member is unterminated")
                    value_end = close + len(terminator)
                if key in ("comparison_scope", "summary", "extraction"):
                    members[key] = json.loads(self._read_at(value_start, value_end))
                elif key == "assessment":
                    self._read_assessment(value_start, value_end, members)
                self._seek(value_end)
            elif first == b"n" and key == "assessment":
                if self._read(4) != b"null":
                    raise NativeReportError("native report assessment is malformed")
                members["assessment_present"] = True
                members["assessment_null"] = True
            else:
                raw, line_end = self._read_line_value(value_start)
                if key == "schema_version":
                    members[key] = raw
                elif key == "difference_status":
                    members[key] = raw
                self._seek(line_end)
            byte = self._peek(1)
            if byte == b",":
                self._position += 1
                if not self._peek(4).startswith(b'\n  "'):
                    raise NativeReportError("native report member comma is malformed")
            elif byte == b"\n":
                if self._peek(2) != b"\n}":
                    raise NativeReportError("native report member is missing its comma")
            else:
                raise NativeReportError("native report member line has trailing content")
        while True:
            if self._buffer[self._position:].strip(b" \t\r\n"):
                raise NativeReportError("native report has trailing data")
            self._position = len(self._buffer)
            if not self._fill():
                break
        return members


def read_native_report(path):
    """Extracts the native report fields with bounded memory.

    Reads the actual structure with a streaming scanner: required members and
    their types are validated here, `assessment: null` is distinguished from a
    missing or corrupt assessment, and truncation or trailing data raises
    instead of being silently accepted.
    """
    with _NativeReportReader(path) as reader:
        members = reader.read_report()
    if "schema_version" not in members:
        raise NativeReportError("native report lacks schema_version")
    if not _INTEGER.fullmatch(members["schema_version"]):
        raise NativeReportError("native report schema_version is not an integer")
    if "difference_status" not in members:
        raise NativeReportError("native report lacks difference_status")
    difference = json.loads(members["difference_status"])
    if not isinstance(difference, str):
        raise NativeReportError("native report difference_status is not a string")
    for key in ("comparison_scope", "summary", "extraction"):
        if key not in members:
            raise NativeReportError(f"native report lacks {key}")
        if not isinstance(members[key], dict):
            raise NativeReportError(f"native report {key} is not an object")
    if not members["assessment_present"]:
        raise NativeReportError("native report lacks an assessment member")
    return {
        "schema_version": int(members["schema_version"]),
        "difference_status": difference,
        "comparison_scope": members["comparison_scope"],
        "assessment_present": members["assessment_present"],
        "assessment_null": members["assessment_null"],
        "candidates_truncated": members["candidates_truncated"],
        "summary": members["summary"],
        "extraction": members["extraction"],
    }


def require_boolean(value, label):
    if type(value) is not bool:
        raise ValueError(f"native {label} must be a JSON boolean")
    return value


def require_count(value, label):
    if type(value) is not int or value < 0:
        raise ValueError(f"native {label} must be a non-negative integer")
    return value


def require_coverage(value, label, extraction_complete):
    """Mirrors the core coverage contract, including ratio availability."""
    if not isinstance(value, dict):
        raise ValueError(f"native {label} must be an object")
    resolved = require_count(value.get("resolved_tokens"), f"{label}.resolved_tokens")
    total = require_count(value.get("total_tokens"), f"{label}.total_tokens")
    if resolved > total:
        raise ValueError(f"native {label} resolves more tokens than it owns")
    ratio = value.get("ratio")
    expected = 1.0 if total == 0 else resolved / total
    if extraction_complete:
        if type(ratio) not in (int, float) or isinstance(ratio, bool):
            raise ValueError(f"native {label}.ratio must be a number")
        ratio = float(ratio)
        if (
            not math.isfinite(ratio)
            or not 0.0 <= ratio <= 1.0
            or abs(ratio - expected) > 1.0e-12
        ):
            raise ValueError(f"native {label}.ratio disagrees with its token counts")
    elif ratio is not None:
        raise ValueError(f"native {label}.ratio must be null while extraction is incomplete")
    return {"resolved_tokens": resolved, "total_tokens": total, "ratio": ratio}


def require_issues(value):
    if not isinstance(value, list):
        raise ValueError("native extraction.issues must be an array")
    kinds = Counter()
    for issue in value:
        if not isinstance(issue, dict):
            raise ValueError("native extraction issue must be an object")
        if issue.get("side") not in ("old", "new"):
            raise ValueError("native extraction issue side is invalid")
        kind = issue.get("kind")
        if kind not in ("unsupported", "unresolved"):
            raise ValueError("native extraction issue kind is invalid")
        if not isinstance(issue.get("description"), str):
            raise ValueError("native extraction issue description must be a string")
        if "page" in issue:
            require_count(issue["page"], "extraction issue page")
        if "retained_glyphs_before" in issue:
            require_count(issue["retained_glyphs_before"], "extraction issue glyphs")
        kinds[kind] += 1
    return kinds


def summarize_native(fields):
    """Validates bounded native report fields (schema 11) with fail-closed checks.

    Every required member is type-checked, the native scope must be text-only,
    coverage ratios must agree with their token counts, and the primary
    predicate is recomputed from the contract fields and required to agree with
    the report. A pair with no native tokens on either side is recorded as
    vacuous rather than as a meaningful success.
    """
    schema = fields.get("schema_version")
    if type(schema) is not int or schema != 11:
        raise ValueError("native report schema is not 11")
    status = fields.get("difference_status")
    if status not in _NATIVE_STATUSES:
        raise ValueError("native report difference status is invalid")
    scope = fields.get("comparison_scope")
    if not isinstance(scope, dict):
        raise ValueError("native report comparison_scope must be an object")
    if require_boolean(scope.get("supported_text"), "comparison_scope.supported_text") is not True:
        raise ValueError("native scope must compare supported text")
    if require_boolean(scope.get("images_compared"), "comparison_scope.images_compared") is not False:
        raise ValueError("native scope must not compare images")
    if not fields.get("assessment_present"):
        raise ValueError("native report lacks an assessment member")
    assessment_null = require_boolean(fields.get("assessment_null"), "assessment_null")
    truncated = fields.get("candidates_truncated")
    if assessment_null:
        if truncated is not None:
            raise ValueError("null native assessment must not carry candidates_truncated")
        assessment_complete = True
    else:
        truncated = require_boolean(truncated, "candidates_truncated")
        assessment_complete = not truncated
    summary = fields.get("summary")
    if not isinstance(summary, dict):
        raise ValueError("native report summary must be an object")
    for key in _NATIVE_REQUIRED:
        if key not in summary:
            raise ValueError(f"native summary lacks {key}")
    if summary["difference_status"] != status:
        raise ValueError("native summary difference status disagrees with the report")
    if summary.get("comparison_scope") != scope:
        raise ValueError("native summary comparison scope disagrees with the report")
    comparison_complete = require_boolean(
        summary["comparison_complete"], "summary.comparison_complete"
    )
    established = require_count(summary["established_changes"], "summary.established_changes")
    content = require_count(summary["content_changes"], "summary.content_changes")
    if established != content:
        raise ValueError("native established and content change counts disagree")
    proven = require_count(summary["proven_changed_regions"], "summary.proven_changed_regions")
    formatting = require_count(
        summary["formatting_only_changes"], "summary.formatting_only_changes"
    )
    uncertain = require_count(summary["uncertain_changes"], "summary.uncertain_changes")
    unresolved = require_count(summary["unresolved_regions"], "summary.unresolved_regions")
    tentative = require_count(summary["tentative_candidates"], "summary.tentative_candidates")
    unsupported = require_count(
        summary["unsupported_extraction_issues"], "summary.unsupported_extraction_issues"
    )
    unresolved_issues = require_count(
        summary["unresolved_extraction_issues"], "summary.unresolved_extraction_issues"
    )
    extraction = fields.get("extraction")
    if not isinstance(extraction, dict):
        raise ValueError("native report extraction must be an object")
    old_complete = require_boolean(extraction.get("old_complete"), "extraction.old_complete")
    new_complete = require_boolean(extraction.get("new_complete"), "extraction.new_complete")
    kinds = require_issues(extraction.get("issues"))
    if unsupported != kinds["unsupported"] or unresolved_issues != kinds["unresolved"]:
        raise ValueError("native extraction issue counts disagree with the report")
    old_coverage = require_coverage(
        summary["old_alignment_coverage"], "old_alignment_coverage", old_complete
    )
    new_coverage = require_coverage(
        summary["new_alignment_coverage"], "new_alignment_coverage", new_complete
    )
    recomputed = (
        tentative == 0
        and proven == 0
        and unresolved == 0
        and old_coverage["resolved_tokens"] == old_coverage["total_tokens"]
        and new_coverage["resolved_tokens"] == new_coverage["total_tokens"]
        and old_complete
        and new_complete
        and assessment_complete
    )
    if recomputed != comparison_complete:
        raise ValueError("native comparison predicate disagrees with its fields")
    if comparison_complete and status == "indeterminate":
        raise ValueError("a complete native comparison cannot be indeterminate")
    if not comparison_complete and status == "no_content_change":
        raise ValueError("an incomplete native comparison cannot report no content change")
    return {
        "comparison_complete": comparison_complete,
        "difference_status": status,
        "established_changes": established,
        "content_changes": content,
        "proven_changed_regions": proven,
        "formatting_only_changes": formatting,
        "uncertain_changes": uncertain,
        "tentative_candidates": tentative,
        "unresolved_regions": unresolved,
        "unsupported_extraction_issues": unsupported,
        "unresolved_extraction_issues": unresolved_issues,
        "old_extraction_complete": old_complete,
        "new_extraction_complete": new_complete,
        "assessment_null": assessment_null,
        "candidates_truncated": truncated,
        "images_compared": scope["images_compared"],
        "vacuous": old_coverage["total_tokens"] == 0
        and new_coverage["total_tokens"] == 0,
        "old_alignment_coverage": old_coverage,
        "new_alignment_coverage": new_coverage,
        "unresolved_reasons": dict(kinds),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--pair", action="append")
    parser.add_argument(
        "--route",
        choices=("native", "text"),
        default="native",
        help="comparison route; native is the corrected primary scope",
    )
    parser.add_argument(
        "--no-rotate",
        action="store_true",
        help="skip the automatic retention pass after a completed capture",
    )
    args = parser.parse_args()
    panel = json.loads(PANEL.read_text())
    pairs = panel["pairs"]
    if args.pair:
        ids = {pair["id"] for pair in pairs}
        if len(set(args.pair)) != len(args.pair) or not set(args.pair) <= ids:
            parser.error("pair IDs must be unique members of the frozen panel")
        pairs = [pair for pair in pairs if pair["id"] in args.pair]
    # Small documents run first so acquisition and solver pilots are available
    # while the full, unchanged denominator is still being captured.
    pairs.sort(key=lambda pair: sum(pair[side].get("bytes", 0) for side in ("old", "new")))
    require_free_space(args.output.parent)
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.output / "pdfdelta"
    shutil.copy2(args.binary, binary)
    production_diff = subprocess.check_output([
        "git", "diff", "HEAD", "--", "crates", "Cargo.toml", "Cargo.lock"
    ])
    (args.output / "production.patch").write_bytes(production_diff)
    source_paths = subprocess.check_output([
        "git", "ls-files", "--cached", "--others", "--exclude-standard", "--",
        "crates", "Cargo.toml", "Cargo.lock", ".cargo",
    ], text=True).splitlines()
    source_archive = args.output / "production.tar.gz"
    with tarfile.open(source_archive, "w:gz") as archive:
        for path in sorted(set(source_paths)):
            if Path(path).is_file():
                archive.add(path, arcname=path, recursive=False)
    shutil.copy2(__file__, args.output / "capture.py")
    record = {
        "version": 1,
        "head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
        "binary": reference(binary),
        "production_diff": reference(args.output / "production.patch"),
        "source_archive": reference(source_archive),
        "panel": reference(PANEL),
        "capture_script": reference(__file__),
        "driver": reference(DRIVER),
        "fixed_denominator": 36,
        "selected_pairs": len(pairs),
        "route": args.route,
        "limit_scale": 1,
        "timeout_seconds": 180,
        "rows": [],
    }
    marker = args.output / ".capture-run.json"
    created = datetime.now(timezone.utc).isoformat()
    transition_run_marker(marker, record, args, len(pairs), "active", created)
    failed = False
    try:
        for pair in pairs:
            destination = args.output / pair["id"]
            command = [
                sys.executable, str(DRIVER), str(binary),
                str(Path(pair["old"]["path"]).parent), str(destination),
                "--manifest", pair["historical_inputs"], "--pair", pair["id"],
                "--implementation", record["head"], "--route", args.route,
            ]
            result = subprocess.run(command, check=False)
            row = {"pair": pair["id"], "driver_exit_code": result.returncode, "command": command}
            runs = destination / "runs.json"
            if runs.is_file():
                row["runs"] = reference(runs)
                run = json.loads(runs.read_text())["runs"][0]
                row["status"] = run["status"]
                row["wall_seconds"] = run.get("wall_seconds")
                if run["route"] != args.route:
                    raise ValueError(
                        f"{pair['id']}: driver route {run['route']} is not {args.route}"
                    )
                if run["status"] == "captured":
                    report = resolve_report_path(
                        destination / f"{pair['id']}-{args.route}.json"
                    )
                    row["report"] = report_reference(report)
                    if row["report"]["sha256"] != run.get("report_sha256"):
                        raise ValueError(
                            f"{pair['id']}: report logical hash disagrees with the driver record"
                        )
                    if args.route == "native":
                        fields = read_native_report(report)
                        row["schema_version"] = fields["schema_version"]
                        row.update(summarize_native(fields))
                    else:
                        with open_report(report) as stream:
                            payload = json.load(stream)
                        row["schema_version"] = payload.get("schema_version")
                        row.update(summarize(payload))
            else:
                row["status"] = "driver_failed"
            failed |= row["status"] != "captured"
            record["rows"].append(row)
            record["complete_pairs"] = sum(row.get("comparison_complete", False) for row in record["rows"])
            (args.output / "summary.json").write_text(json.dumps(record, indent=2) + "\n")
    except BaseException:
        # Interrupted or failing captures stay protected: the marker records
        # the failure so automatic retention never removes a partial run.
        transition_run_marker(marker, record, args, len(pairs), "failed", created)
        raise
    transition_run_marker(
        marker,
        record,
        args,
        len(pairs),
        "completed" if not failed else "failed",
        created,
    )
    if not args.no_rotate:
        try:
            import rotate_runs

            retention_failures = rotate_runs.run_retention(
                root=args.output.parent,
                keep=3,
                protect=[str(args.output)],
                apply=True,
            )
            if retention_failures:
                print(f"warning: retention reported {retention_failures} failure(s)")
        except Exception as error:  # noqa: BLE001 - retention must not break capture
            print(f"warning: retention pass failed: {error}")
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
