"""Bind repeated V77 captures to the complete reviewed V75 output payloads."""

from collections import Counter
import copy
from functools import cache
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"
verify.DIRECTORY = BASE


def read(path):
    return json.loads(path.read_text())


@cache
def ref(path):
    return {"path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def save(name, value):
    (BASE / name).write_text(json.dumps(value, indent=2) + "\n")


def evidence(references):
    unique = {(row["path"], row["sha256"]): row for row in references}
    for path, digest in unique:
        assert ref(ROOT / path)["sha256"] == digest
    return list(unique.values())


def main():
    _, registered = verify.registration()
    first = read(BASE / "v77-panel-observations.json")
    second = read(BASE / "v77-panel-repetition2-observations.json")
    assert first["build"] == second["build"]
    captured_build = verify.read_reference(first["build"])
    final_build = ref(ROOT / "benchmark/realworld/cache/source-boundaries-order-guard-v77-final/build.json")
    build = verify.read_reference(final_build)
    assert captured_build["binary"]["sha256"] == build["binary"]["sha256"]
    first["build"] = final_build
    assert build["production_sha256"] == verify.source_fingerprint()
    observations = {"version": 1, "repetitions": 2, "build": first["build"],
                    "observations": first["observations"] + second["observations"]}
    verify.observations(observations, registered["panel"], build["binary"])
    prior_path = BASE / "v75-adjudications.json"
    prior = {(row["pair"], row["repetition"]): row for row in read(prior_path)["observations"]}
    baseline = {(row["pair"], row["repetition"]): row
                for row in registered["baseline-observations"]["observations"]}
    secondary = {(row["pair"], row["repetition"]): row
                 for row in read(BASE / "baseline-observations.json")["observations"]}
    targets = {row["pair"]: row for row in registered["targets"]["targets"]}
    full, formal, replay, scores, incremental = [], [], [], [], []
    repeated = {}
    for observation in observations["observations"]:
        pair, repetition = observation["pair"], observation["repetition"]
        report = verify.read_reference(observation["report"])
        events = verify.events(report)
        signatures = [verify.event_digest(event) for event in events]
        complete = [(signature, event.get("review")) for signature, event in zip(signatures, events)]
        if pair in repeated:
            assert repeated[pair] == complete, pair
        else:
            repeated[pair] = complete
        previous = prior[pair, repetition]
        old_events = {verify.event_digest(event): event
                      for event in verify.events(verify.read_reference(previous["report"]))}
        old_reviews = {row["event_sha256"]: row for row in previous["events"]}
        rows = []
        for event, digest in zip(events, signatures):
            if digest in old_events:
                assert event.get("review") == old_events[digest].get("review"), (pair, digest)
                row = copy.deepcopy(old_reviews[digest])
                row["identity_revalidation"] = (
                    "The complete event and local review equal V75, including its exact mask, "
                    "normalization premises, source order and cut certificates.")
                references = row["source_evidence"] + [ref(prior_path), previous["report"]]
            else:
                raise AssertionError((pair, digest, "new output requires source adjudication"))
            row["pointer"] = event["pointer"]
            row["source_evidence"] = evidence(references)
            rows.append(row)
        full.append({**observation, "events": rows})
        old_signatures = set(old_events)
        replay.append({"pair": pair, "repetition": repetition, "before": previous["report"],
                       "after": observation["report"],
                       "retained_complete_reviews": len(old_signatures & set(signatures)),
                       "lost_event_digests": sorted(old_signatures - set(signatures)),
                       "added_event_digests": sorted(set(signatures) - old_signatures)})
        core, extent, controls = verify.historical.target_sources(targets[pair])
        gold = None
        if targets[pair]["strict_event_gold"] is not None:
            gold = {"operation": targets[pair]["strict_event_gold"],
                    "sources": {tuple(atom) for atom in targets[pair]["strict_changed_position_gold"]}}
        for index, output in ((baseline, scores), (secondary, incremental)):
            before_row = index[pair, repetition]
            before = verify.read_reference(before_row["report"]) if before_row.get("report") else None
            counts = Counter(verify.event_digest(event) for event in verify.events(before))
            needed = []
            for digest, row in zip(signatures, rows):
                if counts[digest]:
                    counts[digest] -= 1
                else:
                    needed.append(row)
            score = verify.pair_recovery(before, report, core, extent, controls, needed, gold)
            assert score["correct"], (pair, repetition, score)
            output.append({"pair": pair, "repetition": repetition, "score": score})
            if index is baseline:
                formal.append({**observation, "events": needed})
    assert len(observations["observations"]) == 72
    assert sum(map(len, repeated.values())) == 224
    save("v77-repeated-observations.json", observations)
    save("v77-development-identity-replay.json", {"version": 1,
         "prior_adjudications": ref(prior_path), "build": first["build"], "rows": replay})
    save("v77-adjudications.json", {"version": 1,
         "scope": "All A/B outputs in both V77 panels. Retained V75 outputs have complete payload identity; no outputs are added and three outside-target outputs are lost.",
         "observations": full})
    save("v77-formal-adjudications.json", {"version": 1,
         "scope": "Outputs additional to the corresponding exact historical baseline attempts.",
         "all_outputs": ref(BASE / "v77-adjudications.json"), "observations": formal})
    save("v77-reviewed-panel-scores.json", {"version": 1,
         "historical_scores": scores, "incremental_599855d_scores": incremental,
         "observations": ref(BASE / "v77-repeated-observations.json"),
         "adjudications": ref(BASE / "v77-adjudications.json")})
    print("Reviewed 224 distinct outputs across 72 captures against both registered baselines.")


if __name__ == "__main__":
    main()
