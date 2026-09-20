#!/usr/bin/env python3
"""Audits the corrected native-text-scope captures with strict binding.

Verifies both captures against the frozen panel: the exact 36 panel IDs,
per-pair runs records and their hashes, old/new input hashes, route, exit
codes, the copied binary hash, report bytes and hashes, and the native
comparison predicate recomputed with a bounded streaming reader. Merged
baseline sources are re-verified from their own summaries, and every captured
row must come from a verified source. Failed rows must record their reason,
command, memory limit and exit code and stay in the denominator. Negative
controls drive the production reader, summarizer and collector with one
injected defect each. Raw gate logs are stored under `logs/`.

Diagnostic only: it certifies no text inventory and no strict completion.
"""

import argparse
from collections import Counter
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[4]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import capture as capture_module  # noqa: E402
import native_scope_fixtures as fixtures  # noqa: E402

PANEL = ROOT / "benchmark/realworld/followup/panel.json"
PANEL_SHA256 = "c3aa4a5dd7edb3b7b4b5144ea31a9eb48645fa5ebe5a27546f3be09b0dd6e744"
REPORT = ROOT / "benchmark/realworld/remaining/completion-investigation/native-text-scope.md"
SMALL_GATES = {
    "fmt": (["cargo", "fmt", "--all", "--", "--check"], {}),
    "diff-check": (["git", "diff", "--check"], {}),
}
REUSED_GATES = {
    "cli-scope-tests": ["cargo", "test", "-p", "pdfdelta-cli", "--test", "comparison_cli"],
    "acceptance-verify": [
        "cargo",
        "run",
        "--release",
        "-p",
        "pdfdelta-bench",
        "--bin",
        "pdfbench",
        "--",
        "verify",
    ],
}


_DIGEST_CACHE = {}


def digest(path: Path) -> str:
    """Streams the hash so multi-gigabyte reports stay memory-bounded."""
    path = Path(path)
    key = (str(path), path.stat().st_size, path.stat().st_mtime_ns)
    cached = _DIGEST_CACHE.get(key)
    if cached is not None:
        return cached
    with path.open("rb") as stream:
        value = hashlib.file_digest(stream, "sha256").hexdigest()
    _DIGEST_CACHE[key] = value
    return value


def reference(path: Path, base: Path = ROOT) -> dict:
    resolved = Path(path).resolve()
    try:
        recorded = str(resolved.relative_to(Path(base).resolve()))
    except ValueError:
        recorded = str(resolved)
    return {
        "path": recorded,
        "sha256": digest(resolved),
        "bytes": resolved.stat().st_size,
    }


def load(path: Path) -> dict:
    return json.loads(Path(path).read_text())


def is_hex(value, length: int) -> bool:
    return isinstance(value, str) and len(value) == length and all(
        character in "0123456789abcdef" for character in value
    )


def load_panel() -> dict:
    if digest(PANEL) != PANEL_SHA256:
        raise ValueError("frozen panel hash mismatch")
    panel = load(PANEL)
    pairs = panel.get("pairs")
    if not isinstance(pairs, list) or len(pairs) != 36:
        raise ValueError("frozen panel must hold 36 pairs")
    if len({pair.get("id") for pair in pairs}) != 36:
        raise ValueError("frozen panel pair IDs are not unique")
    return panel


def require_settings(summary: dict, label: str) -> None:
    if summary.get("version") != 1:
        raise ValueError(f"{label}: capture record version is not 1")
    if summary.get("route") != "native":
        raise ValueError(f"{label}: route is not native")
    if summary.get("fixed_denominator") != 36:
        raise ValueError(f"{label}: denominator changed")
    if summary.get("limit_scale") != 1:
        raise ValueError(f"{label}: limit scale changed")
    if summary.get("timeout_seconds") != 180:
        raise ValueError(f"{label}: timeout changed")
    if summary.get("panel", {}).get("sha256") != PANEL_SHA256:
        raise ValueError(f"{label}: panel hash mismatch")
    if not is_hex(summary.get("head"), 40):
        raise ValueError(f"{label}: recorded head commit is invalid")


def verify_binary(summary: dict, label: str, base: Path) -> Path:
    binary = summary.get("binary")
    if not isinstance(binary, dict):
        raise ValueError(f"{label}: binary record is missing")
    path = Path(base) / binary["path"]
    if not path.is_file():
        raise ValueError(f"{label}: recorded binary is missing")
    if digest(path) != binary.get("sha256"):
        raise ValueError(f"{label}: binary hash mismatch")
    return path


def verify_optional_reference(value, label: str, base: Path) -> dict:
    """Verifies a recorded reference when its file still exists."""
    if value is None:
        return {"recorded": None, "matches_current": None}
    if not isinstance(value, dict):
        raise ValueError(f"{label}: reference record is malformed")
    path = Path(base) / value["path"]
    if not path.is_file():
        return {"recorded": value, "matches_current": None}
    matches = digest(path) == value.get("sha256")
    return {"recorded": value, "matches_current": matches}


