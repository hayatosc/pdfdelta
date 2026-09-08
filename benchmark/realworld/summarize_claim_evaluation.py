"""Compare a claim-evaluation run with the frozen real-world baseline."""

import argparse
from collections import Counter
import csv
import hashlib
import json
from pathlib import Path
from typing import Any


SCRIPT_ROOT = Path(__file__).resolve().parent
DEFAULT_MANIFEST = SCRIPT_ROOT / "manifest.tsv"
DEFAULT_BASELINE = SCRIPT_ROOT / "results/issue20-order-experiment/baseline-fixed"
REPORT_SCHEMA_VERSION = 1


class DataError(Exception):
    """Raised when the immutable manifest or baseline is not internally valid."""


def read_json(path: Path) -> tuple[Any | None, str | None]:
    if not path.is_file():
        return None, "missing"
    try:
        return json.loads(path.read_text()), None
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return None, f"invalid JSON: {error}"


def require_json(path: Path) -> Any:
    value, error = read_json(path)
    if error is not None:
        raise DataError(f"{path}: {error}")
    return value


def require_single_record(path: Path) -> dict[str, Any]:
    value = require_json(path)
    if not isinstance(value, dict) or not isinstance(value.get("records"), list):
        raise DataError(f"{path}: expected an object with a records array")
    records = value["records"]
    if len(records) != 1 or not isinstance(records[0], dict):
        raise DataError(f"{path}: expected exactly one object record")
    return records[0]


