"""Exercise every stage with constructed evidence, never real recovery claims."""

from contextlib import ExitStack, redirect_stderr, redirect_stdout
import hashlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import verify
import test_verify


class StageTests(unittest.TestCase):
    def test_all_stages_pass_and_final_rejects_missing_or_stale_evidence(self):
        with tempfile.TemporaryDirectory() as temporary, ExitStack() as stack:
            root = Path(temporary)
            remaining = root / "benchmark/realworld/remaining"
            historical = remaining.parent / "followup"
            stack.enter_context(patch.multiple(verify, ROOT=root, DIRECTORY=remaining))
            stack.enter_context(patch.multiple(verify.historical, ROOT=root, DIRECTORY=historical))

            def write(path, value):
                path = root / path
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps(value))
                return {"path": str(path.relative_to(root)),
                        "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}

            def annotation(path, pair, control=False):
                selectors, resolved = [], []
                for side, glyph in (("old", 1), ("new", 2)):
                    name = f"{side}-paragraph-0-piece-0" if control else f"body-{side}"
                    selectors.append({"id": name, "side": side, "literal_quote": side[0]})
                    resolved.append({"id": name, "status": "unique", "sources": [
                        {"atoms": [{"kind": "glyph", "id": glyph}]}]})
                authored = write(path.with_suffix(".json"), {
                    "selectors": selectors, **{side + "_sha256": pair[side]["sha256"] for side in ("old", "new")}})
                resolution = write(path.with_suffix(".resolved.json"), {
                    "selector_resolution_complete": True, "selectors": resolved})
                return {"annotation": authored, "resolution": resolution}

            def common_report(changed, complete=False, route="text"):
                report = test_verify.EvidenceTests.report()
                scope = report["comparison"]["scopes"][0]["result"]
                if changed:
                    scope["text_scope_reviews"][0]["comparison"]["operation"].update(old="o", new="n")
                else:
                    scope["text_scope_reviews"] = []
                    report["scope_content_changes"] = 0
                scope.update(unresolved=[], candidates={"exhaustive": True}, text_search={"exhaustive": True},
                             matching={"conflict_search_complete": True, "components": [{"exhaustive": True}]})
                coverage = {"channel": "text", "complete": complete}
                for side in ("old", "new"):
                    coverage.update({side + "_inventory_complete": complete, side + "_discovered_sources": 1,
                                     side + "_compared_sources": int(complete), side + "_presence_sources": 0,
                                     side + "_uncompared_sources": int(not complete)})
                channels = ["text"] if route == "text" else ["text", "visual", "forms", "relations"]
                report.update(comparison_complete=complete, coverage=[coverage],
                              contract={"version": 1, "channels": channels})
                return report

            baseline_binary = write("baseline-binary", "constructed baseline executable")
            binary = write("current-binary", "constructed current executable")
            for path in ("Cargo.toml", "Cargo.lock", "crates/pdfdelta-core/Cargo.toml",
                         "crates/pdfdelta-cli/Cargo.toml", "crates/pdfdelta-bench/Cargo.toml"):
                write(path, "constructed source fingerprint input")
            build = write(remaining / "build.json", {
                "production_sha256": verify.source_fingerprint(), "binary": binary, "exit_code": 0,
                "command": ["cargo", "build", "--release", "-p", "pdfdelta-cli", "--locked"],
                "log": write("build.log", "constructed successful build observation")})

            def panel_and_targets(prefix, count):
                pairs, targets = [], []
                for index in range(count):
                    name = f"{prefix}-{index}"
                    pair = {"id": name, "family": f"family-{index % 6}", "language": "ja" if index % 2 else "en",
                            "series": name, "novelty_review": "Constructed disjoint series",
                            **{side: write(f"inputs/{name}-{side}", f"{name}-{side}") for side in ("old", "new")}}
                    pair["primary_source_evidence"] = [pair["old"], pair["new"]]
                    pairs.append(pair)
                    targets.append({"pair": name, "references": annotation(root / "annotations" / name, pair),
                                    "core_selectors": {side: [f"body-{side}"] for side in ("old", "new")},
                                    "permissible_extent_selectors": {side: [f"body-{side}"] for side in ("old", "new")},
                                    "unchanged_control_selectors": [], "body_eligible": True,
                                    "source_resolution_complete": True, "independent_producer": f"producer-{index % 3}",
                                    "strict_event_gold": None, "strict_changed_position_gold": None})
                return {"pairs": pairs}, {"targets": targets}

            panel, targets = panel_and_targets("development", 36)
            blind_panel, blind_targets = panel_and_targets("blind", 12)

            def captured_run(pair, route, reference, repetition=1, complete=False):
                return {"pair": pair["id"], "route": route, "status": "captured", "exit_code": 0 if complete else 3,
                        **{side + "_sha256": pair[side]["sha256"] for side in ("old", "new")},
                        "wall_seconds": repetition, "peak_rss_kib": 100,
                        "report_bytes": (root / reference["path"]).stat().st_size, "report_sha256": reference["sha256"]}

            def phase_records(prefix, panel, targets, executable, gains=0, completions=0):
                runs, observations, adjudications = [], [], []
                for index, (pair, target) in enumerate(zip(panel["pairs"], targets["targets"])):
                    for repetition in (1, 2):
                        report = common_report(index < gains, index < completions)
                        reference = write(f"reports/{prefix}-{pair['id']}-{repetition}.json", report)
                        observations.append({"pair": pair["id"], "repetition": repetition,
                                             "run_index": len(runs), "report": reference})
                        runs.append(captured_run(pair, "text", reference, repetition, index < completions))
                        if index < gains:
                            reviews = test_verify.EvidenceTests.adjudication(report)
                            reviews[0]["source_evidence"] = list(target["references"].values())
                            adjudications.append({"pair": pair["id"], "repetition": repetition,
                                                  "report": reference, "events": reviews})
                capture = write(f"{prefix}-capture.json", {
                    "binary_sha256": executable["sha256"], "timeout_seconds": 180, "limit_scale": 1, "runs": runs})
                for observation in observations:
                    observation["capture"] = capture
                return {"production_sha256": verify.source_fingerprint(), "binary": executable, "build": build,
                        "observations": write(f"{prefix}-observations.json", {"observations": observations}),
                        "adjudications": write(f"{prefix}-adjudications.json", {"observations": adjudications})}

            baseline = phase_records("baseline", panel, targets, baseline_binary)
            development = phase_records("development", panel, targets, binary, gains=6, completions=2)
            blind_baseline = phase_records("blind-baseline", blind_panel, blind_targets, baseline_binary)
            blind = phase_records("blind", blind_panel, blind_targets, binary, gains=3)
            blind.update(comparison_started_utc="2026-09-11T04:00:00Z",
                         baseline_comparison_started_utc="2026-09-11T04:00:00Z")

            directory = remaining.parent / "next/layout-controls"
            generated, expectations, real, references, runs, reports = [], [], [], [], [], []
            for index in range(63):
                name = f"control-{index}"
                pair = {"id": name, **{side: write(f"controls/{name}-{side}", side) for side in ("old", "new")}}
                references.extend(annotation(directory / "annotations" / name, pair, control=index < 60).values())
                expectation = {"strict_events": 1, "strict_source_atoms": [
                    {"side": side, "atoms": [{"kind": "glyph", "id": glyph}]} for side, glyph in (("old", 1), ("new", 2))]}
                if index < 60:
                    generated.append(pair)
                    expectations.append(dict(expectation, pair=name, changed_paragraph=0))
                else:
                    real.append(dict(pair, **expectation))
                for route in ("native", "text", "all"):
                    if route == "native":
                        report = {"changes": [{"kind": "replacement", "occurrences": [{
                            side + "_span": {"sources": [{"kind": "glyph", "glyph_id": glyph}]}
                            for side, glyph in (("old", 1), ("new", 2))}]}]}
                    else:
                        report = common_report(True, route=route)
                    reference = write(f"control-reports/{name}-{route}.json", report)
                    reports.append({"pair": name, "route": route, "report": reference})
                    runs.append(captured_run(pair, route, reference))
            control_files = [write(directory / "manifest.json", {"generated_pairs": generated}),
                             write(directory / "expectations.json", {"pairs": expectations}),
                             write(directory / "source-mutation-expectations.json", {"pairs": real})]
            references.extend(control_files)
            development["controls"] = write("control-results.json", {"reports": reports, "capture": write(
                "control-capture.json", {"binary_sha256": binary["sha256"], "timeout_seconds": 180, "limit_scale": 1,
                                         "runs": runs, "reference_hashes": {Path(ref["path"]).name: ref["sha256"]
                                                                              for ref in control_files}})})
            control_registration = write(historical / "controls.json", {"references": references})
            baseline_reference = write(historical / "baseline.json", {"binary": baseline_binary})
            target_reference = write(historical / "targets.json", targets)
            panel.update(baseline_sha256=baseline_reference["sha256"], targets=target_reference)
            panel_reference = write(historical / "panel.json", panel)
            baseline_observations = write(historical / "baseline-observations.json", verify.read_reference(baseline["observations"]))
            registration = write(remaining / "registration.json", {
                "historical": {"panel": panel_reference, "baseline": baseline_reference, "targets": target_reference,
                               "controls": control_registration, "baseline-observations": baseline_observations},
                "completion_contract": {"channels": ["text"], "timeout_seconds": 180, "limit_scale": 1, "repetitions": 2}})
            write(remaining / "diagnosis.json", {"registration": registration, "targets": [
                {"pair": pair["id"], "stage": "normalization", "reason": "Constructed blocker",
                 "next_action": "Exercise stage validation", "evidence": [target_reference]} for pair in panel["pairs"]]})
            write(remaining / "development.json", development)
            write(remaining / "blind.json", blind)
            blind_panel_reference = write(remaining / "blind-panel.json", blind_panel)
            blind_targets_reference = write(remaining / "blind-targets.json", blind_targets)
            excluded = remaining.parent / "next/blind"
            write(excluded / "excluded-series.json", {"series": []})
            for name in ("selection.json", "replacement-selection.json"):
                write(excluded / name, {"pairs": []})
            for command in (["git", "init", "-q"], ["git", "add", "."],
                            ["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                             "commit", "-qm", "test: register constructed blind evidence"]):
                subprocess.run(command, cwd=root, check=True, capture_output=True)
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
            write(remaining / "blind-freeze.json", {
                "production_sha256": verify.source_fingerprint(), "binary": binary, "panel": blind_panel_reference,
                "targets": blind_targets_reference, "registration_commit": commit,
                "freeze_utc": "2026-09-11T01:00:00Z", "selection_started_utc": "2026-09-11T02:00:00Z",
                "annotation_completed_utc": "2026-09-11T03:00:00Z",
                "baseline_observations": blind_baseline["observations"]})
            commands = [["cargo", "fmt", "--all", "--", "--check"],
                        ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"],
                        ["cargo", "test", "--workspace", "--locked"],
                        ["cargo", "run", "-p", "pdfdelta-bench", "--locked", "--", "verify"]]
            write(remaining / "quality-checks.json", {
                "source_sha256": verify.source_fingerprint(include_bench=True), "checks": [
                    {"command": command, "exit_code": 0, "log": write(f"quality-{index}.log", "constructed pass")}
                    for index, command in enumerate(commands)]})

            def run(stage):
                output, errors = io.StringIO(), io.StringIO()
                with patch("sys.argv", ["verify", "--stage", stage]), redirect_stdout(output), redirect_stderr(errors):
                    status = verify.main()
                return status, output.getvalue(), errors.getvalue()

            for stage in verify.STAGES:
                with self.subTest(stage=stage):
                    status, output, errors = run(stage)
                    self.assertEqual(status, 0, errors)
            self.assertIn('"G5": {\n    "passed": true', output)
            adjudication = root / development["adjudications"]["path"]
            saved = adjudication.read_text()
            adjudication.write_text("{}")
            status, _, errors = run("final")
            self.assertEqual(status, 1)
            self.assertIn("stale evidence", errors)
            adjudication.write_text(saved)
            adjudication.unlink()
            status, _, errors = run("final")
            self.assertEqual(status, 1)
            self.assertIn("No such file", errors)


if __name__ == "__main__":
    unittest.main()