def verify_captured_row(
    row: dict, pair: dict, base: Path, binary_sha: str, label: str
) -> dict:
    if type(row.get("driver_exit_code")) is not int:
        raise ValueError(f"{label}: {pair['id']} driver exit code is not an integer")
    runs_reference = row.get("runs")
    if not isinstance(runs_reference, dict):
        raise ValueError(f"{label}: {pair['id']} lacks a runs record")
    runs_path = Path(base) / runs_reference["path"]
    if digest(runs_path) != runs_reference.get("sha256"):
        raise ValueError(f"{label}: {pair['id']} runs hash mismatch")
    runs = load(runs_path)
    if runs.get("version") != 1:
        raise ValueError(f"{label}: {pair['id']} driver record version is not 1")
    if runs.get("binary_sha256") != binary_sha:
        raise ValueError(f"{label}: {pair['id']} ran a different binary")
    if runs.get("inputs_sha256") != pair.get("historical_inputs_sha256"):
        raise ValueError(f"{label}: {pair['id']} input manifest hash mismatch")
    if runs.get("limit_scale") != 1 or runs.get("timeout_seconds") != 180:
        raise ValueError(f"{label}: {pair['id']} limits changed")
    entries = runs.get("runs")
    if not isinstance(entries, list) or len(entries) != 1:
        raise ValueError(f"{label}: {pair['id']} must record exactly one run")
    run = entries[0]
    if run.get("pair") != pair["id"]:
        raise ValueError(f"{label}: {pair['id']} runs record names another pair")
    if run.get("route") != "native":
        raise ValueError(f"{label}: {pair['id']} run route is not native")
    if run.get("status") != "captured":
        raise ValueError(f"{label}: {pair['id']} run status is {run.get('status')}")
    if run.get("old_sha256") != pair["old"]["sha256"]:
        raise ValueError(f"{label}: {pair['id']} old input hash mismatch")
    if run.get("new_sha256") != pair["new"]["sha256"]:
        raise ValueError(f"{label}: {pair['id']} new input hash mismatch")
    if not is_hex(run.get("reference_sha256"), 64):
        raise ValueError(f"{label}: {pair['id']} reference hash is malformed")
    exit_code = run.get("exit_code")
    if type(exit_code) is not int or exit_code not in (0, 1, 3):
        raise ValueError(f"{label}: {pair['id']} exit code {exit_code} is invalid")
    report = row.get("report")
    if not isinstance(report, dict):
        raise ValueError(f"{label}: {pair['id']} lacks a report record")
    report_path = Path(base) / report["path"]
    if report_path.name != f"{pair['id']}-native.json":
        raise ValueError(f"{label}: {pair['id']} report name is unexpected")
    if report_path.stat().st_size != run.get("report_bytes"):
        raise ValueError(f"{label}: {pair['id']} report bytes mismatch")
    if digest(report_path) != run.get("report_sha256"):
        raise ValueError(f"{label}: {pair['id']} report hash disagrees with its run")
    if digest(report_path) != report.get("sha256"):
        raise ValueError(f"{label}: {pair['id']} report record disagrees with its run")
    if "bytes" in report and report["bytes"] != run.get("report_bytes"):
        raise ValueError(f"{label}: {pair['id']} report record disagrees with its run")
    fields = capture_module.read_native_report(report_path)
    if row.get("schema_version") != fields["schema_version"]:
        raise ValueError(f"{label}: {pair['id']} row schema disagrees with its report")
    native = capture_module.summarize_native(fields)
    for key in (
        "comparison_complete",
        "difference_status",
        "content_changes",
        "unresolved_regions",
        "tentative_candidates",
    ):
        if row.get(key) != native[key]:
            raise ValueError(f"{label}: {pair['id']} row {key} disagrees with its report")
    if native["comparison_complete"] != (exit_code in (0, 1)):
        raise ValueError(f"{label}: {pair['id']} completion disagrees with its exit code")
    return native


def verify_failed_row(row: dict, base: Path, label: str) -> None:
    if not isinstance(row.get("reason"), str) or not row["reason"]:
        raise ValueError(f"{label}: failed row {row.get('pair')} lacks a reason")
    if not isinstance(row.get("command"), str) or not row["command"]:
        raise ValueError(f"{label}: failed row {row.get('pair')} lacks a command")
    memory_limit = row.get("memory_limit")
    if not isinstance(memory_limit, str) or "MemoryMax=" not in memory_limit or (
        "MemorySwapMax=" not in memory_limit
    ):
        raise ValueError(f"{label}: failed row {row.get('pair')} lacks its memory limit")
    if row.get("exit_code") is not None and type(row["exit_code"]) is not int:
        raise ValueError(f"{label}: failed row {row.get('pair')} exit code is malformed")
    driver = row.get("driver_result")
    if not isinstance(driver, dict):
        raise ValueError(f"{label}: failed row {row.get('pair')} lacks driver evidence")
    if driver.get("status") != "captured":
        raise ValueError(f"{label}: failed row {row.get('pair')} driver status changed")
    if type(driver.get("exit_code")) is not int or driver["exit_code"] not in (0, 1, 3):
        raise ValueError(f"{label}: failed row {row.get('pair')} driver exit is invalid")
    if type(driver.get("report_bytes")) is not int or driver["report_bytes"] <= 0:
        raise ValueError(f"{label}: failed row {row.get('pair')} report size is invalid")
    if not is_hex(driver.get("report_sha256"), 64):
        raise ValueError(f"{label}: failed row {row.get('pair')} report hash is malformed")
    runs_path = driver.get("runs_path")
    if runs_path is not None:
        actual = Path(base) / runs_path
        if digest(actual) != driver.get("runs_sha256"):
            raise ValueError(f"{label}: failed row {row.get('pair')} runs hash mismatch")
    report_path = driver.get("report_path")
    if report_path is not None:
        actual = Path(base) / report_path
        if actual.stat().st_size != driver["report_bytes"]:
            raise ValueError(f"{label}: failed row {row.get('pair')} report size mismatch")
        if digest(actual) != driver["report_sha256"]:
            raise ValueError(f"{label}: failed row {row.get('pair')} report hash mismatch")


