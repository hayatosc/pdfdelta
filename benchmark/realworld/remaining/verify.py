#!/usr/bin/env python3
"""Verify the remaining recovery contract from hash-bound observations."""

import argparse
from collections import Counter
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys


DIRECTORY = Path(__file__).resolve().parent
ROOT = DIRECTORY.parents[2]
spec = importlib.util.spec_from_file_location(
    "historical_evidence", DIRECTORY.parent / "followup" / "verify.py")
historical = importlib.util.module_from_spec(spec)
spec.loader.exec_module(historical)
STAGES = ("registration", "diagnosis", "development", "blind-freeze", "blind", "final")


def read_reference(reference):
    return historical.read(historical.checked_path(reference))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def registration():
    record = historical.read(DIRECTORY / "registration.json")
    sources = {key: read_reference(value) for key, value in record["historical"].items()}
    require(record["completion_contract"] == {
        "channels": ["text"], "timeout_seconds": 180, "limit_scale": 1, "repetitions": 2,
    }, "registered completion contract changed")
    panel = historical.registration()
    require(panel == sources["panel"], "historical panel differs from the registered panel")
    historical.baseline_observations(panel)
    return record, sources


def complete(report, run):
    if not historical.common_complete(report, run):
        return False
    scopes = report["comparison"]["scopes"]
    require(bool(scopes), "complete report has no comparison scopes")
    for row in scopes:
        scope = row["result"]
        require(not scope["unresolved"] and scope["candidates"]["exhaustive"]
                and scope["text_search"]["exhaustive"]
                and scope["matching"]["conflict_search_complete"]
                and all(component["exhaustive"] for component in scope["matching"]["components"]),
                "complete report retains unresolved or unfinished search")
    return True


def observations(index, panel, binary, route="text"):
    """Validate repeats, identity, process costs and full common-text coverage."""
    require(route in ("text", "native", "all"), "unknown capture route")
    historical.checked_path(binary)
    pairs = historical.unique_by(panel["pairs"], "id")
    observed = {}
    attempts = set()
    for observation in index["observations"]:
        name, repetition = observation["pair"], observation["repetition"]
        key = name, repetition
        require(key not in observed and repetition in (1, 2), "duplicate/invalid repetition")
        pair = pairs[name]
        capture = read_reference(observation["capture"])
        attempt = observation["capture"]["sha256"], observation["run_index"]
        require(attempt not in attempts, "one captured attempt reused as a repetition")
        attempts.add(attempt)
        require(capture["binary_sha256"] == binary["sha256"]
                and capture["timeout_seconds"] == 180 and capture["limit_scale"] == 1,
                "capture executable or budget changed")
        run = capture["runs"][observation["run_index"]]
        require(run["pair"] == name and run["route"] == route, "capture identity mismatch")
        for side in ("old", "new"):
            require(run[side + "_sha256"] == pair[side]["sha256"], "capture input mismatch")
        require(run["wall_seconds"] >= 0 and run["peak_rss_kib"] >= 0,
                "missing or negative process cost")
        report = None
        path = None
        if run["status"] == "captured":
            require(run["exit_code"] in (0, 1, 3), "captured report has a failed exit status")
            path = historical.checked_path(observation["report"])
            require(run["report_sha256"] == observation["report"]["sha256"]
                    and run["report_bytes"] == path.stat().st_size, "capture report mismatch")
            report = historical.read(path)
            if route != "native":
                channels = ["text"] if route == "text" else ["text", "visual", "forms", "relations"]
                require(report["contract"] == {"version": 1, "channels": channels},
                        "report channel contract changed")
        else:
            require(run["status"] == "failed" and run.get("exit_code") not in (0, 1),
                    "missing report is not an explicit failed attempt")
        observed[key] = {"complete": route == "text" and report is not None and complete(report, run),
                         "run": run, "report_path": path, "reference": observation.get("report")}
    require(observed.keys() == {(name, repeat) for name in pairs for repeat in (1, 2)},
            "panel does not contain exactly two observations per pair")
    require(all(observed[name, 1]["complete"] == observed[name, 2]["complete"] for name in pairs),
            "completion differs between repetitions")
    return observed


