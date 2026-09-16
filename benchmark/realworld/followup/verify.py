#!/usr/bin/env python3
"""Check retained follow-up evidence; missing stages never imply success."""

import argparse
import hashlib
import json
import mmap
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[3]
DIRECTORY = Path(__file__).resolve().parent
STAGES = ("registration", "diagnosis", "development", "blind-freeze", "blind", "final")


def read(path):
    return json.loads(path.read_text())


def checked_path(reference):
    path = ROOT / reference["path"]
    with path.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != reference["sha256"]:
        raise ValueError(f"stale evidence: {path}")
    return path


def source_rows(selector):
    if "sources" in selector:
        return [row["atoms"] for row in selector["sources"]]
    return [row[2] for row in selector.get("source_rows", [])]


def unique_by(rows, key):
    result = {row[key]: row for row in rows}
    if len(result) != len(rows):
        raise ValueError(f"duplicate {key} entries")
    return result


def target_sources(target):
    annotation = read(checked_path(target["references"]["annotation"]))
    resolution = read(checked_path(target["references"]["resolution"]))
    selectors = unique_by(annotation["selectors"], "id")
    resolved = unique_by(resolution.get("selectors", []), "id")

    def atoms(ids):
        return {(selectors[name]["side"], atom["id"])
                for name in ids for row in source_rows(resolved.get(name, {}))
                for atom in row if atom["kind"] == "glyph"}

    core = atoms([name for names in target["core_selectors"].values() for name in names])
    extent = atoms([name for names in target["permissible_extent_selectors"].values() for name in names])
    controls = atoms(target["unchanged_control_selectors"])
    return core, extent, controls


def native_sources(sources, side):
    return {(side, source["glyph"]) for source in sources if source["origin"] == "native"}


def range_recovery(review, core, extent):
    comparison = review["comparison"]
    sources = set().union(*(native_sources(review[side + "_sources"], side)
                            for side in ("old", "new")))
    eligible = (comparison["interpretation"] == "conditional_on_correspondence"
                and comparison["compared"] is True and not comparison["unresolved"]
                and comparison["operation"] is not None
                and comparison["operation"]["kind"] == "text_changed")
    both_cores = all(any(atom[0] == side for atom in core) for side in ("old", "new"))
    return {"source_range_hit": eligible and both_cores and core <= sources <= extent,
            "category": "B" if comparison["interpretation"] == "conditional_on_correspondence" else "C",
            "predicted_atoms": len(sources), "core_intersection": len(core & sources),
            "extra_context_atoms": len(sources - extent)}


def common_target_score(report, target):
    core, extent, controls = target_sources(target)
    reviews = []
    strict_sources = set()
    for scope_index, scope in enumerate(report["comparison"]["scopes"]):
        for review_index, review in enumerate(scope["result"].get("text_scope_reviews", [])):
            score = range_recovery(review, core, extent)
            score["pointer"] = f"/comparison/scopes/{scope_index}/result/text_scope_reviews/{review_index}"
            reviews.append(score)
        for comparison in scope["result"]["comparisons"]:
            if (comparison["interpretation"] == "conditional_on_correspondence"
                    and comparison["operation"] is not None and comparison["text_mask"] is not None):
                for side in ("old", "new"):
                    for token in comparison["text_mask"][side]:
                        strict_sources.update(native_sources(token["sources"], side))
    # A range hit still needs a source-content and correspondence adjudication.
    return {"pair": target["pair"], "core_atoms": len(core), "extent_atoms": len(extent),
            "strict_control_atoms_claimed": len(strict_sources & controls),
            "B_source_range_hits": sum(review["source_range_hit"] for review in reviews),
            "strict_event_recall": None, "strict_source_recall": None,
            "reviews": reviews}


def recovery_gate(pair_ids, producers, minimum_pairs, minimum_producers):
    pairs = set(pair_ids)
    publishers = {producers[name] for name in pairs}
    return {"observed_pairs": len(pairs), "required_pairs": minimum_pairs,
            "observed_producers": len(publishers), "required_producers": minimum_producers,
            "passed": len(pairs) >= minimum_pairs and len(publishers) >= minimum_producers}


def completion_gate(baseline_complete, current_complete):
    gained = set(current_complete) - set(baseline_complete)
    lost = set(baseline_complete) - set(current_complete)
    return {"gained_pairs": sorted(gained), "lost_pairs": sorted(lost),
            "required_gain": 2, "allowed_losses": 0, "passed": len(gained) >= 2 and not lost}


