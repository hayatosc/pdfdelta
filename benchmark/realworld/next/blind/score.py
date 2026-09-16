#!/usr/bin/env python3
"""Score frozen blind references without changing their pre-comparison meaning."""

import argparse
from collections import Counter
import hashlib
import json
import mmap
from pathlib import Path


ROOT = Path(__file__).resolve().parent


def read(path):
    return json.loads(path.read_text())


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def ratio(n, d):
    return n / d if d else None


def native_summary(path):
    """Read bounded fields from the CLI's pretty JSON; skip huge region evidence.

    This is deliberately specific to the frozen serializer's two-space root
    indentation. A different format fails instead of silently returning no data.
    The complete file hash is checked separately against the capture ledger.
    """
    result = {}
    with path.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
        assert data[:2] == b"{\n"
        for key in ("schema_version", "summary", "changes", "change_candidates", "extraction"):
            start = data.find(f'\n  "{key}": '.encode())
            assert start >= 0, key
            end = data.find(b'\n  "', start + 1)
            if end < 0:
                end = data.rfind(b"\n}")
            assert start < end and end - start <= 128 * 1024 * 1024, key
            fragment = data[start:end].rstrip().removesuffix(b",")
            result.update(json.loads(b"{" + fragment + b"}"))
    assert result["schema_version"] == 11
    return result


def source_set(sources, side):
    return {(side, s["glyph"]) for s in sources if s["origin"] == "native"}


def comparison_atoms(comparison):
    mask = comparison["text_mask"]
    return set() if mask is None else {
        atom for side in ("old", "new") for token in mask[side]
        for atom in source_set(token["sources"], side)
    }


def reference_sets(reference, resolved):
    target = {(side, atom["id"]) for side, region in reference["positive_scope"]["old_new_regions"].items()
              for atom in region["source_atoms"] if atom["kind"] == "glyph"}
    unchanged = {(selector["id"].rsplit("-", 1)[1], atom["id"])
                 for selector in resolved["selectors"] if selector["id"] in reference["controls"]
                 for row in selector["sources"] for atom in row["atoms"] if atom["kind"] == "glyph"}
    numeric = reference.get("numeric_target")
    gold = None if numeric is None else {
        (side, atom["id"]) for side, value in numeric["sides"].items() for atom in value["changed_sources"]
    }
    if numeric:
        unchanged.update((side, atom["id"]) for side, value in numeric["sides"].items()
                         for atom in value["unchanged_sources"])
    return target, unchanged, gold


def native_event(change):
    sources = {(side, source["glyph_id"]) for occurrence in change["occurrences"]
               for side in ("old", "new") for source in (occurrence.get(side + "_span") or {}).get("sources", [])
               if source["kind"] == "glyph"}
    return change["kind"], sources


def score_native(report, row, target, negative, gold):
    summary = report["summary"]
    events = [native_event(change) for change in report["changes"]]
    candidates = [native_event(change) for change in report["change_candidates"]]
    assert len(candidates) == summary["tentative_candidates"]
    candidate_atoms = set().union(*(sources for _, sources in candidates))
    row["native_candidate_diagnostics"] = {
        "predictions": len(candidates),
        "touching_target": sum(bool(sources & target) for _, sources in candidates),
        "touching_both_target_sides": sum(all(any(atom[0] == side for atom in sources & target)
                                                for side in ("old", "new")) for _, sources in candidates),
        "unchanged_control_atoms_claimed_conditionally": len(candidate_atoms & negative),
        "interpretation": "Tentative native edits retain their original assumptions; target overlap is not counterpart proof or strict recall.",
    }
    if gold is not None:
        scored = [(kind, sources) for kind, sources in candidates if sources & target]
        hits = sum(kind == "replacement" and sources == gold for kind, sources in scored)
        atoms = candidate_atoms & target
        row["native_C_numeric"] = {"event_tp": min(hits, 1), "event_fp": len(scored) - min(hits, 1), "event_fn": int(not hits),
                                   "source_tp": len(atoms & gold), "source_fp": len(atoms - gold), "source_fn": len(gold - atoms),
                                   "unscored_candidates": len(candidates) - len(scored)}
    row.update(A=len(events), B=None, scope_C=None, legacy_C=summary["tentative_candidates"],
               native_proven_regions=summary["proven_changed_regions"], summary=summary,
               stages={"acquisition": report["extraction"]["old_complete"] and report["extraction"]["new_complete"],
                       "enumeration": None, "conflict_search": None, "optimization": None,
                       "source_comparison": None, "overall": summary["comparison_complete"]},
               extraction_issues=dict(Counter(issue["kind"] for issue in report["extraction"]["issues"])))
    return events