def gate_summary(development_pairs, blind_pairs, development_producers, blind_producers,
                 baseline_complete, current_complete, correctness, evidence):
    """Reduce validated evidence only; raw result files are checked by each stage."""
    return {
        "G1": historical.recovery_gate(development_pairs, development_producers, 6, 3),
        "G2": historical.recovery_gate(blind_pairs, blind_producers, 3, 2),
        "G3": historical.completion_gate(baseline_complete, current_complete),
        "G4": {"passed": correctness},
        "G5": {"passed": evidence},
    }


def events(report):
    """Keep masks (A), finite review ranges (B), and inferred outputs separate."""
    if report is None:
        return []
    if "changes" in report and "comparison" not in report:
        rows = []
        for index, change in enumerate(report["changes"] + report.get("change_candidates", [])):
            sources = set()
            for occurrence in change["occurrences"]:
                for side in ("old", "new"):
                    span = occurrence.get(side + "_span")
                    if span:
                        sources.update((side, source["glyph_id"]) for source in span["sources"]
                                       if source["kind"] == "glyph")
            strict = index < len(report["changes"])
            require(not strict or sources, "native strict event lacks glyph sources")
            pointer = f"/changes/{index}" if strict else f"/change_candidates/{index - len(report['changes'])}"
            rows.append({"category": "A" if strict else "C", "sources": sources, "operation": change["kind"],
                         "source_projection": change, "pointer": pointer})
        # Legacy unresolved regions do not establish the correspondence required
        # by B. Retain them as non-recovery observations alongside candidates.
        for index, region in enumerate(report.get("proven_changed_regions", [])):
            sources = {(side, source["glyph_id"]) for side in ("old", "new")
                       for source in (region.get(side + "_span") or {}).get("sources", [])
                       if source["kind"] == "glyph"}
            rows.append({"category": "C", "sources": sources, "operation": region["proof"],
                         "source_projection": region, "pointer": f"/proven_changed_regions/{index}"})
        return rows
    rows = []
    for scope_index, scope in enumerate(report["comparison"]["scopes"]):
        prefix = f"/comparison/scopes/{scope_index}/result"
        result = scope["result"]
        for index, comparison in enumerate(result["comparisons"]):
            if comparison["operation"] is None:
                continue
            sources = set()
            mask = comparison["text_mask"]
            if mask is not None:
                sources = {atom for side in ("old", "new") for token in mask[side]
                           for atom in historical.native_sources(token["sources"], side)}
            category = "A" if comparison["interpretation"] == "conditional_on_correspondence" else "C"
            if category == "A":
                require(comparison["compared"] and not comparison["unresolved"] and sources,
                        "strict common-text operation lacks compared nonempty source masks")
            rows.append({"category": category, "sources": sources,
                         "operation": comparison["operation"],
                         "source_projection": mask,
                         "pointer": f"{prefix}/comparisons/{index}"})
        for index, review in enumerate(result.get("text_scope_reviews", [])):
            comparison = review["comparison"]
            if comparison["operation"] is None:
                continue
            category = "B" if comparison["interpretation"] == "conditional_on_correspondence" else "C"
            if category == "B":
                require(comparison["compared"] and not comparison["unresolved"],
                        "B operation retains unresolved comparison")
            sources = set().union(*(historical.native_sources(review[side + "_sources"], side)
                                    for side in ("old", "new")))
            rows.append({"category": category, "sources": sources,
                         "operation": comparison["operation"], "review": review,
                         "source_projection": {key: review[key] for key in (
                             "old_sources", "new_sources", "old_boundaries", "new_boundaries", "convention")},
                         "pointer": f"{prefix}/text_scope_reviews/{index}"})
    counts = Counter(row["category"] for row in rows)
    require(counts["A"] == report["typed_changes"],
            "strict report contains an unhandled nonlocal event; extend the source adapter")
    require(counts["B"] == report.get("scope_content_changes", 0), "B event denominator mismatch")
    return rows


def event_digest(event):
    # Include the complete operation and source multiplicity in identity. Pointer
    # movement alone is not a newly recovered event.
    payload = {"category": event["category"], "sources": sorted(event["sources"]),
               "operation": event["operation"], "source_projection": event["source_projection"]}
    return hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()


