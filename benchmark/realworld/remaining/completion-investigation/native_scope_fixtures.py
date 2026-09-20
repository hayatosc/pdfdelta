#!/usr/bin/env python3
"""Minimal fixtures that exercise the real native-scope audit paths.

Each fixture builds a tiny but complete capture directory (36 panel rows, one
binary, per-pair runs records and pretty-printed native reports) so the
production reader, summarizer and collector can be tested end to end with
exactly one injected defect. Fixtures are deliberately small; they never stand
in for the natural panel measurements.
"""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
import zlib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture as capture_module  # noqa: E402


def file_digest(path: Path) -> str:
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def reference(path: Path, base: Path) -> dict:
    path = Path(path).resolve()
    try:
        recorded = str(path.relative_to(base.resolve()))
    except ValueError:
        recorded = str(path)
    return {"path": recorded, "sha256": file_digest(path), "bytes": path.stat().st_size}


def native_report(complete: bool = True, vacuous: bool = False, schema: int = 11) -> dict:
    """Builds one internally consistent native report (schema 11)."""
    if complete:
        difference = "no_content_change"
        old_total = 0 if vacuous else 2
        new_total = 0 if vacuous else 3
        old_resolved = old_total
        new_resolved = new_total
        old_ratio = 1.0
        new_ratio = 1.0
    else:
        difference = "indeterminate"
        old_total = 0
        new_total = 2
        old_resolved = 0
        new_resolved = 0
        old_ratio = None
        new_ratio = None
    issues = [] if complete else [
        {
            "side": "new",
            "kind": "unsupported",
            "scope": "page",
            "page": 2,
            "description": 'curved { clip } with \\"quotes\\" and a } brace',
        }
    ]
    return {
        "schema_version": schema,
        "difference_status": difference,
        "note": 'decoy {"summary": {"x": 1}} and a lone } and \\" escaped',
        "comparison_scope": {"supported_text": True, "images_compared": False},
        "assessment": {"policy_version": 1, "candidates_truncated": False},
        "summary": {
            "comparison_complete": complete,
            "difference_status": difference,
            "comparison_scope": {"supported_text": True, "images_compared": False},
            "established_changes": 0,
            "content_changes": 0,
            "proven_changed_regions": 0,
            "formatting_only_changes": 0,
            "uncertain_changes": 0,
            "unresolved_regions": 0 if complete else 1,
            "unsupported_extraction_issues": 0 if complete else 1,
            "unresolved_extraction_issues": 0,
            "tentative_candidates": 0,
            "old_alignment_coverage": {
                "resolved_tokens": old_resolved,
                "total_tokens": old_total,
                "ratio": old_ratio,
            },
            "new_alignment_coverage": {
                "resolved_tokens": new_resolved,
                "total_tokens": new_total,
                "ratio": new_ratio,
            },
        },
        "changes": [] if complete else [
            {"kind": "replacement", "old": "4", "new": "5"}
        ],
        "change_candidates": [] if complete else [
            {"kind": "replacement", "old": "4", "new": "5"}
        ],
        "proven_changed_regions": [],
        "formatting_only_changes": [],
        "unresolved_regions": [] if complete else [
            {"blocks": [4], "text": "Atta2chm0ent 2 4"}
        ],
        "extraction": {
            "old_complete": complete,
            "new_complete": complete,
            "issues": issues,
        },
    }


def write_report(path: Path, report: dict) -> None:
    path.write_text(json.dumps(report, indent=2) + "\n")


