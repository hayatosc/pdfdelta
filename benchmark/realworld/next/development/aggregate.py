#!/usr/bin/env python3
"""Aggregate retained development observations without rerunning comparisons."""

import argparse
import hashlib
import json
from pathlib import Path


ROOT = Path(__file__).resolve().parent


def read(relative):
    return json.loads((ROOT / relative).read_text())


def first(row, *keys):
    return next((row[key] for key in keys if row.get(key) is not None), None)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    input_manifest = read("inputs.json")
    inputs = {p["id"]: p for p in input_manifest["pairs"] if "replacement" not in p}
    registration = {p["id"]: p for p in read("selection.json")["pairs"] if p["id"] in inputs}
    assert len(registration) == 24
    records, runs = {}, {}
    sources = ["inputs.json", "selection.json", "stage-evidence.json"]
    stages = {(r["revision"], r["pair"], r["route"]): (r, index)
              for index, r in enumerate(read("stage-evidence.json")["rows"])}
    (args.output / "replaced-attempts.json").write_text(json.dumps(
        [p for p in input_manifest["pairs"] if "replacement" in p], indent=2) + "\n")

    def add_scores(path, revision_key=None, selected=None, native=False):
        sources.append(path)
        for index, row in enumerate(read(path)):
            if selected is not None and row.get(revision_key) not in selected:
                continue
            if revision_key:
                label = row.get(revision_key)
                revisions = ["baseline" if label in ("3f318d7", "baseline") else "current"] if label else ["baseline", "current"]
            else:
                revisions = ["baseline", "current"]
            for revision in revisions:
                route = "native" if native else row["route"]
                key = (revision, row["pair"], route)
                assert key not in records, key
                records[key] = (row, f"{path}#/{index}")

    for directory in ("irs-results", "nist-edpb-results"):
        add_scores(f"{directory}/scores.json")
    add_scores("remaining-results/scores.json", "revision")
    add_scores("two-final-results/scores.json", "revision")
    for name, native in (("shared-scores.json", False), ("native-contracts.json", True)):
        add_scores(f"arxiv-page-gap-results/{name}", "implementation", {"3f318d7", "page-gap"}, native)
    for directory in ("irs-results", "nist-edpb-results", "remaining-results", "two-final-results", "arxiv-page-gap-results"):
        for revision in ("baseline", "current"):
            if directory == "two-final-results":
                filename = revision + "-runs.json"
            elif directory == "arxiv-page-gap-results" and revision == "current":
                filename = "page-gap-runs.json"
            else:
                filename = ("3f318d7" if revision == "baseline" else "9093cab") + "-runs.json"
            path = f"{directory}/{filename}"
            sources.append(path)
            document = read(path)
            for index, run in enumerate(document["runs"]):
                key = (revision, run["pair"], run["route"])
                assert key not in runs, key
                runs[key] = (run, document, f"{path}#/runs/{index}")
    assert records.keys() == runs.keys()
    assert len(records) == 144
    annotations = {}
    for id in inputs:
        path = f"annotations/{id}.resolved.json"
        resolved = read(path)
        annotation = read(f"annotations/{id}.json")
        assert not resolved["selector_resolution_complete"] or "selectors" in resolved
        annotations[id] = {
            "pair_resolved": resolved["selector_resolution_complete"],
            "attempted_selectors": len(annotation["selectors"]),
            "unique_selectors": sum(s["status"] == "unique" for s in resolved.get("selectors", [])),
            "source_extraction_complete": {side: (resolved.get(side) or {}).get("extraction_complete") for side in ("old", "new")},
        }
        sources.extend((path, f"annotations/{id}.json", f"annotations/{id}.expectations.json"))
    rows = []
    for key in sorted(records):
        revision, id, route = key
        score, pointer = records[key]
        run, capture, run_pointer = runs[key]
        summary = score.get("summary", {})
        numeric = score.get("strict_numeric_target") or {}
        event_denominator = first(numeric, "event_denominator")
        if event_denominator is None:
            event_denominator = first(score, "strict_numeric_event_denominator")
        if score.get("end_to_end_numeric_event_recall"):
            event_denominator = score["end_to_end_numeric_event_recall"]["denominator"]
        event_hits = first(numeric, "true_events")
        if event_hits is None:
            event_hits = first(score, "strict_numeric_event_hits")
        if score.get("end_to_end_numeric_event_recall"):
            event_hits = score["end_to_end_numeric_event_recall"]["numerator"]
        atom_denominator = first(numeric, "changed_source_atom_denominator")
        if atom_denominator is None:
            atom_denominator = first(score, "strict_numeric_changed_source_denominator")
        atom_hits = first(numeric, "true_source_atoms")
        if atom_hits is None:
            atom_hits = first(score, "strict_numeric_changed_source_hits")
        if score.get("end_to_end_numeric_source_recall"):
            atom_denominator = score["end_to_end_numeric_source_recall"]["denominator"]
            atom_hits = score["end_to_end_numeric_source_recall"]["numerator"]
        observed_stage, stage_index = stages.get(key, ({}, None))
        stage = {field: observed_stage.get("fields", {}).get(field) for field in (
            "enumeration", "conflict_search", "optimization", "text_search", "inventory", "source_comparison",
        )}
        rows.append({
            "revision": revision, "pair": id, "family": registration[id]["family"],
            "publisher": registration[id]["publisher"], "language": registration[id]["language"],
            "route": route, "score_source": pointer, "capture_source": run_pointer,
            "binary_sha256": capture["binary_sha256"], "build_label": capture["implementation_commit"],
            "status": run["status"], "exit_code": run["exit_code"],
            "complete": first(score, "comparison_complete") if not summary else summary["comparison_complete"],
            "strict_predictions": first(score, "A", "strict_events", "strict_typed_changes") if not summary else summary["established_changes"],
            "strict_numeric_event_denominator": event_denominator, "strict_numeric_event_hits": event_hits,
            "strict_numeric_atom_denominator": atom_denominator, "strict_numeric_atom_hits": atom_hits,
            "B_predictions": observed_stage.get("B"),
            "B_frozen_hits": first(score, "B_detected", "frozen_changed_scope_hits") if route != "native" else None,
            "frozen_scope_denominator": int(annotations[id]["pair_resolved"]),
            "scope_C": observed_stage.get("scope_C"), "legacy_C": observed_stage.get("legacy_C"),
            "stages": stage, "stage_source": None if stage_index is None else f"stage-evidence.json#/rows/{stage_index}",
            "coverage": score.get("coverage"),
            "wall_seconds": run["wall_seconds"], "peak_rss_kib": run["peak_rss_kib"],
            "report_bytes": run["report_bytes"], "annotation": annotations[id],
        })
    (args.output / "pairs.json").write_text(json.dumps(rows, indent=2) + "\n")
    family_rows = []
    for revision in ("baseline", "current"):
        for family in sorted({r["family"] for r in rows}):
            for route in ("native", "text", "all"):
                group = [r for r in rows if r["revision"] == revision and r["family"] == family and r["route"] == route]
                assert len(group) == 4
                totals = {"revision": revision, "family": family, "route": route, "attempted_pairs": len(group),
                          "captured_pairs": sum(r["status"] == "captured" for r in group),
                          "complete_pairs": sum(r["complete"] is True for r in group),
                          "resolved_reference_pairs": sum(r["annotation"]["pair_resolved"] for r in group)}
                for field in ("strict_numeric_event_denominator", "strict_numeric_event_hits", "strict_numeric_atom_denominator", "strict_numeric_atom_hits",
                              "strict_predictions", "B_predictions", "B_frozen_hits", "frozen_scope_denominator", "scope_C", "legacy_C", "report_bytes", "wall_seconds"):
                    observed = [r[field] for r in group if r[field] is not None]
                    totals[field] = sum(observed) if observed else None
                totals["peak_rss_kib"] = max((r["peak_rss_kib"] for r in group if r["peak_rss_kib"] is not None), default=None)
                totals["stages"] = {stage: {"complete": sum(r["stages"][stage] is True for r in group),
                                           "observed": sum(r["stages"][stage] is not None for r in group), "attempted": 4}
                                    for stage in ("enumeration", "conflict_search", "optimization", "text_search", "inventory", "source_comparison")}
                family_rows.append(totals)
    (args.output / "families.json").write_text(json.dumps(family_rows, indent=2) + "\n")
    (args.output / "sources.json").write_text(json.dumps({path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in sorted(set(sources))}, indent=2) + "\n")


if __name__ == "__main__":
    main()