def pair_recovery(before, after, core, extent, controls, adjudications, strict_gold=None):
    previous, current = events(before), events(after)
    previous_counts = Counter(event_digest(event) for event in previous)
    reviews = historical.unique_by(adjudications, "pointer")
    expected_reviews = set()
    correct = True
    categories = Counter()
    for event in current:
        categories[event["category"]] += 1
        if event["category"] == "C":
            continue
        signature = event_digest(event)
        if previous_counts[signature]:
            previous_counts[signature] -= 1
            continue
        expected_reviews.add(event["pointer"])
        review = reviews.get(event["pointer"])
        correct &= bool(review and review["event_sha256"] == signature
                        and review["verdict"] == "source_supported"
                        and review["source_content_rationale"] and review["correspondence_rationale"])
    require(reviews.keys() <= expected_reviews, "adjudication points to a non-new or absent A/B event")
    correct &= reviews.keys() == expected_reviews
    strict_atoms = set().union(*(event["sources"] for event in current if event["category"] == "A"))
    false_control_atoms = len(strict_atoms & controls)
    correct &= false_control_atoms == 0
    if strict_gold is not None:
        correct &= all(event["sources"] == strict_gold["sources"]
                       and event["operation"] == strict_gold["operation"]
                       for event in current if event["category"] == "A" and event["sources"] & extent)

    def hits(rows):
        found = set()
        for event in rows:
            if event["category"] == "B" and historical.range_recovery(
                    event["review"], core, extent)["source_range_hit"]:
                found.add("B")
            if (event["category"] == "A" and strict_gold is not None
                    and event["sources"] == strict_gold["sources"]
                    and event["operation"] == strict_gold["operation"]):
                found.add("A")
        return found

    old_hits, new_hits = hits(previous), hits(current)
    # Recovery is additional only if neither accepted route already hit the target.
    recovered = sorted(new_hits) if correct and not old_hits else []
    result = {"additional_categories": recovered, "correct": correct,
            "A": categories["A"], "B": categories["B"], "C": categories["C"],
            "new_AB_outputs": len(expected_reviews), "adjudicated_AB_outputs": len(reviews),
            "strict_control_atoms_claimed": false_control_atoms,
            "strict_event_precision": None, "strict_event_recall": None,
            "strict_source_precision": None, "strict_source_recall": None}
    if strict_gold is not None:
        require(strict_gold["sources"] and strict_gold["sources"] <= extent,
                "strict gold is empty or outside the frozen finite extent")
        scored = [event for event in current if event["category"] == "A" and event["sources"] & extent]
        hits = sum(event["sources"] == strict_gold["sources"]
                   and event["operation"] == strict_gold["operation"] for event in scored)
        predicted = set().union(*(event["sources"] for event in scored))
        result.update(strict_event_precision=min(hits, 1) / len(scored) if scored else None,
                      strict_event_recall=int(bool(hits)),
                      strict_source_precision=len(predicted & strict_gold["sources"]) / len(predicted)
                      if predicted else None,
                      strict_source_recall=len(predicted & strict_gold["sources"]) / len(strict_gold["sources"]))
    return result


def source_fingerprint(include_bench=False):
    crates = ("pdfdelta-core", "pdfdelta-cli", "pdfdelta-bench") if include_bench else ("pdfdelta-core", "pdfdelta-cli")
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
    for crate in crates:
        directory = ROOT / "crates" / crate
        paths.extend(directory.rglob("*.rs"))
        paths.append(directory / "Cargo.toml")
    digest = hashlib.sha256()
    for path in sorted(paths):
        content = path.read_bytes()
        digest.update(str(path.relative_to(ROOT)).encode() + b"\0")
        digest.update(str(len(content)).encode() + b"\0" + content)
    return digest.hexdigest()