def require_rows(summary: dict, label: str, base: Path = ROOT, panel: dict = None) -> tuple:
    """Splits the 36 frozen rows into captured and explicitly failed rows.

    Binds every row to the frozen panel, the recorded binary and its own runs
    record; a failed row must record its reason, command, memory limit and exit
    code and stays in the denominator as a non-success.
    """
    panel = panel or load_panel()
    require_settings(summary, label)
    binary = summary.get("binary") or {}
    verify_binary(summary, label, base)
    pairs = {pair["id"]: pair for pair in panel["pairs"]}
    rows = summary.get("rows")
    if not isinstance(rows, list) or len(rows) != 36:
        raise ValueError(f"{label}: expected 36 rows")
    if {row.get("pair") for row in rows} != set(pairs):
        raise ValueError(f"{label}: row pairs differ from the frozen panel")
    captured = {}
    failed = {}
    natives = {}
    for row in rows:
        pair = pairs[row["pair"]]
        status = row.get("status")
        if status == "captured":
            natives[row["pair"]] = verify_captured_row(
                row, pair, base, binary.get("sha256"), label
            )
            captured[row["pair"]] = row
        elif status == "failed":
            verify_failed_row(row, base, label)
            failed[row["pair"]] = row
        else:
            raise ValueError(f"{label}: row {row['pair']} is {status}")
    complete = sum(1 for row in captured.values() if row.get("comparison_complete"))
    if summary.get("complete_pairs") != complete:
        raise ValueError(f"{label}: recorded complete_pairs disagrees")
    return captured, failed, natives


def verify_merged_sources(
    summary: dict, label: str, base: Path, captured: dict, failed: dict
) -> list:
    """Re-verifies every merged source from its own summary and rows."""
    sources = summary.get("merged_partial_captures")
    if sources is None:
        return []
    if not isinstance(sources, list) or not sources:
        raise ValueError(f"{label}: merged capture list is empty")
    panel = load_panel()
    verified = []
    seen = {}
    for entry in sources:
        if not isinstance(entry, dict):
            raise ValueError(f"{label}: merged source record is malformed")
        directory = entry.get("directory")
        if not isinstance(directory, str) or not directory:
            raise ValueError(f"{label}: merged source directory is missing")
        condition = entry.get("memory_condition")
        if not isinstance(condition, str) or not condition:
            raise ValueError(f"{label}: merged source memory condition is missing")
        source_dir = Path(base) / directory
        if not source_dir.is_dir():
            raise ValueError(f"{label}: merged source {directory} is missing")
        source_summary = entry.get("summary")
        if source_summary is None:
            if entry.get("captured_rows") != 0:
                raise ValueError(f"{label}: summary-less source claims captured rows")
            backs_failure = any(
                isinstance(row.get("driver_result"), dict)
                and isinstance(row["driver_result"].get("runs_path"), str)
                and str((Path(base) / row["driver_result"]["runs_path"]).resolve()).startswith(
                    str(source_dir.resolve())
                )
                for row in failed.values()
            )
            if not backs_failure:
                raise ValueError(f"{label}: summary-less source backs no failure")
            verified.append(
                {
                    "directory": directory,
                    "memory_condition": condition,
                    "captured_rows": 0,
                    "summary": None,
                }
            )
            continue
        source_summary_path = Path(base) / source_summary["path"]
        if digest(source_summary_path) != source_summary.get("sha256"):
            raise ValueError(f"{label}: merged source summary hash mismatch")
        source = load(source_summary_path)
        require_settings(source, f"{label}:{directory}")
        if source.get("binary", {}).get("sha256") != summary["binary"]["sha256"]:
            raise ValueError(f"{label}: merged source ran a different binary")
        source_rows = source.get("rows")
        if not isinstance(source_rows, list):
            raise ValueError(f"{label}: merged source rows are missing")
        source_captured = 0
        for row in source_rows:
            status = row.get("status")
            if status == "captured":
                source_captured += 1
                pair = row["pair"]
                if pair in seen:
                    raise ValueError(f"{label}: pair {pair} appears in two merged sources")
                seen[pair] = directory
                merged = captured.get(pair)
                if merged is None:
                    raise ValueError(f"{label}: merged row {pair} is missing")
                if merged["runs"]["sha256"] != row["runs"]["sha256"]:
                    raise ValueError(f"{label}: merged row {pair} runs hash changed")
                if merged["report"]["sha256"] != row["report"]["sha256"]:
                    raise ValueError(f"{label}: merged row {pair} report hash changed")
            elif status == "failed":
                verify_failed_row(row, base, f"{label}:{directory}")
            else:
                raise ValueError(f"{label}: merged source row {row.get('pair')} is {status}")
        if entry.get("captured_rows") != source_captured:
            raise ValueError(f"{label}: merged source row count changed")
        verified.append(
            {
                "directory": directory,
                "memory_condition": condition,
                "captured_rows": source_captured,
                "summary": reference(source_summary_path, base),
            }
        )
    if set(seen) != set(captured):
        raise ValueError(f"{label}: merged sources do not cover every captured row")
    for entry in summary.get("memory_conditions") or []:
        if not isinstance(entry, dict):
            raise ValueError(f"{label}: memory condition record is malformed")
        directory = entry.get("directory")
        if not isinstance(directory, str) or not (Path(base) / directory).is_dir():
            raise ValueError(f"{label}: memory condition directory is missing")
        if not isinstance(entry.get("memory_condition"), str) or not entry["memory_condition"]:
            raise ValueError(f"{label}: memory condition text is missing")
        recorded = entry.get("summary")
        if recorded is None:
            if entry.get("captured_rows") != 0:
                raise ValueError(f"{label}: summary-less condition claims captured rows")
            continue
        path = Path(base) / recorded["path"]
        if digest(path) != recorded.get("sha256"):
            raise ValueError(f"{label}: memory condition summary hash mismatch")
        source = load(path)
        require_settings(source, f"{label}:{directory}")
        if source.get("binary", {}).get("sha256") != summary["binary"]["sha256"]:
            raise ValueError(f"{label}: memory condition ran a different binary")
    return verified