def registration():
    panel = read(DIRECTORY / "panel.json")
    baseline_path = DIRECTORY / "baseline.json"
    checked_path({"path": str(baseline_path.relative_to(ROOT)),
                  "sha256": panel["baseline_sha256"]})
    baseline = read(baseline_path)
    checked_path(baseline["binary"])
    targets = read(checked_path(panel["targets"]))
    pairs = unique_by(panel["pairs"], "id")
    references = unique_by(targets["targets"], "pair")
    if pairs.keys() != references.keys() or len(pairs) < 12:
        raise ValueError("panel/target denominator mismatch or fewer than 12 pairs")
    if len({p["family"] for p in pairs.values()}) < 6:
        raise ValueError("fewer than six panel families")
    if not {"en", "ja"} <= {p["language"] for p in pairs.values()}:
        raise ValueError("missing English/Japanese panel representation")
    eligible = []
    for name, pair in pairs.items():
        for side in ("old", "new"):
            checked_path(pair[side])
        target = references[name]
        files = {key: read(checked_path(value))
                 for key, value in target["references"].items()}
        annotation = files["annotation"]
        selectors = unique_by(annotation["selectors"], "id")
        resolved = unique_by(files["resolution"].get("selectors", []), "id")
        for side in ("old", "new"):
            if annotation[side + "_sha256"] != pair[side]["sha256"]:
                raise ValueError(f"annotation input mismatch: {name}/{side}")
            core = target["core_selectors"].get(side, [])
            extent = target["permissible_extent_selectors"].get(side, [])
            if not set(core) <= set(extent):
                raise ValueError(f"core outside permissible extent: {name}/{side}")
            for selector_id in extent:
                selector = selectors[selector_id]
                if selector["side"] != side or not selector["literal_quote"]:
                    raise ValueError(f"invalid finite selector: {name}/{selector_id}")
                if target["source_resolution_complete"]:
                    resolution = resolved[selector_id]
                    rows = source_rows(resolution)
                    if resolution["status"] != "unique" or not rows or any(not row for row in rows):
                        raise ValueError(f"missing resolved sources: {name}/{selector_id}")
                    if len(rows) != len(selector["literal_quote"]):
                        raise ValueError(f"lost scalar multiplicity: {name}/{selector_id}")
        for selector_id in target["unchanged_control_selectors"]:
            if selector_id not in selectors:
                raise ValueError(f"unknown unchanged control: {name}/{selector_id}")
        if target["body_eligible"] and target["source_resolution_complete"]:
            if not all(target["core_selectors"].get(side) for side in ("old", "new")):
                raise ValueError(f"body target lacks both source cores: {name}")
            eligible.append(target)
    publishers = {target["independent_producer"] for target in eligible}
    if len(eligible) < 6 or len(publishers) < 3:
        raise ValueError("fewer than six resolved body pairs or three publishers")
    print(f"Source registration: {len(pairs)} panel pairs; "
          f"{len(eligible)} resolved body candidates; {len(publishers)} independent producers")
    controls = read(DIRECTORY / "controls.json")
    control_files = {Path(reference["path"]).name: checked_path(reference)
                     for reference in controls["references"]}
    generated = read(control_files["expectations.json"])["pairs"]
    metamorphic = read(control_files["source-mutation-expectations.json"])["pairs"]
    if len(generated) != 60 or len(metamorphic) != 3:
        raise ValueError("historical control denominators changed")
    print("Control registration: 60 generated and 3 real-source metamorphic pairs")
    return panel


def common_complete(report, run):
    """Require document-wide source coverage as well as the public complete flag."""
    if run["status"] != "captured" or run.get("exit_code") not in (0, 1):
        return False
    if report.get("comparison_complete") is not True:
        return False
    if report["contract"]["channels"] != ["text"]:
        raise ValueError("completion channel contract changed")
    coverage = report["coverage"]
    if len(coverage) != 1 or coverage[0]["channel"] != "text":
        raise ValueError("missing common-text coverage")
    coverage = coverage[0]
    if coverage["complete"] is not True:
        raise ValueError("complete report has incomplete coverage")
    for side in ("old", "new"):
        discovered = coverage[side + "_discovered_sources"]
        compared = coverage[side + "_compared_sources"]
        presence = coverage[side + "_presence_sources"]
        if (not coverage[side + "_inventory_complete"] or discovered <= 0
                or coverage[side + "_uncompared_sources"] != 0
                or compared + presence != discovered):
            raise ValueError("complete report lacks nonempty full source coverage")
    return True


