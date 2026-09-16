#!/usr/bin/env python3
"""Re-resolve fixed quotes after native acquisition changes, without editing gold."""

import argparse
import hashlib
import json
from pathlib import Path
import resource
import subprocess

import verify


def reference(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "sha256": digest}


def read(path):
    return json.loads(path.read_text())


def checked(ref):
    path = Path(ref["path"])
    if reference(path) != ref:
        raise ValueError(f"stale evidence: {path}")
    return read(path)


def output_limit():
    resource.setrlimit(resource.RLIMIT_FSIZE, (128 * 1024**2, 128 * 1024**2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    panel_path = Path("benchmark/realworld/remaining/deferred-clip-panel-result.json")
    targets_path = Path("benchmark/realworld/followup/targets.json")
    panel = read(panel_path)
    inputs = {p["id"]: p for p in checked(panel["panel"])["pairs"]}
    targets = {t["pair"]: t for t in read(targets_path)["targets"]}
    result = {
        "status": "diagnostic_fixed_quote_rebinding",
        "production_changed": False,
        "certifies_gate_gain": False,
        "panel": reference(panel_path),
        "targets": reference(targets_path),
        "resolver": reference(args.binary),
        "script": reference(Path(__file__)),
        "timeout_seconds": 180,
        "output_limit_bytes": 128 * 1024**2,
        "fixed_panel_denominator": 36,
        "rows": [],
        "limitations": [
            "Unique literal selectors do not certify complete extraction or inventory.",
            "Source-range hits are diagnostic, not adjudicated gate scores.",
            "Unresolved or changed quotes retain failure; annotations are never rewritten.",
            "Resolver and comparison native glyph counts must match before scoring; this alone is not a full provenance equality proof.",
        ],
    }
    verify.CONTRACT = "source-boundaries-v1"
    for previous in panel["rows"]:
        if previous["native_equal"]:
            continue
        pair = previous["pair"]
        target = targets[pair]
        row = {"pair": pair, "historically_resolved": target["source_resolution_complete"]}
        result["rows"].append(row)
        annotation_ref = target["references"]["annotation"]
        checked(annotation_ref)
        command = [str(args.binary.resolve()), "validate-literal-selectors",
                   "--annotation", annotation_ref["path"]]
        for side in ("old", "new"):
            source = inputs[pair][side]
            if reference(Path(source["path"]))["sha256"] != source["sha256"]:
                raise ValueError(f"stale PDF: {pair} {side}")
            command.extend([f"--{side}", source["path"]])
        output = args.output / f"{pair}.json"
        error = args.output / f"{pair}.stderr.txt"
        with output.open("wb") as stdout, error.open("wb") as stderr:
            try:
                completed = subprocess.run(command, stdout=stdout, stderr=stderr,
                                           timeout=180, preexec_fn=output_limit, check=False)
                row["exit_code"] = completed.returncode
            except subprocess.TimeoutExpired:
                row["status"] = "timeout"
        row.update(annotation=annotation_ref, resolution=reference(output), stderr=reference(error))
        if row.get("exit_code") in (0, 3):
            resolved = read(output)
            row["selector_statuses"] = {s["id"]: s["status"] for s in resolved["selectors"]}
            row["native"] = {side: resolved[side] for side in ("old", "new")}
            report = checked(previous["report_after"])
            row["report"] = previous["report_after"]
            row["native_counts_match"] = all(
                resolved[side]["sha256"] == report[side]["revision"]
                and resolved[side]["glyph_count"] == report[side]["native_glyphs"]
                for side in ("old", "new"))
            if not row["native_counts_match"]:
                row["status"] = "native_population_mismatch"
            elif not resolved["selector_resolution_complete"]:
                row["status"] = "unresolved_selectors"
            else:
                row["status"] = "resolved"
                selectors = {s["id"]: s for s in resolved["selectors"]}
                sets = []
                for key in ("core_selectors", "permissible_extent_selectors"):
                    sets.append({(side, atom["id"])
                                 for side, ids in target[key].items() for name in ids
                                 for atoms in verify.historical.source_rows(selectors[name])
                                 for atom in atoms if atom["kind"] == "glyph"})
                core, extent = sets
                if not core or not core <= extent:
                    raise ValueError(f"invalid fixed target: {pair}")
                row["core_atoms"] = len(core)
                events = verify.events(report)
                row["B_range_hits"] = [e["pointer"] for e in events
                                       if e["category"] == "B" and verify.range_recovery(
                                           e["review"], core, extent)["source_range_hit"]]
                overlaps = [
                    {"pointer": e["pointer"], "core_intersection": len(e["sources"] & core),
                     "missing_core": len(core - e["sources"]),
                     "outside_extent": len(e["sources"] - extent)}
                    for e in events if e["category"] == "B" and e["sources"] & core]
                row["nearest_B_ranges"] = sorted(
                    overlaps, key=lambda e: (e["missing_core"], e["outside_extent"]))[:3]
        else:
            row.setdefault("status", "resolver_failed")
        (args.output / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(pair, row["status"], row.get("B_range_hits"), flush=True)


if __name__ == "__main__":
    main()