def score_common(report, row, target, annotation):
    scopes = [s["result"] for s in report["comparison"]["scopes"]]
    comparisons = [c for s in scopes for c in s["comparisons"]]
    events = []
    for c in comparisons:
        if c["operation"] is not None and c["interpretation"] == "conditional_on_correspondence":
            # A text operation owns only the positions in its strict mask.
            kind = c["operation"]["kind"]
            assert kind in ("text_changed", "value_changed", "rendered_region_changed", "page_rendering_changed"), kind
            events.append(("replacement" if kind == "text_changed" else kind, comparison_atoms(c)))
    reviews = []
    for si, scope in enumerate(scopes):
        for ri, review in enumerate(scope.get("text_scope_reviews", [])):
            sources = source_set(review["old_sources"], "old") | source_set(review["new_sources"], "new")
            intersection = sources & target
            reviews.append({
                "pointer": f"/comparison/scopes/{si}/result/text_scope_reviews/{ri}",
                "category": "B" if review["comparison"]["interpretation"] == "conditional_on_correspondence" else "C",
                "predicted_atoms": len(sources), "target_intersection": len(intersection),
                "range_exact": bool(target) and sources == target,
                "contains_target": bool(target) and target <= sources,
                "touches_both_sides": all(any(atom[0] == side for atom in intersection) for side in ("old", "new")),
                "range_precision": ratio(len(intersection), len(sources)),
                "range_recall": ratio(len(intersection), len(target)),
                "extra_context_atoms": len(sources - target),
                "quality_status": "partial_reference_overlap" if intersection else "unannotated",
            })
    legacy = [c for c in comparisons if c["operation"] is not None and c["interpretation"] == "inferred"]
    quotes = {s["side"]: s["literal_quote"] for s in annotation["selectors"] if s["id"].startswith("body-")}
    # Literal containment is a narrow text diagnostic, not counterpart proof or
    # source localization. No whitespace normalization is introduced by scoring.
    literal_hits = sum(c["operation"]["kind"] == "text_changed" and all(
        quotes[side] in (c["operation"][side] or "") for side in ("old", "new")) for c in legacy)
    coverage = report["coverage"]
    row.update(A=report["typed_changes"], B=report.get("scope_content_changes", 0),
               scope_C=report.get("inferred_scope_changes", 0), legacy_C=report["inferred_changes"],
               legacy_text_C=sum(c["operation"]["kind"] == "text_changed" for c in legacy),
               legacy_C_literal_target_containment=literal_hits, reviews=reviews, coverage=coverage,
               strict_nonlocal_events=report["typed_changes"] - len(events),
               stages={"acquisition": all(c["old_inventory_complete"] and c["new_inventory_complete"] for c in coverage),
                       "enumeration": all(s["candidates"]["exhaustive"] for s in scopes),
                       "conflict_search": all(s["matching"]["conflict_search_complete"] for s in scopes),
                       "optimization": all(s["matching"]["conflict_search_complete"] and all(
                           c["exhaustive"] for c in s["matching"]["components"]) for s in scopes),
                       "source_comparison": all(c["complete"] for c in coverage),
                       "overall": report["comparison_complete"]},
               compared_local_pairs=sum(c["compared"] for c in comparisons),
               selected_local_pairs=len(comparisons),
               text_search_complete=all(s["text_search"]["exhaustive"] for s in scopes),
               extraction_issues={side: dict(Counter(i["kind"] for i in report[side]["issues"])) for side in ("old", "new")},
               scope_unresolved_reasons=dict(Counter(reason for s in scopes for reason in s["unresolved"])))
    assert row["strict_nonlocal_events"] >= 0
    return events