def optional_single_record(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    value, error = read_json(path)
    if error is not None:
        return None, error
    if not isinstance(value, dict) or not isinstance(value.get("records"), list):
        return None, "expected an object with a records array"
    records = value["records"]
    if len(records) != 1 or not isinstance(records[0], dict):
        return None, "expected exactly one object record"
    return records[0], None


def load_manifest(path: Path) -> list[dict[str, str]]:
    try:
        with path.open(newline="") as source:
            rows = list(csv.DictReader((line for line in source if not line.startswith("#")), delimiter="\t"))
    except (OSError, UnicodeError, csv.Error) as error:
        raise DataError(f"{path}: cannot read manifest: {error}") from error
    required = {"pair_id", "set", "role", "expected_file", "expected_extraction"}
    if not rows or not required.issubset(rows[0]):
        raise DataError(f"{path}: missing required manifest columns")
    seen: set[str] = set()
    for row in rows:
        pair = row["pair_id"]
        if not pair or pair in seen or Path(pair).name != pair or pair in (".", ".."):
            raise DataError(f"{path}: invalid or duplicate pair_id {pair!r}")
        seen.add(pair)
        expected_file = row["expected_file"]
        if expected_file != "-":
            expected_path = Path(expected_file)
            if expected_path.is_absolute() or ".." in expected_path.parts:
                raise DataError(f"{path}: expected_file escapes the manifest directory: {expected_file!r}")
    return rows


def load_expected(path: Path) -> dict[str, Any]:
    value = require_json(path)
    if not isinstance(value, dict) or not isinstance(value.get("changes"), list):
        raise DataError(f"{path}: expected an object with a changes array")
    changes = value["changes"]
    ids: set[str] = set()
    for change in changes:
        if not isinstance(change, dict) or not isinstance(change.get("id"), str):
            raise DataError(f"{path}: every change needs a string id")
        if change["id"] in ids:
            raise DataError(f"{path}: duplicate change id {change['id']!r}")
        ids.add(change["id"])
    return value


def load_outputs(directory: Path, pair: str, strict: bool) -> dict[str, Any]:
    outputs: dict[str, Any] = {"errors": []}
    paths = {
        "process": directory / f"{pair}.process.json",
        "summary": directory / f"{pair}.summary.json",
        "evaluation": directory / f"{pair}.evaluation.json",
        "raw": directory / f"{pair}.raw.json",
    }
    process, error = (require_json(paths["process"]), None) if strict else read_json(paths["process"])
    if error is not None:
        outputs["errors"].append(f"process: {error}")
    elif not isinstance(process, dict):
        outputs["errors"].append("process: expected an object")
    else:
        outputs["process"] = process
    for key in ("summary", "evaluation"):
        record, error = (require_single_record(paths[key]), None) if strict else optional_single_record(paths[key])
        if error is not None:
            outputs["errors"].append(f"{key}: {error}")
        elif record is not None:
            outputs[key] = record
    raw, error = read_json(paths["raw"])
    if error is not None and error != "missing":
        outputs["errors"].append(f"raw: {error}")
    elif raw is not None:
        outputs["raw"] = raw
    return outputs


def first_present(*values: Any) -> Any:
    for value in values:
        if value is not None:
            return value
    return None


def as_int(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def as_number(value: Any) -> int | float | None:
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) else None


def as_bool(value: Any) -> bool | None:
    return value if isinstance(value, bool) else None


def source_mask(record: dict[str, Any] | None, summary: dict[str, Any] | None) -> dict[str, Any] | None:
    if record is not None and isinstance(record.get("scoped_tokens"), dict):
        return record["scoped_tokens"]
    if summary is not None and isinstance(summary.get("scoped_token_metrics"), dict):
        return summary["scoped_token_metrics"]
    return None


def scoped_false_positive_tokens(mask: dict[str, Any] | None) -> int | None:
    if mask is None:
        return None
    reported = as_int(mask.get("reported_changed_tokens"))
    true_positive = as_int(mask.get("true_positive_tokens"))
    if reported is None or true_positive is None or true_positive > reported:
        return None
    return reported - true_positive


def diagnostic_snapshot(diagnostic: Any) -> dict[str, Any] | None:
    if not isinstance(diagnostic, dict):
        return None
    failures = diagnostic.get("failures")
    if not isinstance(failures, list) or not all(isinstance(item, dict) for item in failures):
        failures = None
    reasons = Counter(
        item.get("reason")
        for item in failures or []
        if isinstance(item.get("reason"), str)
    )
    recovery_watch = diagnostic.get("recovery_watch")
    recovery = None
    if isinstance(recovery_watch, dict):
        recovery = {
            key: recovery_watch.get(key)
            for key in ("complete", "candidate_generation_complete", "near_relation_complete",
                        "near_relation_stop_reason", "segment_stop_reason", "granular_stop_reason",
                        "quote_local_stop_reason")
            if key in recovery_watch
        }
    return {
        "available": True,
        "complete": as_bool(diagnostic.get("complete")),
        "failure_count": None if failures is None else len(failures),
        "failure_ids": None if failures is None else [item.get("expected_id") for item in failures],
        "failure_reasons": dict(sorted(reasons.items())),
        "failures": failures,
        "recovery_watch": recovery,
    }


def assessment_snapshot(record: dict[str, Any] | None, summary: dict[str, Any] | None) -> dict[str, Any] | None:
    assessment = record.get("assessment") if record is not None else None
    if not isinstance(assessment, dict) and summary is not None:
        diagnostic = summary.get("expected_change_diagnostics")
        final_assessment = diagnostic.get("final_assessment") if isinstance(diagnostic, dict) else None
        assessment = final_assessment.get("assessment") if isinstance(final_assessment, dict) else None
    if not isinstance(assessment, dict):
        return None
    return {
        key: assessment.get(key)
        for key in (
            "policy_version",
            "work_limit",
            "work_used",
            "work_by_stage",
            "candidates_truncated",
            "claim_diagnostics",
        )
        if key in assessment
    }


def raw_preview(raw: Any) -> list[dict[str, Any]] | None:
    records = raw if isinstance(raw, list) else raw.get("records") if isinstance(raw, dict) else None
    if not isinstance(records, list) or len(records) != 1 or not isinstance(records[0], dict):
        return None
    preview = records[0].get("reported_changes_preview")
    if not isinstance(preview, list) or not all(isinstance(item, dict) for item in preview):
        return None
    return preview


def absent_quote(value: Any) -> bool:
    return value is None or value == "-"


def raw_quote_matches(change: dict[str, Any], preview: list[dict[str, Any]]) -> list[int]:
    expected_kind = change.get("kind")
    old_quote = change.get("old_quote")
    new_quote = change.get("new_quote")
    matches = []
    for index, item in enumerate(preview):
        if item.get("kind") != expected_kind:
            continue
        old_text = item.get("old_text")
        new_text = item.get("new_text")
        old_matches = absent_quote(old_quote) if absent_quote(old_text) else old_text == old_quote
        new_matches = absent_quote(new_quote) if absent_quote(new_text) else new_text == new_quote
        if old_matches and new_matches:
            matches.append(index)
    return matches


def has_source_mask(change: dict[str, Any]) -> bool:
    return any(
        key in change and change[key] is not None
        for key in ("old_changed_ranges", "new_changed_ranges")
    )


def expected_match_statuses(
    changes: list[dict[str, Any]],
    outputs: dict[str, Any],
) -> tuple[dict[str, bool | None], dict[str, str], dict[str, Any]]:
    ids = [change["id"] for change in changes]
    statuses = {change_id: None for change_id in ids}
    bases = {change_id: "unavailable" for change_id in ids}
    summary = outputs.get("summary")
    evaluation = outputs.get("evaluation")
    quality = evaluation.get("quality") if isinstance(evaluation, dict) else None
    if not isinstance(quality, dict) and isinstance(summary, dict):
        quality = summary.get("quality")
    expected_count = as_int(quality.get("expected_changes")) if isinstance(quality, dict) else None
    matched_count = as_int(quality.get("matched_changes")) if isinstance(quality, dict) else None
    diagnostic = summary.get("expected_change_diagnostics") if isinstance(summary, dict) else None
    diagnostic_complete = as_bool(diagnostic.get("complete")) if isinstance(diagnostic, dict) else None
    failures = diagnostic.get("failures") if isinstance(diagnostic, dict) else None
    failure_by_id = {
        item.get("expected_id"): item
        for item in failures or []
        if isinstance(item, dict) and isinstance(item.get("expected_id"), str)
    }
    diagnostic_error = None
    attribution_inferences: list[str] = []
    official_source = next((record for record in (evaluation, summary)
                            if isinstance(record, dict) and "expected_matches" in record), None)
    if official_source is not None:
        assignments = official_source["expected_matches"]
        if assignments is not None:
            if not isinstance(assignments, list) or any(
                not isinstance(item, dict) or not isinstance(item.get("expected_id"), str)
                or not isinstance(item.get("matched"), bool) for item in assignments
            ):
                raise DataError("invalid official expected-match assignments")
            assigned_ids = [item["expected_id"] for item in assignments]
            if len(assigned_ids) != len(ids) or set(assigned_ids) != set(ids):
                raise DataError("official expected-match IDs do not cover the annotation exactly")
            statuses = {item["expected_id"]: item["matched"] for item in assignments}
            if expected_count != len(ids) or matched_count != sum(statuses.values()):
                raise DataError("official expected-match assignments disagree with quality counts")
            bases = {change_id: "official_event_matcher" for change_id in ids}
        # Explicit unavailability must never become a match inferred from preview text.
        return statuses, bases, {
            "quality_expected_changes": expected_count,
            "quality_matched_changes": matched_count,
            "diagnostic_complete": diagnostic_complete,
            "diagnostic_error": None,
            "attribution_inferences": [],
            "official_assignments": assignments,
        }
    if diagnostic_complete is True and isinstance(failures, list):
        unknown_ids = set(failure_by_id) - set(ids)
        failure_ids = [item.get("expected_id") for item in failures if isinstance(item, dict)]
        duplicate_ids = [change_id for change_id, count in Counter(failure_ids).items() if count > 1]
        if unknown_ids or duplicate_ids:
            diagnostic_error = "; ".join(
                message for message in (
                    f"diagnostic failure ids not in annotation: {sorted(unknown_ids)}" if unknown_ids else None,
                    f"diagnostic contains duplicate failure ids: {sorted(duplicate_ids)}" if duplicate_ids else None,
                ) if message is not None
            )
        else:
            for change_id in ids:
                statuses[change_id] = change_id not in failure_by_id
                bases[change_id] = "expected_change_diagnostics"
            if matched_count is not None and matched_count != sum(statuses.values()):
                diagnostic_error = (
                    f"diagnostic matched count {sum(statuses.values())} differs from quality matched count {matched_count}"
                )
                statuses = {change_id: None for change_id in ids}
                bases = {change_id: "inconsistent_diagnostics" for change_id in ids}
    elif diagnostic_complete is False and isinstance(failures, list):
        for change_id in ids:
            if change_id in failure_by_id:
                statuses[change_id] = False
                bases[change_id] = "expected_change_diagnostics_failure"
        if matched_count == 0 and expected_count == len(ids):
            for change_id in ids:
                if statuses[change_id] is None:
                    statuses[change_id] = False
                    bases[change_id] = "quality_zero_matched_changes"

    if diagnostic_complete is not True:
        preview = raw_preview(outputs.get("raw"))
        raw_matches = {change["id"]: raw_quote_matches(change, preview) for change in changes} if preview is not None else {}
        unique_matches = {change_id: matches[0] for change_id, matches in raw_matches.items() if len(matches) == 1}
        shared_matches = Counter(unique_matches.values())
        changes_by_id = {change["id"]: change for change in changes}
        attributable = {
            change_id: preview_index
            for change_id, preview_index in unique_matches.items()
            if not has_source_mask(changes_by_id[change_id])
            and shared_matches[preview_index] == 1
            and statuses[change_id] is None
        }
        known_matches = sum(status is True for status in statuses.values())
        if matched_count == 0 and expected_count == len(ids):
            statuses = {change_id: False for change_id in ids}
            bases = {change_id: "quality_zero_matched_changes" for change_id in ids}
        elif attributable and (
            matched_count is None
            or known_matches + len(attributable) <= matched_count
        ):
            for change_id in attributable:
                statuses[change_id] = True
                bases[change_id] = "raw_attribution_inference"
                attribution_inferences.append(change_id)
        elif attributable and matched_count is not None and known_matches + len(attributable) > matched_count:
            diagnostic_error = (
                f"raw attribution count {known_matches + len(attributable)} exceeds quality matched count {matched_count}"
            )

    evidence = {
        "quality_expected_changes": expected_count,
        "quality_matched_changes": matched_count,
        "diagnostic_complete": diagnostic_complete,
        "diagnostic_error": diagnostic_error,
        "attribution_inferences": attribution_inferences,
    }
    return statuses, bases, evidence


def run_snapshot(outputs: dict[str, Any]) -> dict[str, Any]:
    process = outputs.get("process")
    summary = outputs.get("summary")
    evaluation = outputs.get("evaluation")
    quality = evaluation.get("quality") if isinstance(evaluation, dict) else None
    if not isinstance(quality, dict) and isinstance(summary, dict):
        quality = summary.get("quality")
    mask = source_mask(evaluation if isinstance(evaluation, dict) else None,
                       summary if isinstance(summary, dict) else None)
    assessment = assessment_snapshot(evaluation if isinstance(evaluation, dict) else None,
                                     summary if isinstance(summary, dict) else None)
    status = first_present(
        evaluation.get("trial_status") if isinstance(evaluation, dict) else None,
        summary.get("status") if isinstance(summary, dict) else None,
    )
    return {
        "status": status,
        "exit_code": process.get("exit_code") if isinstance(process, dict) else None,
        "compared": first_present(
            evaluation.get("compared") if isinstance(evaluation, dict) else None,
            summary.get("compared") if isinstance(summary, dict) else None,
        ),
        "extraction_complete": first_present(
            evaluation.get("extraction_complete") if isinstance(evaluation, dict) else None,
            summary.get("extraction_complete") if isinstance(summary, dict) else None,
        ),
        "comparison_complete": first_present(
            evaluation.get("comparison_complete") if isinstance(evaluation, dict) else None,
            summary.get("comparison_complete") if isinstance(summary, dict) else None,
        ),
        "coverage_comparison": first_present(
            evaluation.get("coverage_comparison") if isinstance(evaluation, dict) else None,
            summary.get("coverage_comparison") if isinstance(summary, dict) else None,
        ),
        "unresolved_regions": first_present(
            evaluation.get("unresolved_regions") if isinstance(evaluation, dict) else None,
            summary.get("unresolved_regions") if isinstance(summary, dict) else None,
        ),
        "accepted_changes": first_present(
            evaluation.get("accepted_changes") if isinstance(evaluation, dict) else None,
            summary.get("reported_content_changes") if isinstance(summary, dict) else None,
        ),
        "candidate_changes": evaluation.get("candidate_changes") if isinstance(evaluation, dict) else None,
        "formatting_only_changes": first_present(
            evaluation.get("formatting_only_changes") if isinstance(evaluation, dict) else None,
            summary.get("reported_formatting_changes") if isinstance(summary, dict) else None,
        ),
        "uncertain_changes": first_present(
            evaluation.get("uncertain_changes") if isinstance(evaluation, dict) else None,
            summary.get("reported_uncertain_changes") if isinstance(summary, dict) else None,
        ),
        "quality_skipped_reason": first_present(
            evaluation.get("quality_skipped_reason") if isinstance(evaluation, dict) else None,
            summary.get("quality_skipped_reason") if isinstance(summary, dict) else None,
        ),
        "quality": quality,
        "source_mask": mask,
        "source_mask_false_positive_tokens": scoped_false_positive_tokens(mask),
        "diagnostics": diagnostic_snapshot(
            summary.get("expected_change_diagnostics") if isinstance(summary, dict) else None
        ),
        "assessment": assessment,
        "resource_limit_failure": first_present(
            summary.get("resource_limit_failure") if isinstance(summary, dict) else None,
            evaluation.get("resource_limit_failure") if isinstance(evaluation, dict) else None,
        ),
        "resources": {
            "process_seconds": process.get("seconds") if isinstance(process, dict) else None,
            "process_timeout_seconds": process.get("timeout_seconds") if isinstance(process, dict) else None,
            "process_peak_rss_bytes": process.get("peak_rss_bytes") if isinstance(process, dict) else None,
            "evaluation_runtime_ms": first_present(
                evaluation.get("trial_runtime_ms") if isinstance(evaluation, dict) else None,
                summary.get("runtime_ms") if isinstance(summary, dict) else None,
            ),
            "evaluation_peak_memory_bytes": evaluation.get("peak_memory_bytes") if isinstance(evaluation, dict) else None,
        },
        "artifact_errors": outputs.get("errors", []),
    }


def sum_metric(records: list[dict[str, Any]], key: str, numeric: bool = False) -> dict[str, Any]:
    parser = as_number if numeric else as_int
    known = [(record["pair"], parser(record.get(key))) for record in records if parser(record.get(key)) is not None]
    missing = [record["pair"] for record in records if parser(record.get(key)) is None]
    total = sum(value for _, value in known) if not missing else None
    return {
        "total": total,
        "known_total": sum(value for _, value in known) if known else None,
        "available_pairs": len(known),
        "missing_pairs": missing,
    }


def status_counts(records: list[dict[str, Any]]) -> dict[str, int]:
    return dict(sorted(Counter(record.get("status") or "unavailable" for record in records).items()))


def side_aggregate(records: list[dict[str, Any]]) -> dict[str, Any]:
    extraction_incomplete = [
        record["pair"] for record in records if record.get("extraction_complete") is False
    ]
    unsupported = [record["pair"] for record in records if record.get("status") == "unsupported"]
    unresolved = [record["pair"] for record in records if record.get("status") == "unresolved"]
    return {
        "status_counts": status_counts(records),
        "extraction_incomplete_pairs": extraction_incomplete,
        "unsupported_pairs": unsupported,
        "unresolved_pairs": unresolved,
        "comparison_complete_pairs": [record["pair"] for record in records if record.get("comparison_complete") is True],
        "accepted_changes": sum_metric(records, "accepted_changes"),
        "candidate_changes": sum_metric(records, "candidate_changes"),
        "formatting_only_changes": sum_metric(records, "formatting_only_changes"),
        "uncertain_changes": sum_metric(records, "uncertain_changes"),
        "process_seconds": sum_metric(
            [{"pair": r["pair"], "process_seconds": r["resources"].get("process_seconds")} for r in records],
            "process_seconds",
            numeric=True,
        ),
        "evaluation_runtime_ms": sum_metric(
            [{"pair": r["pair"], "evaluation_runtime_ms": r["resources"].get("evaluation_runtime_ms")} for r in records],
            "evaluation_runtime_ms",
        ),
        "process_peak_rss_bytes": {
            "max": max(
                (value for value in (record["resources"].get("process_peak_rss_bytes") for record in records)
                 if as_int(value) is not None),
                default=None,
            ),
            "missing_pairs": [
                record["pair"] for record in records
                if as_int(record["resources"].get("process_peak_rss_bytes")) is None
            ],
        },
        "evaluation_peak_memory_bytes": {
            "max": max(
                (value for value in (record["resources"].get("evaluation_peak_memory_bytes") for record in records)
                 if as_int(value) is not None),
                default=None,
            ),
            "missing_pairs": [
                record["pair"] for record in records
                if as_int(record["resources"].get("evaluation_peak_memory_bytes")) is None
            ],
        },
        "scoped_false_positive_tokens": {
            "total": sum_metric(records, "source_mask_false_positive_tokens")["total"],
            "known_total": sum_metric(records, "source_mask_false_positive_tokens")["known_total"],
            "available_pairs": sum_metric(records, "source_mask_false_positive_tokens")["available_pairs"],
            "missing_pairs": sum_metric(records, "source_mask_false_positive_tokens")["missing_pairs"],
        },
        "assessment_work": {
            "work_limit": sum_metric(
                [{"pair": r["pair"], "work_limit": (r.get("assessment") or {}).get("work_limit")} for r in records],
                "work_limit",
            ),
            "work_used": sum_metric(
                [{"pair": r["pair"], "work_used": (r.get("assessment") or {}).get("work_used")} for r in records],
                "work_used",
            ),
            "resource_limit_pairs": [
                record["pair"] for record in records
                if record.get("status") == "limit" or record.get("resource_limit_failure") is not None
            ],
        },
    }


def expected_transition(baseline: bool | None, candidate: bool | None) -> str:
    if baseline is None:
        return "baseline_unmeasurable"
    if candidate is None:
        return "candidate_unavailable"
    if baseline and candidate:
        return "retained_match"
    if baseline and not candidate:
        return "lost_match"
    if not baseline and candidate:
        return "gained_match"
    return "still_unmatched"


def safe_output_path(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        raise DataError(f"refusing to overwrite existing output: {path}")
    return path


def build_report(candidate_dir: Path, baseline_dir: Path, manifest_path: Path) -> dict[str, Any]:
    rows = load_manifest(manifest_path)
    baseline_full = require_json(baseline_dir / "full-summary.json")
    baseline_annotated = require_json(baseline_dir / "annotated-summary.json")
    if not isinstance(baseline_full, dict) or not isinstance(baseline_full.get("records"), list):
        raise DataError(f"{baseline_dir / 'full-summary.json'}: invalid full summary")
    if not isinstance(baseline_annotated, dict) or not isinstance(baseline_annotated.get("records"), list):
        raise DataError(f"{baseline_dir / 'annotated-summary.json'}: invalid annotated summary")
    baseline_full_by_pair = {item.get("pair"): item for item in baseline_full["records"] if isinstance(item, dict)}
    baseline_annotated_by_pair = {item.get("pair"): item for item in baseline_annotated["records"] if isinstance(item, dict)}
    manifest_pairs = {row["pair_id"] for row in rows}
    annotated_pairs = {row["pair_id"] for row in rows if row["expected_file"] != "-"}
    if set(baseline_full_by_pair) != manifest_pairs or set(baseline_annotated_by_pair) != annotated_pairs:
        raise DataError("baseline summaries do not cover the manifest")

    records = []
    expectations = []
    for row in rows:
        pair = row["pair_id"]
        expected = load_expected(manifest_path.parent / row["expected_file"]) if row["expected_file"] != "-" else None
        changes = expected["changes"] if expected is not None else []
        baseline_outputs = load_outputs(baseline_dir, pair, strict=True)
        candidate_outputs = load_outputs(candidate_dir, pair, strict=False)
        baseline_run = run_snapshot(baseline_outputs)
        candidate_run = run_snapshot(candidate_outputs)
        baseline_annotation = baseline_annotated_by_pair.get(pair)
        baseline_expectation_items = {
            item["expected_id"]: item.get("matched")
            for item in (baseline_annotation or {}).get("expectations", [])
            if isinstance(item, dict) and isinstance(item.get("expected_id"), str)
        }
        if set(baseline_expectation_items) != {change["id"] for change in changes}:
            raise DataError(f"baseline annotation ids do not match expected file for {pair}")
        candidate_statuses, candidate_bases, candidate_evidence = expected_match_statuses(changes, candidate_outputs)
        reviewed = []
        for change in changes:
            change_id = change["id"]
            baseline_matched = baseline_expectation_items.get(change_id)
            if not isinstance(baseline_matched, bool):
                baseline_matched = None
            candidate_matched = candidate_statuses[change_id]
            detail = {
                "expected_id": change_id,
                "kind": change.get("kind"),
                "expected_changed_ranges": {
                    "old": change.get("old_changed_ranges"),
                    "new": change.get("new_changed_ranges"),
                },
                "expected_occurrence_count": change.get("occurrence_count"),
                "baseline_matched": baseline_matched,
                "candidate_matched": candidate_matched,
                "transition": expected_transition(baseline_matched, candidate_matched),
                "candidate_match_basis": candidate_bases[change_id],
                "candidate_official_assignment": next(
                    (item for item in candidate_evidence.get("official_assignments") or []
                     if item["expected_id"] == change_id), None,
                ),
                "candidate_failure": None,
            }
            diagnostic = candidate_outputs.get("summary", {}).get("expected_change_diagnostics")
            if isinstance(diagnostic, dict) and isinstance(diagnostic.get("failures"), list):
                detail["candidate_failure"] = next(
                    (item for item in diagnostic["failures"]
                     if isinstance(item, dict) and item.get("expected_id") == change_id),
                    None,
                )
            reviewed.append(detail)
            expectations.append({"pair": pair, **detail})
        candidate_run["expected_match_evidence"] = candidate_evidence if changes else None
        baseline_run["expected_match_evidence"] = {
            "quality_expected_changes": as_int((baseline_run.get("quality") or {}).get("expected_changes")),
            "quality_matched_changes": as_int((baseline_run.get("quality") or {}).get("matched_changes")),
            "source": "frozen_annotated_summary",
        } if changes else None
        records.append({
            "pair": pair,
            "split": row.get("set"),
            "role": row.get("role"),
            "expected_file": row.get("expected_file") if row.get("expected_file") != "-" else None,
            "annotation": expected.get("annotation") if expected is not None else None,
            "expected_extraction": row.get("expected_extraction"),
            "baseline": baseline_run,
            "candidate": candidate_run,
            "reviewed_expectations": reviewed,
        })

    baseline_records = [{"pair": r["pair"], **r["baseline"]} for r in records]
    candidate_records = [{"pair": r["pair"], **r["candidate"]} for r in records]
    baseline_exact_ids = [
        f"{item['pair']}:{item['expected_id']}" for item in expectations if item["baseline_matched"] is True
    ]
    candidate_exact_ids = [
        f"{item['pair']}:{item['expected_id']}" for item in expectations if item["candidate_matched"] is True
    ]
    candidate_unknown = [item for item in expectations if item["candidate_matched"] is None]
    candidate_attribution_inferences = [
        item for item in expectations
        if item["candidate_match_basis"] == "raw_attribution_inference"
    ]
    candidate_diagnostic_exact_ids = [
        f"{item['pair']}:{item['expected_id']}"
        for item in expectations
        if item["candidate_matched"] is True
        and item["candidate_match_basis"] in ("official_event_matcher", "expected_change_diagnostics")
    ]
    baseline_measurable = [item for item in expectations if item["baseline_matched"] is not None]
    if baseline_annotated.get("source_expected") != len(expectations):
        raise DataError("baseline source_expected does not match the manifest annotations")
    if baseline_annotated.get("measured_expected") != len(baseline_measurable):
        raise DataError("baseline measured_expected does not match measurable annotations")
    candidate_measurable_pairs = {
        record["pair"] for record in records
        if as_int((record["candidate"].get("expected_match_evidence") or {}).get("quality_expected_changes")) is not None
        and as_int((record["candidate"].get("expected_match_evidence") or {}).get("quality_matched_changes")) is not None
    }
    candidate_measurable = [item for item in expectations if item["pair"] in candidate_measurable_pairs]
    transition_counts = Counter(item["transition"] for item in expectations)
    baseline_aggregate = side_aggregate(baseline_records)
    candidate_aggregate = side_aggregate(candidate_records)
    metadata, metadata_error = read_json(candidate_dir / "metadata.json")
    candidate_artifact_errors = []
    if metadata_error == "missing":
        metadata = None
    elif metadata_error is not None:
        candidate_artifact_errors.append(f"metadata: {metadata_error}")
    fixed_denominator_matches = 0
    for record in records:
        measurable = sum(item["baseline_matched"] is not None for item in record["reviewed_expectations"])
        if not measurable:
            continue
        evidence = record["candidate"].get("expected_match_evidence") or {}
        matched = as_int(evidence.get("quality_matched_changes"))
        if evidence.get("quality_expected_changes") != measurable or matched is None:
            fixed_denominator_matches = None
            break
        if not 0 <= matched <= measurable:
            raise DataError(f"invalid official match count for {record['pair']}")
        fixed_denominator_matches += matched
    headline = {
        "pairs": len(records),
        "reviewed_expectations": len(expectations),
        "baseline_measurable_expectations": len(baseline_measurable),
        "candidate_measurable_expectations": len(candidate_measurable),
        "baseline_exact_matches": len(baseline_exact_ids),
        "candidate_exact_matches_lower_bound": sum(
            item["candidate_matched"] is True for item in baseline_measurable
        ),
        "candidate_exact_matches": fixed_denominator_matches,
        "baseline_scoped_false_positive_tokens": baseline_aggregate["scoped_false_positive_tokens"]["known_total"],
        "candidate_scoped_false_positive_tokens": candidate_aggregate["scoped_false_positive_tokens"]["known_total"],
        "baseline_accepted_changes": baseline_aggregate["accepted_changes"]["total"],
        "candidate_accepted_changes": candidate_aggregate["accepted_changes"]["total"],
    }
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "comparer_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "methodology": {
            "exact_event_matches": "The aggregate uses official quality matched_changes over the frozen measurable expectations; it is unavailable if any formerly measurable pair loses its quality count. New per-ID assignments come directly from the official matcher; an explicit null remains unavailable. Legacy runs can use complete diagnostics, while unmasked injective raw-preview quotes remain attribution inferences.",
            "unavailable_measurements": "null; missing artifacts and incomplete diagnostics are retained in each pair record.",
            "denominator": "The frozen baseline denominator remains 40 reviewed expectations, of which 39 were measurable; candidate measurability is reported separately.",
        },
        "manifest": {
            "path": str(manifest_path.resolve()),
            "pairs": len(rows),
            "expected_extraction_incomplete_pairs": [
                row["pair_id"] for row in rows if row["expected_extraction"] != "complete"
            ],
        },
        "baseline": {
            "directory": str(baseline_dir.resolve()),
            "annotated_reference": {
                key: baseline_annotated.get(key)
                for key in ("source_commit", "patch", "annotated_pairs", "source_expected", "measured_expected",
                            "matched", "scoped_false_positive_tokens")
                if key in baseline_annotated
            },
            "reference": {
                key: baseline_full.get(key)
                for key in ("total_pairs", "status_counts", "external_timeouts", "crashes", "complete_comparisons")
                if key in baseline_full
            },
            "aggregate": baseline_aggregate,
        },
        "candidate": {
            "directory": str(candidate_dir.resolve()),
            "metadata": metadata,
            "artifact_errors": candidate_artifact_errors,
            "aggregate": candidate_aggregate,
        },
        "comparison": {
            "headline": headline,
            "status_changes": [
                {
                    "pair": record["pair"],
                    "baseline": record["baseline"]["status"],
                    "candidate": record["candidate"]["status"],
                }
                for record in records
                if record["baseline"]["status"] != record["candidate"]["status"]
            ],
            "accepted_changes_delta": (
                candidate_aggregate["accepted_changes"]["total"] - baseline_aggregate["accepted_changes"]["total"]
                if candidate_aggregate["accepted_changes"]["total"] is not None
                and baseline_aggregate["accepted_changes"]["total"] is not None else None
            ),
            "scoped_false_positive_tokens_delta": (
                candidate_aggregate["scoped_false_positive_tokens"]["total"]
                - baseline_aggregate["scoped_false_positive_tokens"]["total"]
                if candidate_aggregate["scoped_false_positive_tokens"]["total"] is not None
                and baseline_aggregate["scoped_false_positive_tokens"]["total"] is not None else None
            ),
            "exact_match_ids": {
                "baseline": baseline_exact_ids,
                "candidate_known": candidate_exact_ids,
                "candidate_diagnostic_exact": candidate_diagnostic_exact_ids,
                "candidate_attribution_inferences": [
                    f"{item['pair']}:{item['expected_id']}"
                    for item in candidate_attribution_inferences
                ],
                "candidate_unknown": [f"{item['pair']}:{item['expected_id']}" for item in candidate_unknown],
            },
            "expectation_transition_counts": dict(sorted(transition_counts.items())),
            "expectations": expectations,
        },
        "records": records,
    }