def phase(record, panel, targets, baseline_index, baseline_binary, route="text"):
    require(record["production_sha256"] == source_fingerprint(), "phase production source is stale")
    build = read_reference(record["build"])
    require(build["production_sha256"] == record["production_sha256"]
            and build["binary"] == record["binary"] and build["exit_code"] == 0
            and build["command"] == ["cargo", "build", "--release", "-p", "pdfdelta-cli", "--locked"],
            "phase executable build is stale or failed")
    historical.checked_path(build["log"])
    baseline = observations(baseline_index, panel, baseline_binary, route)
    current = observations(read_reference(record["observations"]), panel, record["binary"], route)
    adjudication = read_reference(record["adjudications"])
    adjudicated = {}
    for row in adjudication["observations"]:
        key = row["pair"], row["repetition"]
        require(key not in adjudicated and key in current, "duplicate/unknown adjudicated observation")
        require(row["report"] == current[key]["reference"], "adjudication belongs to another report")
        adjudicated[key] = row
    target_by_pair = historical.unique_by(targets["targets"], "pair")
    rows = []
    for pair in panel["pairs"]:
        target = target_by_pair[pair["id"]]
        core, extent, controls = historical.target_sources(target)
        scores = []
        for repetition in (1, 2):
            key = pair["id"], repetition
            before, after = baseline[key], current[key]
            review = adjudicated.get(key, {"events": []})
            for event in review["events"]:
                references = event["source_evidence"]
                require(target["references"]["annotation"] in references
                        and target["references"]["resolution"] in references,
                        "source review is not bound to the frozen annotation and resolution")
                for reference in references:
                    historical.checked_path(reference)
            gold = None
            if target["strict_event_gold"] is not None or target["strict_changed_position_gold"] is not None:
                require(target["strict_event_gold"] is not None and target["strict_changed_position_gold"] is not None,
                        "strict gold requires both event and changed-source annotations")
                gold = {"operation": target["strict_event_gold"],
                        "sources": {tuple(atom) for atom in target["strict_changed_position_gold"]}}
            score = pair_recovery(
                historical.read(before["report_path"]) if before["report_path"] else None,
                historical.read(after["report_path"]) if after["report_path"] else None,
                core, extent, controls, review["events"], gold)
            if not target["body_eligible"] or not target["source_resolution_complete"]:
                score["additional_categories"] = []
            scores.append(score)
        require(scores[0] == scores[1], f"recovery or correctness is not repeatable: {pair['id']}")
        rows.append(dict(scores[0], pair=pair["id"], family=pair["family"],
                         producer=target["independent_producer"],
                         baseline_complete=baseline[pair["id"], 1]["complete"],
                         current_complete=current[pair["id"], 1]["complete"],
                         costs=[{field: current[pair["id"], repeat]["run"].get(field)
                                 for field in ("wall_seconds", "peak_rss_kib", "report_bytes", "exit_code")}
                                for repeat in (1, 2)]))
    return rows


def family_metrics(rows):
    result = {}
    for family in sorted({row["family"] for row in rows}):
        selected = [row for row in rows if row["family"] == family]
        result[family] = {
            "attempted_pairs": len(selected),
            "additional_A_pairs": sum("A" in row["additional_categories"] for row in selected),
            "additional_B_pairs": sum("B" in row["additional_categories"] for row in selected),
            "C_outputs": sum(row["C"] for row in selected),
            "complete_pairs": sum(row["current_complete"] for row in selected),
            "new_AB_outputs": sum(row["new_AB_outputs"] for row in selected),
            "adjudicated_AB_outputs": sum(row["adjudicated_AB_outputs"] for row in selected),
            "wall_seconds": sum(cost["wall_seconds"] for row in selected for cost in row["costs"]),
            "peak_rss_kib_max": max(cost["peak_rss_kib"] for row in selected for cost in row["costs"]),
            "report_bytes": sum(cost["report_bytes"] or 0 for row in selected for cost in row["costs"]),
            "pair_metrics": [{key: row[key] for key in ("pair", "strict_event_precision", "strict_event_recall",
                                                       "strict_source_precision", "strict_source_recall")}
                             for row in selected],
        }
    return result