def audit_capture(directory: Path, label: str, base: Path = ROOT, panel: dict = None) -> dict:
    panel = panel or load_panel()
    summary = load(Path(directory) / "summary.json")
    captured, failed, natives = require_rows(summary, label, base=base, panel=panel)
    merged = verify_merged_sources(summary, label, base, captured, failed)
    complete = 0
    vacuous_complete = 0
    incomplete = 0
    extraction_incomplete = 0
    unresolved_regions = 0
    tentative_candidates = 0
    coverage_ratios = []
    coverage_unavailable = 0
    statuses = Counter()
    causes = Counter()
    pair_rows = []
    for pair, native in sorted(natives.items()):
        statuses[native["difference_status"]] += 1
        unresolved_regions += native["unresolved_regions"]
        tentative_candidates += native["tentative_candidates"]
        for coverage in (native["old_alignment_coverage"], native["new_alignment_coverage"]):
            if not coverage.get("total_tokens"):
                continue
            if coverage.get("ratio") is None:
                coverage_unavailable += 1
            else:
                coverage_ratios.append(coverage["ratio"])
        if not (native["old_extraction_complete"] and native["new_extraction_complete"]):
            extraction_incomplete += 1
        if native["comparison_complete"]:
            if native["vacuous"]:
                vacuous_complete += 1
            else:
                complete += 1
        else:
            incomplete += 1
        for reason, count in native["unresolved_reasons"].items():
            if reason:
                causes[reason] += count
        pair_rows.append(
            {
                "pair": pair,
                "comparison_complete": native["comparison_complete"],
                "vacuous": native["vacuous"],
                "difference_status": native["difference_status"],
                "content_changes": native["content_changes"],
                "unresolved_regions": native["unresolved_regions"],
                "tentative_candidates": native["tentative_candidates"],
                "old_coverage": native["old_alignment_coverage"].get("ratio"),
                "new_coverage": native["new_alignment_coverage"].get("ratio"),
                "old_extraction_complete": native["old_extraction_complete"],
                "new_extraction_complete": native["new_extraction_complete"],
                "assessment_null": native["assessment_null"],
                "unresolved_reasons": native["unresolved_reasons"],
            }
        )
    if complete + vacuous_complete + incomplete + len(failed) != 36:
        raise ValueError(f"{label}: completion classes do not sum to 36")
    return {
        "capture": reference(Path(directory) / "summary.json"),
        "binary": summary["binary"],
        "head": summary.get("head"),
        "merged_sources": merged,
        "complete_non_vacuous": complete,
        "complete_vacuous": vacuous_complete,
        "incomplete": incomplete,
        "failed": len(failed),
        "failed_pairs": [
            {
                "pair": pair,
                "reason": row["reason"],
                "memory_limit": row["memory_limit"],
                "exit_code": row["exit_code"],
                "driver_result": row.get("driver_result"),
            }
            for pair, row in sorted(failed.items())
        ],
        "extraction_incomplete": extraction_incomplete,
        "difference_status": dict(statuses),
        "unresolved_regions": unresolved_regions,
        "tentative_candidates": tentative_candidates,
        "coverage_ratio_unavailable_sides": coverage_unavailable,
        "coverage_ratio_min": min(coverage_ratios) if coverage_ratios else None,
        "coverage_ratio_mean": (
            round(sum(coverage_ratios) / len(coverage_ratios), 6) if coverage_ratios else None
        ),
        "top_unresolved_reasons": causes.most_common(8),
        "rows": pair_rows,
    }


def run_gates(output: Path, names: dict) -> dict:
    output.mkdir(parents=True, exist_ok=True)
    exits = {}
    for name, (command, environment) in names.items():
        started = time.monotonic()
        completed = subprocess.run(
            command,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            env={**os.environ, **environment},
        )
        stdout_path = output / f"{name}.stdout.log"
        stderr_path = output / f"{name}.stderr.log"
        stdout_path.write_text(completed.stdout)
        stderr_path.write_text(completed.stderr)
        exits[name] = {
            "command": command,
            "environment": environment,
            "returncode": completed.returncode,
            "seconds": round(time.monotonic() - started, 3),
            "stdout": reference(stdout_path),
            "stderr": reference(stderr_path),
        }
        print(name, completed.returncode, flush=True)
    (output / "exits.json").write_text(json.dumps(exits, indent=2) + "\n")
    return exits


def verify_gates(output: Path, names: dict) -> dict:
    exits = load(output / "exits.json")
    if set(exits) != set(names):
        raise ValueError("gate set differs from the expected commands")
    for name, (command, environment) in names.items():
        entry = exits[name]
        if entry.get("command") != command or entry.get("environment") != environment:
            raise ValueError(f"gate {name}: command or environment was altered")
        if entry.get("returncode") != 0:
            raise ValueError(f"gate {name}: return code {entry.get('returncode')}")
        for stream in ("stdout", "stderr"):
            recorded = entry.get(stream)
            if digest(ROOT / recorded["path"]) != recorded["sha256"]:
                raise ValueError(f"gate {name}: {stream} log hash mismatch")
    return exits


def verify_reused_gates(cache: Path) -> dict:
    """Binds the reused heavy gates to their recorded v1 logs and hashes."""
    exits = load(Path(cache) / "logs/gates-raw/exits.json")
    reused = {}
    for name, command in REUSED_GATES.items():
        entry = exits.get(name)
        if not isinstance(entry, dict):
            raise ValueError(f"reused gate {name}: record is missing")
        if entry.get("command") != command:
            raise ValueError(f"reused gate {name}: command changed")
        if entry.get("returncode") != 0:
            raise ValueError(f"reused gate {name}: return code is not zero")
        for stream in ("stdout", "stderr"):
            recorded = entry.get(stream)
            if digest(ROOT / recorded["path"]) != recorded["sha256"]:
                raise ValueError(f"reused gate {name}: {stream} log hash mismatch")
        reused[name] = {
            "source": str(Path(cache) / "logs/gates-raw/exits.json"),
            "command": command,
            "returncode": entry["returncode"],
            "stdout": entry["stdout"],
            "stderr": entry["stderr"],
        }
    return reused


