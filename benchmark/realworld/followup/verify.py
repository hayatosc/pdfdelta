#!/usr/bin/env python3
"""Check retained follow-up evidence; missing stages never imply success."""

import argparse
import hashlib
import json
from pathlib import Path
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", choices=STAGES, required=True)
    args = parser.parse_args()
    try:
        panel = registration()
        baseline_observations(panel)
        if args.stage == "registration":
            return 0
        raise ValueError("later-stage recovery, diagnosis and correctness evidence is not registered")
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"FAIL {args.stage}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