def build_capture(
    root: Path,
    pairs: list,
    panel_reference: dict,
    vacuous_pairs: tuple = (),
    with_failed_pair: bool = False,
) -> Path:
    """Writes a complete 36-row fixture capture and returns its summary path.

    With `with_failed_pair`, the last panel pair is recorded as an explicit
    memory-limit failure instead of a captured row; its runs and report
    fragments still exist so the failure evidence can be verified.
    """
    root = Path(root)
    root.mkdir(parents=True, exist_ok=True)
    binary = root / "binary"
    binary.write_bytes(b"fixture-binary")
    binary_ref = reference(binary, root)
    rows = []
    failed_pair = pairs[-1]["id"] if with_failed_pair else None
    for pair in pairs:
        pair_id = pair["id"]
        directory = root / pair_id
        directory.mkdir(parents=True, exist_ok=True)
        vacuous = pair_id in vacuous_pairs
        complete = pair_id != failed_pair
        report_path = directory / f"{pair_id}-native.json"
        write_report(report_path, native_report(complete=complete, vacuous=vacuous))
        exit_code = 0 if complete else 3
        runs = {
            "version": 1,
            "implementation_commit": "0" * 40,
            "binary_sha256": binary_ref["sha256"],
            "inputs_sha256": pair["historical_inputs_sha256"],
            "timeout_seconds": 180,
            "limit_scale": 1,
            "annotation_scoring": False,
            "runs": [
                {
                    "pair": pair_id,
                    "route": "native",
                    "old_sha256": pair["old"]["sha256"],
                    "new_sha256": pair["new"]["sha256"],
                    "reference_sha256": "1" * 64,
                    "exit_code": exit_code,
                    "wall_seconds": 0.01,
                    "peak_rss_kib": 1,
                    "status": "captured",
                    "report_bytes": report_path.stat().st_size,
                    "report_sha256": file_digest(report_path),
                    "stderr": "",
                }
            ],
        }
        runs_path = directory / "runs.json"
        runs_path.write_text(json.dumps(runs, indent=2) + "\n")
        if complete:
            fields = capture_module.read_native_report(report_path)
            row = {
                "pair": pair_id,
                "status": "captured",
                "driver_exit_code": 0,
                "command": ["python", "capture-comparisons.py", str(binary), pair_id],
                "wall_seconds": 0.01,
                "runs": reference(runs_path, root),
                "report": reference(report_path, root),
                "schema_version": fields["schema_version"],
            }
            row.update(capture_module.summarize_native(fields))
        else:
            row = {
                "pair": pair_id,
                "status": "failed",
                "reason": "fixture failure: report summarization exceeded the memory limit",
                "command": (
                    "systemd-run --user --scope --quiet -p MemoryMax=6G "
                    "-p MemorySwapMax=0 python capture.py fixture"
                ),
                "memory_limit": "MemoryMax=6G MemorySwapMax=0",
                "exit_code": None,
                "driver_result": {
                    "status": "captured",
                    "exit_code": 3,
                    "wall_seconds": 0.01,
                    "peak_rss_kib": 1,
                    "report_bytes": report_path.stat().st_size,
                    "report_sha256": file_digest(report_path),
                    "runs_path": reference(runs_path, root)["path"],
                    "runs_sha256": file_digest(runs_path),
                    "report_path": reference(report_path, root)["path"],
                },
            }
        rows.append(row)
    summary = {
        "version": 1,
        "route": "native",
        "fixed_denominator": 36,
        "limit_scale": 1,
        "timeout_seconds": 180,
        "head": "0" * 40,
        "panel": panel_reference,
        "binary": binary_ref,
        "rows": rows,
        "complete_pairs": sum(row.get("comparison_complete", False) for row in rows),
    }
    summary_path = root / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    return summary_path


def load_summary(root: Path) -> dict:
    return json.loads((Path(root) / "summary.json").read_text())


def _pdf(objects: list) -> bytes:
    """Serializes numbered objects with a classic xref table."""
    parts = [b"%PDF-1.4\n"]
    offsets = []
    for index, body in enumerate(objects, start=1):
        offsets.append(sum(len(part) for part in parts))
        parts.append(f"{index} 0 obj\n".encode() + body + b"\nendobj\n")
    xref_offset = sum(len(part) for part in parts)
    xref = [f"xref\n0 {len(objects) + 1}\n".encode(), b"0000000000 65535 f \n"]
    xref.extend(f"{offset:010d} 00000 n \n".encode() for offset in offsets)
    trailer = (
        f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\n"
        f"startxref\n{xref_offset}\n%%EOF\n"
    ).encode()
    return b"".join(parts + xref + [trailer])


def write_text_pdf(path: Path, text: str) -> None:
    content = f"BT /F1 12 Tf 20 50 Td ({text}) Tj ET".encode()
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
    ]
    Path(path).write_bytes(_pdf(objects))


def write_image_pdf(path: Path, rgb: tuple) -> None:
    pixels = bytes(rgb)
    content = b"q 72 0 0 72 0 0 cm /I Do Q"
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 72 72] "
        b"/Resources << /XObject << /I 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /XObject /Subtype /Image /Width 1 /Height 1 "
        b"/BitsPerComponent 8 /ColorSpace /DeviceRGB /Length 3 >>\nstream\n"
        + pixels
        + b"\nendstream",
        b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
    ]
    Path(path).write_bytes(_pdf(objects))


def write_broken_flate_pdf(path: Path) -> None:
    payload = zlib.compress(b"BT /F1 12 Tf 20 50 Td (broken) Tj ET")
    truncated = payload[: len(payload) // 2]
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
        b"/Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>",
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        b"<< /Length %d /Filter /FlateDecode >>\nstream\n" % len(truncated)
        + truncated
        + b"\nendstream",
    ]
    Path(path).write_bytes(_pdf(objects))


def overlay_real_report(root: Path, pair_id: str, source_report: Path, exit_code: int) -> None:
    """Replaces one fixture pair with a report produced by the real CLI."""
    directory = Path(root) / pair_id
    destination = directory / f"{pair_id}-native.json"
    shutil.copy2(source_report, destination)
    runs_path = directory / "runs.json"
    runs = json.loads(runs_path.read_text())
    runs["runs"][0].update(
        {
            "exit_code": exit_code,
            "report_bytes": destination.stat().st_size,
            "report_sha256": file_digest(destination),
        }
    )
    runs_path.write_text(json.dumps(runs, indent=2) + "\n")
    summary_path = Path(root) / "summary.json"
    summary = json.loads(summary_path.read_text())
    row = next(row for row in summary["rows"] if row["pair"] == pair_id)
    fields = capture_module.read_native_report(destination)
    row.update(
        {
            "status": "captured",
            "driver_exit_code": 0,
            "runs": reference(runs_path, root),
            "report": reference(destination, root),
            "schema_version": fields["schema_version"],
        }
    )
    row.update(capture_module.summarize_native(fields))
    summary["complete_pairs"] = sum(
        row.get("comparison_complete", False) for row in summary["rows"]
    )
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