def _expect_reject(name: str, action) -> dict:
    try:
        action()
    except ValueError as error:
        return {"control": name, "rejected": True, "error": str(error)}
    return {"control": name, "rejected": False, "error": None}


def _expect_pass(name: str, action) -> dict:
    try:
        value = action()
    except ValueError as error:
        return {"control": name, "passed": False, "error": str(error)}
    return {"control": name, "passed": True, "value": value}


def self_test() -> list:
    """Runs positive and negative controls through the production paths."""
    panel = load_panel()
    panel_reference = reference(PANEL)
    pairs = panel["pairs"]
    results = []
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        def build(name: str, **kwargs) -> Path:
            root = tmp / name
            fixtures.build_capture(root, pairs, panel_reference, **kwargs)
            return root

        def mutate_report(root: Path, pair_id: str, mutate) -> Path:
            path = root / pair_id / f"{pair_id}-native.json"
            report = json.loads(path.read_text())
            mutate(report)
            fixtures.write_report(path, report)
            return path

        def audit(root: Path) -> dict:
            return audit_capture(root, "control", base=root, panel=panel)

        def require(root: Path):
            return require_rows(fixtures.load_summary(root), "control", base=root, panel=panel)

        valid = build("valid")
        results.append(
            _expect_pass(
                "valid_rows",
                lambda: len(require(valid)[0]) == 36 and not require(valid)[1],
            )
        )
        results.append(
            _expect_pass(
                "valid_audit",
                lambda: audit(valid)["complete_non_vacuous"] == 36
                and audit(valid)["failed"] == 0,
            )
        )

        vacuous = build("vacuous", vacuous_pairs=(pairs[0]["id"],))
        results.append(
            _expect_pass(
                "vacuous_classified",
                lambda: audit(vacuous)["complete_vacuous"] == 1
                and audit(vacuous)["complete_non_vacuous"] == 35,
            )
        )

        failed = build("failed", with_failed_pair=True)
        results.append(
            _expect_pass(
                "failure_retained",
                lambda: audit(failed)["failed"] == 1
                and audit(failed)["complete_non_vacuous"] == 35
                and audit(failed)["failed_pairs"][0]["memory_limit"]
                == "MemoryMax=6G MemorySwapMax=0",
            )
        )
        results.append(
            _expect_pass(
                "escaped_issue_description",
                lambda: capture_module.summarize_native(
                    capture_module.read_native_report(
                        failed / pairs[-1]["id"] / f"{pairs[-1]['id']}-native.json"
                    )
                )["unresolved_reasons"]
                == {"unsupported": 1},
            )
        )
        null_report = build("assessment_null")
        null_path = mutate_report(null_report, pairs[0]["id"], lambda report: report.update({"assessment": None}))
        results.append(
            _expect_pass(
                "assessment_null",
                lambda: capture_module.summarize_native(
                    capture_module.read_native_report(null_path)
                )["assessment_null"]
                is True,
            )
        )

        def reader_control(name: str, mutate, raw=None) -> None:
            root = build(f"reader_{name}")
            path = root / pairs[0]["id"] / f"{pairs[0]['id']}-native.json"
            if raw is not None:
                path.write_bytes(raw(path.read_bytes()))
            else:
                mutate_report(root, pairs[0]["id"], mutate)
            results.append(
                _expect_reject(
                    name,
                    lambda: capture_module.summarize_native(
                        capture_module.read_native_report(path)
                    ),
                )
            )

        reader_control(
            "missing_summary_field",
            lambda report: report["summary"].pop("comparison_complete"),
        )
        reader_control(
            "string_boolean",
            lambda report: report["extraction"].update({"old_complete": "true"}),
        )
        reader_control(
            "string_count",
            lambda report: report["summary"].update({"unresolved_regions": "0"}),
        )
        reader_control(
            "coverage_missing_counts",
            lambda report: report["summary"].update(
                {"old_alignment_coverage": {"ratio": 1.0}}
            ),
        )
        reader_control(
            "coverage_ratio_mismatch",
            lambda report: report["summary"]["old_alignment_coverage"].update(
                {"ratio": 0.5}
            ),
        )
        reader_control(
            "scope_images",
            lambda report: report["comparison_scope"].update({"images_compared": True}),
        )
        reader_control(
            "scope_missing_text",
            lambda report: report["comparison_scope"].pop("supported_text"),
        )
        reader_control(
            "false_complete",
            lambda report: report["summary"].update(
                {"comparison_complete": True, "unresolved_regions": 1}
            ),
        )
        reader_control(
            "indeterminate_complete",
            lambda report: (
                report.update({"difference_status": "indeterminate"}),
                report["summary"].update({"difference_status": "indeterminate"}),
            ),
        )
        reader_control("assessment_missing", lambda report: report.pop("assessment"))
        reader_control(
            "truncated_tail", None, raw=lambda data: data[: len(data) // 2]
        )
        reader_control("trailing_garbage", None, raw=lambda data: data + b"\n{}\n")
        reader_control(
            "compact_format",
            None,
            raw=lambda data: json.dumps(json.loads(data)).encode(),
        )
        reader_control(
            "missing_comma",
            None,
            raw=lambda data: data.replace(b"11,\n", b"11\n", 1),
        )
        reader_control(
            "trailing_comma",
            None,
            raw=lambda data: (
                lambda index: data[:index] + b"," + data[index:]
            )(data.rfind(b"\n}")),
        )
        reader_control(
            "duplicate_key",
            None,
            raw=lambda data: data.replace(
                b'  "changes": [],\n', b'  "summary": {},\n  "changes": [],\n', 1
            ),
        )

        def collector_control(name: str, mutate) -> None:
            root = build(f"collector_{name}")
            summary = fixtures.load_summary(root)
            mutate(root, summary)
            results.append(
                _expect_reject(
                    name,
                    lambda: require_rows(summary, "control", base=root, panel=panel),
                )
            )

        collector_control(
            "wrong_pair",
            lambda root, summary: summary["rows"][0].update({"pair": "unknown-pair"}),
        )
        collector_control(
            "missing_row",
            lambda root, summary: summary["rows"].pop(),
        )
        collector_control(
            "wrong_route",
            lambda root, summary: (
                (root / summary["rows"][0]["pair"] / "runs.json").write_text(
                    json.dumps(
                        {
                            **json.loads(
                                (root / summary["rows"][0]["pair"] / "runs.json").read_text()
                            ),
                            "runs": [
                                {
                                    **json.loads(
                                        (
                                            root
                                            / summary["rows"][0]["pair"]
                                            / "runs.json"
                                        ).read_text()
                                    )["runs"][0],
                                    "route": "text",
                                }
                            ],
                        },
                        indent=2,
                    )
                    + "\n"
                ),
                summary["rows"][0]["runs"].update(
                    {"sha256": digest(root / summary["rows"][0]["pair"] / "runs.json")}
                ),
            ),
        )
        collector_control(
            "wrong_schema",
            lambda root, summary: (
                mutate_report(
                    root,
                    summary["rows"][0]["pair"],
                    lambda report: report.update({"schema_version": 2}),
                ),
                summary["rows"][0].update(
                    {
                        "schema_version": 2,
                        "report": {
                            **summary["rows"][0]["report"],
                            "sha256": digest(
                                root
                                / summary["rows"][0]["pair"]
                                / f"{summary['rows'][0]['pair']}-native.json"
                            ),
                            "bytes": (
                                root
                                / summary["rows"][0]["pair"]
                                / f"{summary['rows'][0]['pair']}-native.json"
                            ).stat().st_size,
                        },
                    }
                ),
                (
                    root / summary["rows"][0]["pair"] / "runs.json"
                ).write_text(
                    json.dumps(
                        {
                            **json.loads(
                                (root / summary["rows"][0]["pair"] / "runs.json").read_text()
                            ),
                            "runs": [
                                {
                                    **json.loads(
                                        (
                                            root
                                            / summary["rows"][0]["pair"]
                                            / "runs.json"
                                        ).read_text()
                                    )["runs"][0],
                                    "report_sha256": digest(
                                        root
                                        / summary["rows"][0]["pair"]
                                        / f"{summary['rows'][0]['pair']}-native.json"
                                    ),
                                    "report_bytes": (
                                        root
                                        / summary["rows"][0]["pair"]
                                        / f"{summary['rows'][0]['pair']}-native.json"
                                    ).stat().st_size,
                                }
                            ],
                        },
                        indent=2,
                    )
                    + "\n"
                ),
                summary["rows"][0]["runs"].update(
                    {"sha256": digest(root / summary["rows"][0]["pair"] / "runs.json")}
                ),
            ),
        )
        collector_control(
            "wrong_report_hash",
            lambda root, summary: (
                root
                / summary["rows"][0]["pair"]
                / f"{summary['rows'][0]['pair']}-native.json"
            ).write_bytes(
                (
                    root
                    / summary["rows"][0]["pair"]
                    / f"{summary['rows'][0]['pair']}-native.json"
                ).read_bytes()
                + b"\n"
            ),
        )
        collector_control(
            "wrong_input_hash",
            lambda root, summary: (
                (root / summary["rows"][0]["pair"] / "runs.json").write_text(
                    json.dumps(
                        {
                            **json.loads(
                                (root / summary["rows"][0]["pair"] / "runs.json").read_text()
                            ),
                            "runs": [
                                {
                                    **json.loads(
                                        (
                                            root
                                            / summary["rows"][0]["pair"]
                                            / "runs.json"
                                        ).read_text()
                                    )["runs"][0],
                                    "old_sha256": "2" * 64,
                                }
                            ],
                        },
                        indent=2,
                    )
                    + "\n"
                ),
                summary["rows"][0]["runs"].update(
                    {"sha256": digest(root / summary["rows"][0]["pair"] / "runs.json")}
                ),
            ),
        )
        collector_control(
            "wrong_exit",
            lambda root, summary: (
                (root / summary["rows"][0]["pair"] / "runs.json").write_text(
                    json.dumps(
                        {
                            **json.loads(
                                (root / summary["rows"][0]["pair"] / "runs.json").read_text()
                            ),
                            "runs": [
                                {
                                    **json.loads(
                                        (
                                            root
                                            / summary["rows"][0]["pair"]
                                            / "runs.json"
                                        ).read_text()
                                    )["runs"][0],
                                    "exit_code": 3,
                                }
                            ],
                        },
                        indent=2,
                    )
                    + "\n"
                ),
                summary["rows"][0]["runs"].update(
                    {"sha256": digest(root / summary["rows"][0]["pair"] / "runs.json")}
                ),
            ),
        )
        collector_control(
            "wrong_binary",
            lambda root, summary: summary["binary"].update({"sha256": "3" * 64}),
        )
        collector_control(
            "unrecorded_failure",
            lambda root, summary: (
                fixtures.build_capture(root, pairs, panel_reference, with_failed_pair=True),
                summary.clear(),
                summary.update(fixtures.load_summary(root)),
                summary["rows"][-1].pop("memory_limit"),
            ),
        )
    return results


def real_report_check(cache: Path, binary: Path) -> int:
    """Feeds real CLI reports through the production reader and collector.

    Four tiny inputs exercise the real serde-pretty format end to end:
    identical text, an exact replacement, an image-only vacuous pair, and a
    corrupt Flate stream. The reports are also overlaid into a 36-row fixture
    capture so the collector verifies them with the same binding as the panel.
    """
    directory = Path(cache) / "real-reports"
    inputs = directory / "inputs"
    reports = directory / "reports"
    inputs.mkdir(parents=True, exist_ok=True)
    reports.mkdir(parents=True, exist_ok=True)
    fixtures.write_text_pdf(inputs / "text-old.pdf", "10 days")
    fixtures.write_text_pdf(inputs / "text-new.pdf", "20 days")
    fixtures.write_image_pdf(inputs / "image-old.pdf", (255, 0, 0))
    fixtures.write_image_pdf(inputs / "image-new.pdf", (0, 0, 255))
    fixtures.write_broken_flate_pdf(inputs / "broken.pdf")
    scenarios = [
        (
            "identical",
            inputs / "text-old.pdf",
            inputs / "text-old.pdf",
            {"comparison_complete": True, "difference_status": "no_content_change",
             "content_changes": 0, "vacuous": False, "exit": 0},
        ),
        (
            "replacement",
            inputs / "text-old.pdf",
            inputs / "text-new.pdf",
            {"comparison_complete": True, "difference_status": "detected",
             "content_changes": 1, "vacuous": False, "exit": 1},
        ),
        (
            "image_only",
            inputs / "image-old.pdf",
            inputs / "image-new.pdf",
            {"comparison_complete": True, "vacuous": True,
             "content_changes": 0, "exit": 0},
        ),
        (
            "broken_stream",
            inputs / "broken.pdf",
            inputs / "broken.pdf",
            {"comparison_complete": False, "exit": 3},
        ),
    ]
    results = []
    passed = True
    for name, old, new, expected in scenarios:
        report = reports / f"{name}.json"
        command = [
            str(binary), str(old), str(new),
            "--native-text-only", "--json", str(report), "--quiet",
        ]
        completed = subprocess.run(
            command, cwd=ROOT, capture_output=True, text=True, check=False
        )
        if not report.is_file():
            results.append(
                {"scenario": name, "passed": False, "command": command,
                 "exit": completed.returncode, "stderr": completed.stderr[-400:],
                 "error": "the CLI wrote no report"}
            )
            passed = False
            continue
        fields = capture_module.read_native_report(report)
        native = capture_module.summarize_native(fields)
        checks = {"exit": completed.returncode == expected["exit"]}
        for key in ("comparison_complete", "difference_status", "content_changes", "vacuous"):
            if key in expected:
                checks[key] = native[key] == expected[key]
        scenario_passed = all(checks.values())
        passed &= scenario_passed
        results.append(
            {
                "scenario": name,
                "passed": scenario_passed,
                "checks": checks,
                "exit": completed.returncode,
                "command": command,
                "report": reference(report),
                "classification": {
                    key: native[key]
                    for key in (
                        "comparison_complete",
                        "difference_status",
                        "content_changes",
                        "vacuous",
                        "old_extraction_complete",
                        "new_extraction_complete",
                    )
                },
            }
        )
    capture_directory = directory / "capture"
    fixtures.build_capture(capture_directory, load_panel()["pairs"], reference(PANEL))
    for index, (name, _, _, expected) in enumerate(scenarios):
        fixtures.overlay_real_report(
            capture_directory, load_panel()["pairs"][index]["id"],
            reports / f"{name}.json", expected["exit"],
        )
    try:
        audit = audit_capture(
            capture_directory, "real-report-capture",
            base=capture_directory, panel=load_panel(),
        )
        collector = {
            "passed": True,
            "complete_non_vacuous": audit["complete_non_vacuous"],
            "complete_vacuous": audit["complete_vacuous"],
            "incomplete": audit["incomplete"],
            "failed": audit["failed"],
        }
    except ValueError as error:
        collector = {"passed": False, "error": str(error)}
        passed = False
    record = {
        "binary": reference(binary),
        "scenarios": results,
        "collector": collector,
    }
    (directory / "results.json").write_text(json.dumps(record, indent=2) + "\n")
    print(json.dumps(record, indent=1))
    return 0 if passed else 1


def merge_capture(source: Path, output: Path) -> None:
    """Builds the v2 audit records from verified existing raw captures."""
    panel = load_panel()
    output = Path(output)
    output.mkdir(parents=True, exist_ok=True)
    for label in ("baseline-native", "current-native"):
        source_dir = Path(source) / label
        summary = load(source_dir / "summary.json")
        require_settings(summary, label)
        verify_binary(summary, label, ROOT)
        rows = json.loads(json.dumps(summary["rows"]))
        for row in rows:
            if row.get("status") != "failed":
                continue
            driver = row["driver_result"]
            pair_dir = source_dir / row["pair"]
            runs_path = pair_dir / "runs.json"
            report_path = pair_dir / f"{row['pair']}-native.json"
            if driver.get("runs_path") is None and runs_path.is_file():
                driver["runs_path"] = str(runs_path.resolve().relative_to(ROOT))
                driver["runs_sha256"] = digest(runs_path)
            if driver.get("report_path") is None and report_path.is_file():
                driver["report_path"] = str(report_path.resolve().relative_to(ROOT))
                driver["report_bytes"] = report_path.stat().st_size
                driver["report_sha256"] = digest(report_path)
        memory_conditions = []
        for entry in summary.get("merged_partial_captures") or [
            {"directory": label, "memory_condition": "MemoryMax=6G"}
        ]:
            name = entry["directory"]
            candidates = [source_dir / name, source_dir.parent / name]
            source_dir_path = next(
                (candidate.resolve() for candidate in candidates if candidate.is_dir()),
                (source_dir.parent / name).resolve(),
            )
            source_summary_path = source_dir_path / "summary.json"
            captured_rows = 0
            summary_reference = None
            if source_summary_path.is_file():
                source_summary = load(source_summary_path)
                require_settings(source_summary, f"{label}:{name}")
                if (
                    source_summary.get("binary", {}).get("sha256")
                    != summary["binary"]["sha256"]
                ):
                    raise ValueError(f"{label}:{name}: merged source binary differs")
                captured_rows = sum(
                    row.get("status") == "captured"
                    for row in source_summary.get("rows", [])
                )
                summary_reference = reference(source_summary_path)
            memory_conditions.append(
                {
                    "directory": str(source_dir_path.relative_to(ROOT)),
                    "memory_condition": entry["memory_condition"],
                    "captured_rows": captured_rows,
                    "summary": summary_reference,
                }
            )
        aggregate_captured = sum(row.get("status") == "captured" for row in rows)
        sources = [
            {
                "directory": str(source_dir.resolve().relative_to(ROOT)),
                "memory_condition": "mixed; see memory_conditions",
                "captured_rows": aggregate_captured,
                "summary": reference(source_dir / "summary.json"),
            }
        ]
        record = {
            "version": 1,
            "route": "native",
            "fixed_denominator": 36,
            "limit_scale": 1,
            "timeout_seconds": 180,
            "head": summary["head"],
            "panel": summary["panel"],
            "binary": summary["binary"],
            "rows": rows,
            "complete_pairs": summary["complete_pairs"],
            "merged_partial_captures": sources,
            "memory_conditions": memory_conditions,
            "source_capture": reference(source_dir / "summary.json"),
            "merge_note": (
                "Rows merged from serial captures of one binary. Captured rows were produced "
                "without a memory cap and the recorded failure ran under MemoryMax=6G, so rows "
                "do not share one memory condition; internal limits, route, panel, binary hash "
                "and schema were verified identical across sources."
            ),
        }
        (output / label).mkdir(parents=True, exist_ok=True)
        (output / label / "summary.json").write_text(
            json.dumps(record, indent=2) + "\n"
        )
        print(label, "merged", len(rows), "rows", flush=True)


def freeze_report(cache: Path) -> int:
    summary_path = cache / "summary.json"
    summary = load(summary_path)
    snapshot = cache / "tools/native-text-scope.md.snapshot"
    snapshot.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(REPORT, snapshot)
    summary["report"] = {
        **reference(snapshot),
        "live_path": str(REPORT.relative_to(ROOT)),
    }
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"report_snapshot": summary["report"]}, indent=1))
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("cache", type=Path, nargs="?")
    parser.add_argument("--output", type=Path, metavar="CACHE")
    parser.add_argument("--merge-source", type=Path, metavar="CACHE")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--run-gates", type=Path, metavar="CACHE")
    parser.add_argument("--freeze-report", type=Path, metavar="CACHE")
    parser.add_argument("--reuse-gates", type=Path, metavar="CACHE")
    parser.add_argument("--real-report-check", type=Path, metavar="CACHE")
    parser.add_argument("--binary", type=Path, metavar="PDFDELTA")
    args = parser.parse_args()
    if args.self_test:
        controls = self_test()
        print(json.dumps(controls, indent=2))
        return 0 if all(
            control.get("rejected") or control.get("passed") for control in controls
        ) else 1
    if args.real_report_check:
        binary = args.binary or (ROOT / "target/release/pdfdelta")
        return real_report_check(args.real_report_check, binary)
    if args.run_gates:
        run_gates(args.run_gates / "logs/gates-raw", SMALL_GATES)
        return 0
    if args.freeze_report:
        return freeze_report(args.freeze_report)
    if args.merge_source:
        if args.output is None:
            parser.error("--merge-source requires --output")
        merge_capture(args.merge_source, args.output)
        return 0
    if args.cache is None:
        parser.error("cache is required")
    cache = args.cache
    output = args.output or cache
    output.mkdir(parents=True, exist_ok=True)
    panel = load_panel()
    baseline = audit_capture(cache / "baseline-native", "baseline-native", panel=panel)
    current = audit_capture(cache / "current-native", "current-native", panel=panel)
    controls = self_test()
    if not all(
        control.get("rejected") or control.get("passed") for control in controls
    ):
        raise ValueError("a negative control was not rejected")
    run_gates(output / "logs/gates-raw", SMALL_GATES)
    gates = verify_gates(output / "logs/gates-raw", SMALL_GATES)
    reused = verify_reused_gates(args.reuse_gates) if args.reuse_gates else None
    summary = {
        "version": "native-text-scope-v2",
        "date": "2026-09-19",
        "purpose": (
            "re-audit of the corrected native-text-scope captures with strict panel, runs, "
            "input, binary and report binding; image pixels, path lettering and OCR stay "
            "outside the primary scope"
        ),
        "strict_status": (
            "the common document-wide 0/36 remains historical old-scope evidence; the native "
            "primary counts below are the corrected target"
        ),
        "panel": reference(PANEL),
        "baseline": baseline,
        "current": current,
        "negative_controls": controls,
        "gates": {"executed": gates, "reused": reused},
        "report": {"path": str(REPORT.relative_to(ROOT))},
        "limitations": [
            "the first-followup binary next-execution/pdfdelta-9093cab is not present in the checkout; the a3c8b08 baseline is used",
            "the current binary includes uncommitted held changes; the comparison is against the baseline binary, not against a released revision",
            "captured rows were produced without a memory cap and the recorded baseline failure ran under MemoryMax=6G, so rows do not share one memory condition",
            "the heavy CLI scope and acceptance gates were not re-run because no Rust or CLI file changed after their v1 pass; their v1 logs and hashes are referenced",
            "no production rule, predicate, panel, denominator or limit was changed",
        ],
    }
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(
        json.dumps(
            {
                "baseline": {
                    key: baseline[key]
                    for key in (
                        "complete_non_vacuous",
                        "complete_vacuous",
                        "incomplete",
                        "failed",
                    )
                },
                "current": {
                    key: current[key]
                    for key in (
                        "complete_non_vacuous",
                        "complete_vacuous",
                        "incomplete",
                        "failed",
                    )
                },
                "gates": {name: entry["returncode"] for name, entry in gates.items()},
                "reused": {name: entry["returncode"] for name, entry in (reused or {}).items()},
            },
            indent=1,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