def controls(record, registered, binary):
    """Score the immutable 60 generated and three real-source controls on all routes."""
    for reference in registered["references"]:
        historical.checked_path(reference)
    directory = DIRECTORY.parent / "next" / "layout-controls"
    authored = historical.unique_by(historical.read(directory / "manifest.json")["generated_pairs"], "id")
    expected = historical.unique_by(historical.read(directory / "expectations.json")["pairs"], "pair")
    real = historical.unique_by(historical.read(directory / "source-mutation-expectations.json")["pairs"], "id")
    require(len(authored) == 60 and len(real) == 3 and not authored.keys() & real.keys(),
            "control denominator changed")
    expected.update(real)
    pairs = authored | real
    capture = read_reference(record["capture"])
    require(capture["binary_sha256"] == binary["sha256"] and capture["timeout_seconds"] == 180
            and capture["limit_scale"] == 1, "control executable or budget mismatch")
    require(set(capture["reference_hashes"]) == {
        "manifest.json", "expectations.json", "source-mutation-expectations.json"},
        "control capture omits frozen references")
    for name, digest in capture["reference_hashes"].items():
        require(hashlib.sha256((directory / name).read_bytes()).hexdigest() == digest,
                "control capture reference changed")
    reports = {}
    for report in record["reports"]:
        key = report["pair"], report["route"]
        require(key not in reports, "duplicate control report")
        reports[key] = report["report"]
    adjudicated = {}
    for review in record.get("adjudications", []):
        key = review["pair"], review["route"], review["pointer"]
        require(key not in adjudicated, "duplicate control adjudication")
        require(review["report"] == reports[key[:2]], "control adjudication report changed")
        require(review["verdict"] == "source_supported" and review["source_content_rationale"]
                and review["correspondence_rationale"] and review["source_evidence"],
                "control adjudication lacks source support")
        for reference in review["source_evidence"]:
            historical.checked_path(reference)
        required_paths = {str((directory / "annotations" / f"{review['pair']}{suffix}").relative_to(ROOT))
                          for suffix in (".json", ".resolved.json")}
        require(required_paths <= {reference["path"] for reference in review["source_evidence"]},
                "control review omits the original annotation or source resolution")
        adjudicated[key] = review
    rows, seen = [], set()
    for run in capture["runs"]:
        name, route = key = run["pair"], run["route"]
        require(key not in seen and name in pairs and route in ("native", "text", "all"),
                "duplicate or unknown control run")
        seen.add(key)
        require(all(run[side + "_sha256"] == pairs[name][side]["sha256"] for side in ("old", "new")),
                "control source bytes changed")
        row = {"pair": name, "route": route, "status": run["status"], "correct": False}
        require(run["wall_seconds"] >= 0 and run["peak_rss_kib"] >= 0, "missing control process costs")
        row["costs"] = {field: run[field] for field in
                        ("wall_seconds", "peak_rss_kib", "report_bytes", "exit_code")}
        if run["status"] != "captured":
            require(run["status"] == "failed" and run["exit_code"] not in (0, 1),
                    "control is not an explicit failed attempt")
            rows.append(row)
            continue
        reference = reports[key]
        path = historical.checked_path(reference)
        require(reference["sha256"] == run["report_sha256"] and path.stat().st_size == run["report_bytes"],
                "control report differs from capture")
        require(run["exit_code"] in (0, 1, 3), "control report comes from a failed process")
        target, unchanged = set(), set()
        resolved = historical.read(directory / "annotations" / f"{name}.resolved.json")
        require(resolved["selector_resolution_complete"], "unresolved control annotation")
        for selector in resolved["selectors"]:
            if name in authored:
                side, _, paragraph, _, _ = selector["id"].split("-")
                changed = int(paragraph) == expected[name]["changed_paragraph"]
            else:
                side = selector["id"].rsplit("-", 1)[1]
                changed = selector["id"].startswith("body-")
            (target if changed else unchanged).update(
                (side, atom["id"]) for atoms in historical.source_rows(selector)
                for atom in atoms if atom["kind"] == "glyph")
        report = historical.read(path)
        if route != "native":
            channels = ["text"] if route == "text" else ["text", "visual", "forms", "relations"]
            require(report["contract"] == {"version": 1, "channels": channels},
                    "control report channel contract changed")
        detected = events(report)
        strict = set().union(*(event["sources"] for event in detected if event["category"] == "A"))
        gold = expected[name]["strict_source_atoms"]
        gold = None if gold is None else {(item["side"], atom["id"]) for item in gold for atom in item["atoms"]}
        false_masks = len(strict & unchanged)
        unproved_strict = False
        for event in detected:
            if event["category"] != "A" or (gold is not None and event["sources"] <= gold):
                continue
            if gold is not None:
                unproved_strict = True
                continue
            review = adjudicated.pop((name, route, event["pointer"]), None)
            unproved_strict |= not review or review["event_sha256"] != event_digest(event)
        bad_ranges = sum(event["sources"] != target or not target
                         for event in detected if event["category"] == "B")
        row.update(correct=not false_masks and not unproved_strict and not bad_ranges,
                   false_strict_control_atoms=false_masks, unproved_strict=unproved_strict,
                   unsupported_B_ranges=bad_ranges, **Counter(event["category"] for event in detected))
        rows.append(row)
    require(seen == {(name, route) for name in pairs for route in ("native", "text", "all")},
            "control capture does not contain all 189 attempts")
    require(reports.keys() == {(row["pair"], row["route"]) for row in rows if row["status"] == "captured"},
            "missing or unused control reports")
    require(not adjudicated, "unused control adjudication")
    return {"attempts": len(rows), "correct": all(row["correct"] for row in rows), "rows": rows}