def aggregate(rows):
    result = {"attempted_pairs": len(rows), "captured_pairs": sum(r["status"] == "captured" for r in rows),
              "annotation_resolved_pairs": sum(r["annotation_resolved"] for r in rows),
              "changed_scope_targets": len(rows),
              "B_exact_target_pairs": sum(r.get("B_exact_target", False) for r in rows),
              "B_contains_target_pairs": sum(r.get("B_contains_target", False) for r in rows),
              "C_contains_target_pairs": sum(r.get("C_contains_target", False) for r in rows),
              "wall_seconds_sum": sum(r.get("wall_seconds", 0) for r in rows),
              "peak_rss_kib_max": max((r.get("peak_rss_kib", 0) for r in rows), default=0),
              "report_bytes_sum": sum(r.get("report_bytes") or 0 for r in rows),
              "process_timeout_pairs": sum(r.get("exit_code") in (124, 137) for r in rows)}
    for key in ("A", "B", "scope_C", "legacy_C", "legacy_text_C", "legacy_C_literal_target_containment",
                "negative_atoms", "negative_strict_hits", "unscored_strict_events"):
        values = [r[key] for r in rows if r.get(key) is not None]
        result[key] = sum(values) if values else None
    result["stages"] = {stage: {"completed": sum(r.get("stages", {}).get(stage) is True for r in rows),
                                "observed": sum(r.get("stages", {}).get(stage) is not None for r in rows),
                                "attempted": len(rows)} for stage in (
                                    "acquisition", "enumeration", "conflict_search", "optimization", "source_comparison", "overall")}
    gold = [r for r in rows if r["strict_gold_events"]]
    result["strict_gold_events"] = len(gold)
    result["strict_gold_source_atoms"] = sum(r["strict_gold_atoms"] for r in gold)
    for unit in ("event", "source"):
        counts = {key: sum(r.get(f"strict_{unit}_{key}", 0) for r in gold) for key in ("tp", "fp", "fn")}
        result[f"strict_{unit}"] = dict(counts, precision=ratio(counts["tp"], counts["tp"] + counts["fp"]),
                                        recall=ratio(counts["tp"], counts["tp"] + counts["fn"]))
    for category in ("B", "C"):
        reviews = [v for r in rows for v in r.get("reviews", []) if v["category"] == category]
        touched = [v for v in reviews if v["target_intersection"]]
        result[f"{category}_quality"] = {"predictions": len(reviews), "touching_reference": len(touched),
                                          "unannotated": len(reviews) - len(touched),
                                          "range_precision_on_touched": ratio(sum(v["target_intersection"] for v in touched), sum(v["predicted_atoms"] for v in touched)),
                                          "extra_context_atoms_on_touched": sum(v["extra_context_atoms"] for v in touched)}
    native_gold = [r["native_C_numeric"] for r in rows if "native_C_numeric" in r]
    result["native_C_numeric"] = {"gold_events": len(native_gold), "gold_source_atoms": 2 * len(native_gold)}
    for unit in ("event", "source"):
        counts = {key: sum(r[unit + "_" + key] for r in native_gold) for key in ("tp", "fp", "fn")}
        result["native_C_numeric"][unit] = dict(counts, precision=ratio(counts["tp"], counts["tp"] + counts["fp"]),
                                                recall=ratio(counts["tp"], counts["tp"] + counts["fn"]))
    result["native_C_quality"] = {key: sum(r.get("native_candidate_diagnostics", {}).get(key, 0) for r in rows)
                                  for key in ("predictions", "touching_target", "touching_both_target_sides", "unchanged_control_atoms_claimed_conditionally")}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    freeze = read(ROOT / "annotation-freeze.json")
    assert digest(ROOT / "inputs.json") == freeze["inputs_sha256"]
    assert digest(ROOT / "freeze.json") == freeze["implementation_freeze_sha256"]
    implementation = read(ROOT / "freeze.json")
    registered = {p["id"]: p for p in read(ROOT / "inputs.json")["pairs"] if "replacement" not in p}
    expected = {p["id"]: p for p in freeze["pairs"]}
    assert set(registered) == set(expected) and len(expected) == 12
    rows = []
    args.output.mkdir(parents=True, exist_ok=False)
    for revision, directory in (("baseline", args.baseline), ("current", args.current)):
        ledger = read(directory / "runs.json")
        assert ledger["inputs_sha256"] == freeze["inputs_sha256"]
        assert ledger["binary_sha256"] == implementation["binaries"][revision]["sha256"]
        assert ledger["timeout_seconds"] == 180 and ledger["limit_scale"] == 1
        assert {(r["pair"], r["route"]) for r in ledger["runs"]} == {(p, route) for p in expected for route in ("native", "text", "all")}
        assert len(ledger["runs"]) == 36
        (args.output / f"{revision}-runs.json").write_text(json.dumps(ledger, indent=2) + "\n")
        for run in ledger["runs"]:
            id, route = run["pair"], run["route"]
            assert all(run[side + "_sha256"] == registered[id][side]["sha256"] for side in ("old", "new"))
            for suffix, field in (("expectations", "expectations_sha256"), ("resolved", "resolution_sha256"), ("", "annotation_sha256")):
                path = ROOT / "annotations" / f'{id}{"." + suffix if suffix else ""}.json'
                assert digest(path) == expected[id][field]
            assert run["reference_sha256"] == expected[id]["expectations_sha256"]
            reference = read(ROOT / "annotations" / f"{id}.expectations.json")
            resolved = read(ROOT / "annotations" / f"{id}.resolved.json")
            annotation = read(ROOT / "annotations" / f"{id}.json")
            target, negative, gold = reference_sets(reference, resolved)
            row = dict(run, revision=revision, family=registered[id]["family"], annotation_resolved=resolved["selector_resolution_complete"],
                       target_atoms=len(target), negative_atoms=len(negative), strict_gold_events=int(gold is not None),
                       strict_gold_atoms=0 if gold is None else len(gold))
            events = []
            if run["status"] == "captured":
                path = directory / f"{id}-{route}.json"
                assert digest(path) == run["report_sha256"]
                report = native_summary(path) if route == "native" else read(path)
                events = score_native(report, row, target, negative, gold) if route == "native" else score_common(report, row, target, annotation)
                del report
            strict = set().union(*(sources for _, sources in events))
            row["negative_strict_hits"] = len(strict & negative)
            for category in ("B", "C"):
                reviews = [v for v in row.get("reviews", []) if v["category"] == category]
                row[f"{category}_exact_target"] = any(v["range_exact"] for v in reviews)
                row[f"{category}_contains_target"] = any(v["contains_target"] for v in reviews)
            if gold is not None:
                scored = strict & target
                scored_events = [(kind, sources) for kind, sources in events if sources & target]
                matched = sum(kind == "replacement" and sources == gold for kind, sources in scored_events)
                row.update(strict_source_tp=len(scored & gold), strict_source_fp=len(scored - gold), strict_source_fn=len(gold - scored),
                           strict_event_tp=min(matched, 1), strict_event_fp=len(scored_events) - min(matched, 1),
                           strict_event_fn=int(not matched), unscored_strict_events=row.get("A", 0) - len(scored_events))
            else:
                row["unscored_strict_events"] = row.get("A")
            rows.append(row)
            print(revision, id, route, "scored", flush=True)
    (args.output / "pairs.json").write_text(json.dumps(rows, indent=2) + "\n")
    families = [{"revision": revision, "route": route, "family": family,
                 "metrics": aggregate([r for r in rows if r["revision"] == revision and r["route"] == route and (family == "all" or r["family"] == family)])}
                for revision in ("baseline", "current") for route in ("native", "text", "all")
                for family in ["all", *sorted({r["family"] for r in rows})]]
    (args.output / "families.json").write_text(json.dumps(families, indent=2) + "\n")
    sources = {"version": 1, "scorer_sha256": digest(Path(__file__)),
               "annotation_freeze_sha256": digest(ROOT / "annotation-freeze.json"),
               "results": {p.name: digest(p) for p in args.output.glob("*.json")},
               "strict_scope": "Only the frozen W-2 Copy A year is exact event/source gold. All other strict predictions are unscored.",
               "scope_scope": "One frozen partial target per pair; unavailable native atoms remain misses, not empty targets.",
               "source_unit": "Distinct (side, native glyph ID) atoms, not characters or changed ink.",
               "cost_scope": "One sequential process observation per revision/pair/route; peak RSS is not allocator live memory.",
               "zero_prediction_precision": None}
    (args.output / "sources.json").write_text(json.dumps(sources, indent=2) + "\n")


if __name__ == "__main__":
    main()