def baseline_observations(panel):
    index = read(DIRECTORY / "baseline-observations.json")
    baseline = read(DIRECTORY / "baseline.json")
    pairs = unique_by(panel["pairs"], "id")
    observed = {}
    for observation in index["observations"]:
        name, repetition = observation["pair"], observation["repetition"]
        key = name, repetition
        if key in observed or repetition not in (1, 2):
            raise ValueError("duplicate or invalid baseline repetition")
        pair = pairs[name]
        capture = read(checked_path(observation["capture"]))
        if (capture["binary_sha256"] != baseline["binary"]["sha256"]
                or capture["timeout_seconds"] != 180 or capture["limit_scale"] != 1):
            raise ValueError("baseline executable or budget mismatch")
        run = capture["runs"][observation["run_index"]]
        if run["pair"] != name or run["route"] != "text":
            raise ValueError("baseline run identity mismatch")
        for side in ("old", "new"):
            if run[side + "_sha256"] != pair[side]["sha256"]:
                raise ValueError("baseline input mismatch")
        if run["status"] == "captured":
            report_path = checked_path(observation["report"])
            if (observation["report"]["sha256"] != run["report_sha256"]
                    or report_path.stat().st_size != run["report_bytes"]):
                raise ValueError("baseline report mismatch")
            complete = common_complete(read(report_path), run)
        elif run["status"] == "failed" and run.get("exit_code") not in (0, 1):
            complete = False
        else:
            raise ValueError("baseline attempt lacks a report or explicit process failure")
        observed[key] = complete
    expected = {(name, repetition) for name in pairs for repetition in (1, 2)}
    if observed.keys() != expected:
        raise ValueError("baseline panel lacks two observations per pair")
    if any(observed[name, 1] != observed[name, 2] for name in pairs):
        raise ValueError("baseline completion is not repeatable")
    complete = sum(observed[name, 1] for name in pairs)
    print(f"Baseline common-text complete: {complete}/{len(pairs)} in both repetitions")


def constraint_stop(panel):
    record = read(DIRECTORY / "constraint-stop.json")
    checked_path(record["panel"])
    for source in record["contract_sources"]:
        checked_path(source)
    baseline = read(DIRECTORY / "baseline.json")
    unchanged = subprocess.run(
        ["git", "diff", "--quiet", baseline["baseline_commit"], "--",
         "crates/pdfdelta-core", "crates/pdfdelta-cli"], cwd=ROOT, check=False)
    if unchanged.returncode != 0:
        raise ValueError("constraint stop no longer describes the production tree")
    probes = read(checked_path(record["inventory_probes"]))
    if probes["frozen_binary_sha256"] != baseline["binary"]["sha256"]:
        raise ValueError("inventory probes used a different executable")
    pairs = unique_by(panel["pairs"], "id")
    observed = set()
    blocked = set()
    for row in probes["rows"]:
        key = row["pair"], row["side"]
        if key in observed or row["side"] not in ("old", "new"):
            raise ValueError("duplicate or invalid inventory probe")
        observed.add(key)
        if row["pdf_sha256"] != pairs[row["pair"]][row["side"]]["sha256"]:
            raise ValueError("inventory probe input mismatch")
        path = checked_path(row["response"])
        if row["exit_code"] != 0 or not row.get("non_text_paint"):
            continue
        with path.open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
            marker = b'"last_non_text_paint":'
            start = data.find(marker)
            if start < 0:
                raise ValueError("paint observation missing from worker response")
            start += len(marker)
            paint, _ = json.JSONDecoder().raw_decode(data[start:start + 1024 * 1024].decode())
            if paint != row["non_text_paint"]:
                raise ValueError("paint observation differs from worker response")
            blocked.add(row["pair"])
    if observed != {(name, side) for name in pairs for side in ("old", "new")}:
        raise ValueError("inventory probe panel is incomplete")
    upper_bound = len(pairs) - len(blocked)
    if sorted(blocked) != record["pairs_with_non_text_paint"] or upper_bound >= 2:
        raise ValueError("the retained probes do not establish this constraint stop")
    print("G1: 0 additional development pairs demonstrated; required 6 pairs / 3 producers")
    print("G2: fresh blind set not selected; required 12 pairs and recovery on 3 pairs / 2 producers")
    print(f"G3: {len(blocked)}/{len(pairs)} pairs have non-text paint; "
          f"at most {upper_bound} can complete under the preserved native inventory rule; required gain 2")
    print("G4: production unchanged; baseline common-text target controls have no strict-mask claims")
    print("G5: recovery adjudications, fresh blind evidence and the full final evaluator are missing")
    raise ValueError("constraint-blocked stop; goal not achieved")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", choices=STAGES, required=True)
    args = parser.parse_args()
    try:
        panel = registration()
        baseline_observations(panel)
        if args.stage == "registration":
            return 0
        if (DIRECTORY / "constraint-stop.json").exists():
            constraint_stop(panel)
        raise ValueError("later-stage recovery, diagnosis and correctness evidence is not registered")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"FAIL {args.stage}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
