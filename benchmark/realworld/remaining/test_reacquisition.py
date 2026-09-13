"""Exact historical reacquisition controls, using only constructed evidence."""

import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import verify


class ReacquisitionTests(unittest.TestCase):
    def test_only_missing_attempts_can_be_reacquired(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def write(name, value):
                path = root / name
                path.write_text(json.dumps(value))
                return {"path": name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}

            binary = write("binary", "constructed historical executable")
            pair = {"id": "pair", "old": write("old", "old"), "new": write("new", "new")}
            panel = {"pairs": [pair]}

            def capture(name, wall):
                return write(name, {
                    "binary_sha256": binary["sha256"], "timeout_seconds": 180, "limit_scale": 1,
                    "runs": [{"pair": "pair", "route": "text", "status": "failed", "exit_code": 2,
                              "wall_seconds": wall, "peak_rss_kib": 1,
                              **{side + "_sha256": pair[side]["sha256"] for side in ("old", "new")}}]})

            first = {"pair": "pair", "repetition": 1, "run_index": 0,
                     "capture": capture("missing.json", 1), "report": None}
            second = {"pair": "pair", "repetition": 2, "run_index": 0,
                      "capture": capture("retained.json", 2), "report": None}
            original = {"observations": [first, second]}
            original_reference = write("original.json", original)
            sources = {"baseline": {"binary": binary}, "baseline-observations": original}
            record = {"historical": {"baseline-observations": original_reference}}
            registration = write("registration.json", record)
            restoration = write("restoration.json", {
                "restored_binary": binary, "build": write("build.json", {
                    "exit_code": 0, "binary": binary, "log": write("build.log", "constructed build")})})
            index = {
                "original_index": original_reference, "binary": binary, "restoration": restoration,
                "observations": [dict(first, capture=capture("fresh.json", 3)), second],
                "reacquired": [{"pair": "pair", "repetition": 1,
                                "missing_original_references": [first["capture"]]}],
            }
            (root / "missing.json").unlink()

            def validate(value):
                replacement = {"version": 1, "registration": registration,
                               "observations": write("replacement.json", value)}
                return verify.reacquired_baseline(record, sources, panel, replacement)

            with patch.multiple(verify, ROOT=root, DIRECTORY=root, CONTRACT="source-boundaries-v1"), \
                    patch.object(verify.historical, "ROOT", root):
                self.assertEqual(validate(index), index)
                for mutation in ("binary", "original", "absence", "retained", "reused", "restoration"):
                    with self.subTest(mutation=mutation):
                        invalid = copy.deepcopy(index)
                        if mutation == "binary":
                            invalid["binary"] = write("other-binary", "different")
                        elif mutation == "original":
                            invalid["original_index"]["sha256"] = "0" * 64
                        elif mutation == "absence":
                            invalid["reacquired"] = []
                        elif mutation == "retained":
                            invalid["observations"][1]["capture"] = capture("other.json", 4)
                        elif mutation == "reused":
                            invalid["observations"][0]["capture"] = second["capture"]
                        elif mutation == "restoration":
                            invalid["restoration"]["sha256"] = "0" * 64
                        with self.assertRaises(ValueError):
                            validate(invalid)
                for field, value in (("timeout_seconds", 181), ("limit_scale", 2),
                                     ("binary_sha256", "0" * 64)):
                    with self.subTest(capture_field=field):
                        invalid = copy.deepcopy(index)
                        run = json.loads((root / "fresh.json").read_text())
                        run[field] = value
                        invalid["observations"][0]["capture"] = write("bad-capture.json", run)
                        with self.assertRaises(ValueError):
                            validate(invalid)
                # A corrupt surviving file cannot masquerade as an absent attempt.
                (root / "missing.json").write_text("corrupt original")
                with self.assertRaisesRegex(ValueError, "stale evidence"):
                    validate(index)


if __name__ == "__main__":
    unittest.main()
