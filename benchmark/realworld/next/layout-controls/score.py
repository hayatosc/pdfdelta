#!/usr/bin/env python3
"""Score the fixed layout experiment; reject unhandled result shapes."""

import argparse
import hashlib
import json
from pathlib import Path


def read(path):
    return json.loads(path.read_text())


def atoms(rows, side):
    return {(side, atom["id"]) for row in rows for atom in row[2] if atom["kind"] == "glyph"}


def source_set(sources, side):
    return {(side, source["glyph"]) for source in sources if source["origin"] == "native"}


def ratio(numerator, denominator):
    return numerator / denominator if denominator else None


def existing_contract(value):
    if isinstance(value, dict):
        return {key: existing_contract(item) for key, item in value.items() if key not in (
            "comparison_wall_time_ms", "scope_content_changes", "inferred_scope_changes",
            "text_scope_reviews", "index_entries", "token_visits",
        )}
    if isinstance(value, list):
        return [existing_contract(item) for item in value]
    return value


def aggregate(rows):
    gold = [row for row in rows if row.get("strict_gold_atoms") is not None]
    reviews = [review for row in rows for review in row.get("scope_adjudications", [])]
    result = {
        "attempted_pairs": len(rows),
        "captured_pairs": sum(row["status"] == "captured" for row in rows),
        "complete_pairs": sum(row.get("complete", False) for row in rows),
        "changed_scope_denominator": sum(row["changed_scope_denominator"] for row in rows),
        "strict_gold_events": len(gold),
        "strict_gold_atoms": sum(row["strict_gold_atoms"] for row in gold),
        "B_exact_range_detected": sum(row.get("B_exact_range_detected", False) for row in rows),
        "scope_review_count": len(reviews),
        "scope_range_precision": ratio(sum(r["source_intersection"] for r in reviews), sum(r["predicted_atoms"] for r in reviews)),
        "extra_scope_context_atoms": sum(r["extra_context_atoms"] for r in reviews),
    }
    for field in ("A", "B", "scope_C", "legacy_C", "legacy_text_C", "legacy_text_C_matching_content_target",
                  "negative_source_atoms", "negative_false_strict_atoms", "strict_event_tp", "strict_event_fp",
                  "strict_event_fn", "strict_source_tp", "strict_source_fp", "strict_source_fn"):
        values = [row[field] for row in rows if row.get(field) is not None]
        result[field] = sum(values) if values else None
    for stage in ("acquisition_complete", "enumeration_complete", "optimization_complete"):
        observed = [row[stage] for row in rows if stage in row]
        result[stage] = {"completed": sum(observed), "observed_pairs": len(observed)}
    for unit in ("event", "source"):
        tp, fp, fn = [sum(row.get(f"strict_{unit}_{suffix}", 0) for row in gold) for suffix in ("tp", "fp", "fn")]
        result[f"strict_{unit}_precision"] = ratio(tp, tp + fp)
        result[f"strict_{unit}_recall"] = ratio(tp, tp + fn)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("current", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent
    authored = {p["id"]: p for p in read(root / "manifest.json")["generated_pairs"]}
    references = {p["pair"]: p for p in read(root / "expectations.json")["pairs"]}
    real = {p["id"]: p for p in read(root / "source-mutation-expectations.json")["pairs"]}
    references.update(real)
    args.output.mkdir(parents=True, exist_ok=False)
    scores, contracts = [], []
    previous = {}
    for revision, directory in (("baseline", args.baseline), ("current", args.current)):
        runs = read(directory / "runs.json")
        (args.output / f"{revision}-runs.json").write_text(json.dumps(runs, indent=2) + "\n")
        for run in runs["runs"]:
            id, route = run["pair"], run["route"]
            reference = references[id]
            resolved = read(root / "annotations" / f"{id}.resolved.json")
            target, unchanged = set(), set()
            for selector in resolved["selectors"]:
                if id in authored:
                    side, _, paragraph, _, _ = selector["id"].split("-")
                    changed = int(paragraph) == reference["changed_paragraph"]
                else:
                    side = selector["id"].rsplit("-", 1)[1]
                    changed = selector["id"].startswith("body-")
                (target if changed else unchanged).update(atoms(selector["source_rows"], side))
            row = {"revision": revision, "pair": id, "route": route, "status": run["status"],
                   "population": "authored" if id in authored else "real_metamorphic",
                   "content": authored[id]["content_mutation"] if id in authored else "number",
                   "presentation": authored[id]["presentation_mutation"] if id in authored else real[id]["kind"],
                   "changed_scope_denominator": int(bool(target)),
                   "negative_source_atoms": len(unchanged)}
            if run["status"] != "captured":
                scores.append(row)
                continue
            path = directory / f"{id}-{route}.json"
            assert hashlib.sha256(path.read_bytes()).hexdigest() == run["report_sha256"]
            report = read(path)
            normalized = existing_contract(report)
            digest = hashlib.sha256(json.dumps(normalized, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
            if revision == "baseline":
                previous[id, route] = digest
            else:
                contracts.append({"pair": id, "route": route,
                                  "normalized_existing_contract_equal": previous[id, route] == digest,
                                  "baseline_sha256": previous[id, route], "current_sha256": digest})
            gold = reference["strict_source_atoms"]
            gold = None if gold is None else {(x["side"], a["id"]) for x in gold for a in x["atoms"]}
            strict, events = set(), []
            if route == "native":
                row.update(complete=report["summary"]["comparison_complete"],
                           A=len(report["changes"]), B=None, scope_C=None,
                           legacy_regions=len(report["proven_changed_regions"]),
                           acquisition_complete=not report["extraction"]["issues"],
                           summary=report["summary"])
                for event in report["changes"]:
                    sources = set()
                    for occurrence in event["occurrences"]:
                        for side in ("old", "new"):
                            span = occurrence.get(side + "_span")
                            if span:
                                sources.update((side, s["glyph_id"]) for s in span["sources"] if s["kind"] == "glyph")
                    events.append((event["kind"], sources))
                    strict.update(sources)
            else:
                # This experiment has no typed changes; new shapes require explicit scoring.
                assert report["typed_changes"] == 0
                scopes = [s["result"] for s in report["comparison"]["scopes"]]
                reviews = [r for s in scopes for r in s.get("text_scope_reviews", [])]
                adjudications = []
                for review in reviews:
                    sources = source_set(review["old_sources"], "old") | source_set(review["new_sources"], "new")
                    adjudications.append({
                        "interpretation": review["comparison"]["interpretation"],
                        "range_exact": bool(target) and sources == target,
                        "source_intersection": len(sources & target),
                        "source_range_precision": ratio(len(sources & target), len(sources)),
                        "source_range_recall": ratio(len(sources & target), len(target)),
                        "extra_context_atoms": len(sources - target),
                        "predicted_atoms": len(sources),
                    })
                legacy = [c for s in scopes for c in s["comparisons"] if c["operation"] is not None and c["interpretation"] == "inferred"]
                legacy_correct = None
                if id in authored:
                    legacy_correct = 0
                    paragraph = reference["changed_paragraph"]
                    for candidate in legacy:
                        operation = candidate["operation"]
                        if operation["kind"] != "text_changed":
                            continue
                        if paragraph is not None and all(
                            operation[side].replace(" ", "") == authored[id][side + "_paragraphs"][paragraph].replace(" ", "")
                            for side in ("old", "new")
                        ):
                            legacy_correct += 1
                row.update(complete=report["comparison_complete"], A=0,
                           B=report.get("scope_content_changes", 0), scope_C=report.get("inferred_scope_changes", 0),
                           legacy_C=report["inferred_changes"], legacy_text_C=len(legacy),
                           legacy_text_C_matching_content_target=legacy_correct,
                           scope_adjudications=adjudications,
                           B_exact_range_detected=any(x["range_exact"] and x["interpretation"] == "conditional_on_correspondence" for x in adjudications),
                           enumeration_complete=all(s["candidates"]["exhaustive"] for s in scopes),
                           optimization_complete=all(c["exhaustive"] for s in scopes for c in s["matching"]["components"]),
                           coverage=report["coverage"],
                           acquisition_complete=all(c["old_inventory_complete"] and c["new_inventory_complete"] for c in report["coverage"]),
                           compared_pairs=sum(c["compared"] for s in scopes for c in s["comparisons"]))
            row.update(negative_false_strict_atoms=len(strict & unchanged), strict_gold_atoms=None if gold is None else len(gold))
            if gold is not None:
                # Real documents have partial gold: unrelated strict events remain unscored.
                scored = strict if id in authored else strict & target
                scored_events = events if id in authored else [(k, s) for k, s in events if s & target]
                matched = sum(k == "replacement" and s == gold for k, s in scored_events)
                row.update(strict_source_tp=len(scored & gold), strict_source_fp=len(scored - gold),
                           strict_source_fn=len(gold - scored), strict_event_tp=matched,
                           strict_event_fp=len(scored_events) - matched, strict_event_fn=1 - min(matched, 1),
                           strict_source_precision=ratio(len(scored & gold), len(scored)),
                           strict_source_recall=ratio(len(scored & gold), len(gold)),
                           strict_event_precision=ratio(matched, len(scored_events)),
                           strict_event_recall=min(matched, 1),
                           unscored_strict_events=len(events) - len(scored_events))
            scores.append(row)
    (args.output / "scores.json").write_text(json.dumps(scores, indent=2) + "\n")
    (args.output / "contracts.json").write_text(json.dumps(contracts, indent=2) + "\n")
    summary = []
    for revision in ("baseline", "current"):
        for population in ("authored", "real_metamorphic"):
            for route in ("native", "text", "all"):
                rows = [r for r in scores if r["revision"] == revision and r["population"] == population and r["route"] == route]
                summary.append({"revision": revision, "population": population, "route": route,
                                "total": aggregate(rows),
                                "by_presentation": {p: aggregate([r for r in rows if r["presentation"] == p]) for p in sorted({r["presentation"] for r in rows})}})
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()