def blind_freeze(development, sources):
    freeze = historical.read(DIRECTORY / "blind-freeze.json")
    require(freeze["production_sha256"] == source_fingerprint()
            and freeze["binary"] == development["binary"], "blind executable/source freeze changed")
    historical.checked_path(freeze["binary"])
    panel = read_reference(freeze["panel"])
    targets = read_reference(freeze["targets"])
    pairs = historical.unique_by(panel["pairs"], "id")
    target_rows = historical.unique_by(targets["targets"], "pair")
    require(len(pairs) == 12 and pairs.keys() == target_rows.keys(), "blind denominator must be exactly 12")
    require(len({row["family"] for row in pairs.values()}) >= 6
            and {"en", "ja"} <= {row["language"] for row in pairs.values()}
            and len({row["independent_producer"] for row in target_rows.values()}) >= 3,
            "blind families, languages or producers missing")
    exposed = {pair["id"] for pair in sources["panel"]["pairs"]}
    exposed_hashes = {pair[side]["sha256"] for pair in sources["panel"]["pairs"] for side in ("old", "new")}
    excluded = historical.read(DIRECTORY.parent / "next" / "blind" / "excluded-series.json")["series"]
    for filename in ("selection.json", "replacement-selection.json"):
        excluded += historical.read(DIRECTORY.parent / "next" / "blind" / filename)["pairs"]
    exposed.update(row.get("pair", row.get("id")) for row in excluded)
    excluded_series = {row["series"].casefold().strip() for row in excluded}
    series = set()
    for name, pair in pairs.items():
        canonical_series = pair["series"].casefold().strip()
        require(name not in exposed and canonical_series not in series | excluded_series,
                "exposed or duplicate blind series")
        series.add(canonical_series)
        require(pair["novelty_review"] and pair["primary_source_evidence"], "missing series novelty evidence")
        for reference in pair["primary_source_evidence"]:
            historical.checked_path(reference)
        for side in ("old", "new"):
            historical.checked_path(pair[side])
            require(pair[side]["sha256"] not in exposed_hashes, "blind input bytes were already exposed")
        target = target_rows[name]
        annotation = read_reference(target["references"]["annotation"])
        for side in ("old", "new"):
            require(annotation[side + "_sha256"] == pair[side]["sha256"], "blind annotation input mismatch")
        core, extent, _ = historical.target_sources(target)
        require(core and core <= extent and target["source_resolution_complete"], "blind source target unresolved")
    # A committed pre-comparison manifest binds the entire target registration.
    # Novelty reviews additionally identify series, since new URLs alone do not.
    for reference in (freeze["panel"], freeze["targets"]):
        content = subprocess.check_output(["git", "show", f"{freeze['registration_commit']}:{reference['path']}"], cwd=ROOT)
        require(hashlib.sha256(content).hexdigest() == reference["sha256"], "blind registration commit mismatch")
    require(freeze["freeze_utc"] < freeze["selection_started_utc"] <= freeze["annotation_completed_utc"],
            "blind selection/annotation chronology is invalid")
    return freeze, panel, targets


