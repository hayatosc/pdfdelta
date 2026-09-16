"""Rebind the source-adjudicated MobileNetV2 interval to two V65 captures."""

import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / "benchmark/realworld/source-boundaries"
sys.path.insert(0, str(ROOT / "benchmark/realworld/remaining"))
import verify

verify.CONTRACT = "source-boundaries-v1"


def reference(path):
    return {"path": str(path.relative_to(ROOT)),
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    prior_path = BASE / "unseen-v6-diagnosis/v63-mobilenet-review.json"
    prior = json.loads(prior_path.read_text())
    for key in ("build", "targets", "original_sources", "reproducer"):
        verify.historical.checked_path(prior[key])
    cache = ROOT / "benchmark/realworld/cache/source-boundaries-column-edges-v65"
    build = json.loads((cache / "build.json").read_text())
    observations = []
    repeated = None
    for old in prior["observations"]:
        repetition = old["repetition"]
        directory = cache / f"mobilenet-repetition{repetition}"
        report_path = directory / "arxiv-mobilenetv2-v1-to-v4-text.json"
        capture_path = directory / "runs.json"
        before = verify.read_reference(old["report"])
        after = json.loads(report_path.read_text())
        capture = json.loads(capture_path.read_text())
        old_capture = verify.read_reference(old["capture"])
        assert capture["binary_sha256"] == build["binary"]["sha256"]
        assert capture["timeout_seconds"] == old_capture["timeout_seconds"] == 180
        assert capture["limit_scale"] == old_capture["limit_scale"] == 1
        assert len(capture["runs"]) == 1 and capture["runs"][0]["status"] == "captured"
        for side in ("old", "new"):
            assert capture["runs"][0][side + "_sha256"] == old_capture["runs"][0][side + "_sha256"]
            assert before[side]["revision"] == after[side]["revision"]
        signature = lambda report: [(verify.event_digest(e), e["review"])
                                    for e in verify.events(report)]
        current = signature(after)
        assert current == signature(before)
        assert repeated is None or repeated == current
        repeated = current
        observations.append({**old, "report": reference(report_path),
                             "capture": reference(capture_path),
                             "complete_payloads_equal_prior": True})
    result = {"version": 65, "pair": prior["pair"], "build": reference(cache / "build.json"),
              "prior_adjudication": reference(prior_path), "observations": observations,
              "reproducer": reference(Path(__file__)), "scope": prior["scope"]}
    (BASE / "unseen-v6-diagnosis/v65-mobilenet-review.json").write_text(json.dumps(result, indent=2) + "\n")
    print("V65: both MobileNetV2 captures retain every complete adjudicated V63 payload.")


if __name__ == "__main__":
    main()
