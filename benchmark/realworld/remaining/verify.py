#!/usr/bin/env python3
"""Verify the remaining recovery contract from hash-bound observations."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
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


def observations(index, panel, binary):
    """Validate repeats, identity, process costs and full common-text coverage."""
    historical.checked_path(binary)
    pairs = historical.unique_by(panel["pairs"], "id")
    observed = {}
    for observation in index["observations"]:
        name, repetition = observation["pair"], observation["repetition"]
        key = name, repetition
        require(key not in observed and repetition in (1, 2), "duplicate/invalid repetition")
        pair = pairs[name]
        capture = read_reference(observation["capture"])
        require(capture["binary_sha256"] == binary["sha256"]
                and capture["timeout_seconds"] == 180 and capture["limit_scale"] == 1,
                "capture executable or budget changed")
        run = capture["runs"][observation["run_index"]]
        require(run["pair"] == name and run["route"] == "text", "capture identity mismatch")
        for side in ("old", "new"):
            require(run[side + "_sha256"] == pair[side]["sha256"], "capture input mismatch")
        require(run["wall_seconds"] >= 0 and run["peak_rss_kib"] >= 0,
                "missing or negative process cost")
        report = None
        if run["status"] == "captured":
            path = historical.checked_path(observation["report"])
            require(run["report_sha256"] == observation["report"]["sha256"]
                    and run["report_bytes"] == path.stat().st_size, "capture report mismatch")
            report = historical.read(path)
            require(report["contract"] == {"version": 1, "channels": ["text"]},
                    "common-text report contract changed")
        else:
            require(run["status"] == "failed" and run.get("exit_code") not in (0, 1),
                    "missing report is not an explicit failed attempt")
        observed[key] = {"complete": report is not None and complete(report, run),
                         "run": run, "report": report, "reference": observation.get("report")}
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
        # The remaining result adapters are intentionally fail-closed until their
        # source-adjudication and fresh-freeze evidence contracts are implemented.
        raise ValueError("development, blind and final evidence adapters remain unfinished")
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print(json.dumps(gates, indent=2))
        print(f"FAIL {args.stage}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