def quality():
    record = historical.read(DIRECTORY / "quality-checks.json")
    require(record["source_sha256"] == source_fingerprint(include_bench=True), "quality checks are stale")
    required = {
        ("cargo", "fmt", "--all", "--", "--check"),
        ("cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"),
        ("cargo", "test", "--workspace", "--locked"),
        ("cargo", "run", "-p", "pdfdelta-bench", "--locked", "--", "verify"),
    }
    passed = set()
    for check in record["checks"]:
        historical.checked_path(check["log"])
        if check["exit_code"] == 0:
            passed.add(tuple(check["command"]))
    require(required <= passed, "mandatory workspace/generated verification is missing or failed")
    return True


def diagnosis(sources):
    data = historical.read(DIRECTORY / "diagnosis.json")
    require(data["registration"] == {
        "path": str((DIRECTORY / "registration.json").relative_to(ROOT)),
        "sha256": hashlib.sha256((DIRECTORY / "registration.json").read_bytes()).hexdigest(),
    }, "diagnosis registration is stale")
    rows = historical.unique_by(data["targets"], "pair")
    require(rows.keys() == {pair["id"] for pair in sources["panel"]["pairs"]},
            "diagnosis omits registered pairs")
    for row in rows.values():
        require(row["stage"] in ("acquisition", "normalization", "scope", "retrieval",
                                 "optimization", "counterpart", "localization", "reporting"),
                "unknown earliest blocker stage")
        require(row["reason"] and row["next_action"] and row["evidence"],
                "diagnosis lacks an observation or next action")
        for reference in row["evidence"]:
            historical.checked_path(reference)
    return data


def missing_gates():
    return gate_summary([], [], {}, {}, set(), set(), False, False)


def recovery_summary(rows):
    recovered = {row["pair"]: row["producer"] for row in rows if row["additional_categories"]}
    return recovered, {row["pair"] for row in rows if row["baseline_complete"]}, {
        row["pair"] for row in rows if row["current_complete"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", choices=STAGES, required=True)
    args = parser.parse_args()
    gates = missing_gates()
    try:
        _, sources = registration()
        if args.stage == "registration":
            return 0
        diagnosis(sources)
        if args.stage == "diagnosis":
            return 0
        development = historical.read(DIRECTORY / "development.json")
        rows = phase(development, sources["panel"], sources["targets"],
                     sources["baseline-observations"], sources["baseline"]["binary"])
        recovered, before, after = recovery_summary(rows)
        gates = gate_summary(recovered, [], recovered, {}, before, after, False, False)
        print(json.dumps({"development": family_metrics(rows)}, indent=2))
        control_results = controls(read_reference(development["controls"]), sources["controls"],
                                   development["binary"])
        print(json.dumps({"controls": control_results}, indent=2))
        gates["G4"]["passed"] = control_results["correct"] and all(row["correct"] for row in rows) and quality()
        require(all(gates[key]["passed"] for key in ("G1", "G3", "G4")),
                "development recovery, completion or correctness gate unmet")
        if args.stage == "development":
            print(json.dumps(gates, indent=2))
            return 0
        freeze, panel, targets = blind_freeze(development, sources)
        if args.stage == "blind-freeze":
            print(json.dumps(gates, indent=2))
            return 0
        blind = historical.read(DIRECTORY / "blind.json")
        require(blind["binary"] == freeze["binary"], "blind executable differs from freeze")
        baseline = read_reference(freeze["baseline_observations"])
        blind_rows = phase(blind, panel, targets, baseline, sources["baseline"]["binary"])
        blind_recovered, _, _ = recovery_summary(blind_rows)
        gates["G2"] = historical.recovery_gate(blind_recovered, blind_recovered, 3, 2)
        gates["G4"]["passed"] &= all(row["correct"] for row in blind_rows)
        print(json.dumps({"blind": family_metrics(blind_rows)}, indent=2))
        require(gates["G2"]["passed"] and gates["G4"]["passed"], "blind recovery or correctness gate unmet")
        gates["G5"]["passed"] = True
        print(json.dumps(gates, indent=2))
        return 0
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print(json.dumps(gates, indent=2))
        print(f"FAIL {args.stage}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