def format_value(value: Any) -> str:
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, float):
        return f"{value:.3f}"
    return str(value)


def markdown_report(report: dict[str, Any]) -> str:
    comparison = report["comparison"]
    headline = comparison["headline"]
    baseline = report["baseline"]["aggregate"]
    candidate = report["candidate"]["aggregate"]
    lines = [
        "# Claim evaluation comparison",
        "",
        f"Candidate: `{report['candidate']['directory']}`",
        f"Baseline: `{report['baseline']['directory']}`",
        "",
        "## Measured headline",
        "",
        f"- Pairs: {headline['pairs']}.",
        f"- Reviewed expectations: {headline['reviewed_expectations']} total; frozen baseline measurable: {headline['baseline_measurable_expectations']}; candidate measurable: {headline['candidate_measurable_expectations']}.",
        f"- Exact event matches on the frozen measurable set (official matcher): baseline {headline['baseline_exact_matches']}; candidate {format_value(headline['candidate_exact_matches'])}. Per-ID attribution lower bound: {headline['candidate_exact_matches_lower_bound']}.",
        f"- Scoped false-positive tokens: baseline {format_value(headline['baseline_scoped_false_positive_tokens'])}; candidate {format_value(headline['candidate_scoped_false_positive_tokens'])}.",
        f"- Accepted events: baseline {format_value(headline['baseline_accepted_changes'])}; candidate {format_value(headline['candidate_accepted_changes'])}.",
        "",
        "The frozen denominator remains 40 reviewed expectations and 39 measurable expectations. Source-mask token overlap is kept separate from exact event matching.",
        "",
        "## Status and extraction",
        "",
        f"- Baseline statuses: {', '.join(f'{key}={value}' for key, value in baseline['status_counts'].items())}.",
        f"- Candidate statuses: {', '.join(f'{key}={value}' for key, value in candidate['status_counts'].items())}.",
        f"- Baseline unsupported extraction pairs: {len(baseline['unsupported_pairs'])}.",
        f"- Candidate unsupported extraction pairs: {len(candidate['unsupported_pairs'])}.",
        f"- Baseline extraction-incomplete records: {len(baseline['extraction_incomplete_pairs'])}; candidate: {len(candidate['extraction_incomplete_pairs'])}.",
        "",
        "## Pair ledger",
        "",
        "| Pair | Baseline | Candidate | Accepted events | Candidate events | Coverage | Limit | Time (s) | RSS (bytes) |",
        "| --- | --- | --- | ---: | ---: | ---: | --- | ---: | ---: |",
    ]
    for record in report["records"]:
        before = record["baseline"]
        after = record["candidate"]
        lines.append("| {pair} | {before} | {after} | {accepted} | {candidate_events} | {coverage} | {limit} | {seconds} | {rss} |".format(
            pair=record["pair"],
            before=format_value(before["status"]),
            after=format_value(after["status"]),
            accepted=format_value(after["accepted_changes"]),
            candidate_events=format_value(after["candidate_changes"]),
            coverage=format_value(after["coverage_comparison"]),
            limit=format_value(after["assessment"].get("work_limit") if after.get("assessment") else None),
            seconds=format_value(after["resources"].get("process_seconds")),
            rss=format_value(after["resources"].get("process_peak_rss_bytes")),
        ))
    lines.extend(["", "## ID evidence", ""])
    for key in (
        "baseline",
        "candidate_diagnostic_exact",
        "candidate_attribution_inferences",
        "candidate_unknown",
    ):
        values = comparison["exact_match_ids"][key]
        lines.append(f"- {key}: {', '.join(f'`{value}`' for value in values) if values else 'none'}.")
    lines.extend(["", "## Expectation transitions", ""])
    lines.extend(
        f"- {key}: {value}"
        for key, value in comparison["expectation_transition_counts"].items()
    )
    lines.extend([
        "",
        "## Evidence limits",
        "",
        "Missing process, summary, evaluation, raw, or incomplete diagnostic artifacts remain unavailable. Candidate event counts, source-mask metrics, assessment budgets, process time, evaluation time, and memory are reported independently per pair.",
        "",
    ])
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate_dir", type=Path)
    parser.add_argument("output_json", type=Path)
    parser.add_argument("output_markdown", type=Path)
    parser.add_argument("--baseline-dir", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    args = parser.parse_args()
    if not args.candidate_dir.is_dir():
        parser.error(f"candidate directory does not exist: {args.candidate_dir}")
    if not args.baseline_dir.is_dir():
        parser.error(f"baseline directory does not exist: {args.baseline_dir}")
    if not args.manifest.is_file():
        parser.error(f"manifest does not exist: {args.manifest}")
    try:
        output_json = safe_output_path(args.output_json)
        output_markdown = safe_output_path(args.output_markdown)
        if output_json.resolve() == output_markdown.resolve():
            raise DataError("JSON and Markdown output paths must differ")
        report = build_report(args.candidate_dir, args.baseline_dir, args.manifest)
        output_json.open("x").write(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
        output_markdown.open("x").write(markdown_report(report))
    except (DataError, OSError, UnicodeError) as error:
        parser.error(str(error))
    print(json.dumps(report["comparison"]["headline"], indent=2))


if __name__ == "__main__":
    main()
