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
CONTRACT = "historical"


def read_reference(reference):
    return historical.read(historical.checked_path(reference))


def require(condition, message):
    if not condition:
        raise ValueError(message)


def registration():
    record = historical.read(DIRECTORY / "registration.json")
    if CONTRACT == "source-boundaries-v1":
        require(record.get("contract") == "source-boundaries-phase-v1",
                "source-boundary evidence contract is missing or changed")
    sources = {key: read_reference(value) for key, value in record["historical"].items()}
    require(record["completion_contract"] == {
        "channels": ["text"], "timeout_seconds": 180, "limit_scale": 1, "repetitions": 2,
    }, "registered completion contract changed")
    panel = historical.registration()
    require(panel == sources["panel"], "historical panel differs from the registered panel")
    replacement = DIRECTORY / "baseline-reacquisition.json"
    if CONTRACT == "source-boundaries-v1" and replacement.exists():
        sources["baseline-observations"] = reacquired_baseline(
            record, sources, panel, historical.read(replacement))
    else:
        historical.baseline_observations(panel)
    return record, sources


def reacquired_baseline(record, sources, panel, replacement):
    """Replace only absent historical attempts with exact-binary fresh observations.

    Existing files must still match their registered hashes. Surviving attempts,
    including explicit failures, remain immutable; absence is not a successful run.
    The historical checker never opts into this separate evidence overlay.
    """
    require(CONTRACT == "source-boundaries-v1" and replacement.get("version") == 1,
            "baseline reacquisition requires the versioned source-boundary contract")
    require(replacement["registration"] == {
        "path": str((DIRECTORY / "registration.json").relative_to(ROOT)),
        "sha256": hashlib.sha256((DIRECTORY / "registration.json").read_bytes()).hexdigest(),
    }, "baseline reacquisition registration changed")
    index = read_reference(replacement["observations"])
    require(index["original_index"] == record["historical"]["baseline-observations"]
            and index["binary"] == sources["baseline"]["binary"],
            "baseline reacquisition changed the original index or executable")
    read_reference(index["original_index"])
    restoration = read_reference(index["restoration"])
    require(restoration["restored_binary"] == index["binary"],
            "baseline restoration identifies a different executable")
    build = read_reference(restoration["build"])
    require(build["exit_code"] == 0 and build["binary"]["sha256"] == index["binary"]["sha256"],
            "baseline restoration build did not produce the registered executable")
    historical.checked_path(build["binary"])
    historical.checked_path(build["log"])
    original = sources["baseline-observations"]["observations"]
    key = lambda row: (row["pair"], row["repetition"])
    before = {key(row): row for row in original}
    after = {key(row): row for row in index["observations"]}
    declared = {key(row): row for row in index["reacquired"]}
    require(len(before) == len(original) and len(after) == len(index["observations"])
            and len(declared) == len(index["reacquired"]) and before.keys() == after.keys(),
            "baseline reacquisition duplicates or changes registered observation slots")
    missing = {}
    for identity, row in before.items():
        absent = []
        for reference in (row["capture"], row.get("report")):
            if reference is None:
                continue
            try:
                historical.checked_path(reference)
            except FileNotFoundError:
                absent.append(reference)
        if absent:
            missing[identity] = absent
            require(after[identity] != row, "missing baseline attempt was not reacquired")
        else:
            require(after[identity] == row, "surviving baseline attempt was replaced")
    require(missing.keys() == declared.keys() and all(
        declared[identity]["missing_original_references"] == absent
        for identity, absent in missing.items()), "baseline reacquisition absence proof differs")
    observations(index, panel, index["binary"])
    return index


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


def checked_boundary_proposal(result, index):
    accepted = set(result["accepted_correspondences"]) | set(result["text_boundary_correspondences"])
    mandatory = set(result["matching"]["source_only_mandatory"])
    inferred = set(result["matching"]["inferred_proposals"])
    proposals = result["candidates"]["proposals"]

    require(type(index) is int and 0 <= index < len(proposals)
            and index in accepted & mandatory and index not in inferred,
            "interval depends on an unaccepted or inferred boundary")
    value = proposals[index]
    require(all(len(value[side]) == 1 for side in ("old", "new")),
            "interval boundary is not a single retained node")
    return value


def checked_source_cuts(review, result):
    """Bind the declared cut premises; geometric/source adjudication remains required."""
    cuts = review["source_cuts"]
    require(CONTRACT == "source-boundaries-v1"
            and cuts["convention"] == "unique-native-fragment-cuts-v2"
            and cuts["projection"] in ("retained-glyph-ligatures-spacing-v1", "retained-glyph-boundary-padding-v1",
                                       "retained-glyph-whitespace-expansion-v1")
            and not review["boundaries"], "unsupported source-cut contract")

    population = cuts["population"]
    padding = population.get("boundary_padding")
    require(cuts["projection"] == "retained-glyph-whitespace-expansion-v1"
            or (padding is not None) == (cuts["projection"] == "retained-glyph-boundary-padding-v1"),
            "cut padding profile lacks its declared dependencies")
    if padding is not None:
        require(population["kind"] == "matched_interval"
                and padding["convention"] == "optional-clipped-boundary-padding-v1"
                and (padding["old"] or padding["new"]), "unsupported boundary padding census")
        for side in ("old", "new"):
            refs = padding[side]
            atoms = historical.native_sources(refs, side)
            require(len(atoms) == len(refs) and all(ref.get("origin") == "native"
                    and type(ref.get("glyph")) is int and ref["glyph"] >= 0 for ref in refs)
                    and not atoms & historical.native_sources(review[side + "_sources"], side),
                    "uncertain boundary padding enters a compared source range")
    require(population["kind"] in ("complete_page", "matched_interval", "anchored_page"),
            "unknown cut population")
    if population["kind"] == "anchored_page":
        anchor = checked_boundary_proposal(result, population["boundary"])
        require(review.get("native_regions") is None, "page population has segmented dependencies")
        for side in ("old", "new"):
            members = population[side]
            require(type(population[side + "_page"]) is int and population[side + "_page"] >= 0
                    and members and len(set(members)) == len(members)
                    and all(type(node) is int and node >= 0 for node in members)
                    and set(anchor[side]) <= set(members)
                    and set(review["comparison"][side]) <= set(members),
                    "review or page anchor escapes its declared population")
        accepted = ((set(result["accepted_correspondences"])
                     | set(result["text_boundary_correspondences"]))
                    & set(result["matching"]["source_only_mandatory"])
                    - set(result["matching"]["inferred_proposals"]))
        contained = {index for index in accepted
                     if all(len(result["candidates"]["proposals"][index][side]) == 1
                            and result["candidates"]["proposals"][index][side][0] in population[side]
                            for side in ("old", "new"))}
        require(contained == {population["boundary"]}, "page fallback has multiple accepted anchors")
        external = {index for index in accepted
                    if all(len(result["candidates"]["proposals"][index][side]) == 1
                           for side in ("old", "new"))
                    and ((result["candidates"]["proposals"][index]["old"][0] in population["old"])
                         != (result["candidates"]["proposals"][index]["new"][0] in population["new"]))}
        require(len(population["external_boundaries"]) == len(external)
                and set(population["external_boundaries"]) == external,
                "page fallback omits or duplicates external correspondences")
        for side in ("old", "new"):
            excluded = {result["candidates"]["proposals"][index][side][0] for index in external}
            require(not set(review["comparison"][side]) & excluded
                    and all(cuts[name][side]["node"] not in excluded for name in ("entry", "exit")),
                    "page range crosses an external correspondence")
    if population["kind"] == "matched_interval":
        require(population.get("native_regions") == review.get("native_regions"),
                "source cut omits or changes its enclosing region certificate")
        require(len(population["boundaries"]) == 2, "interval lacks outer boundaries")
        outer = [checked_boundary_proposal(result, index) for index in population["boundaries"]]
        for side in ("old", "new"):
            members = population[side]
            require(members and len(set(members)) == len(members)
                    and all(type(node) is int and node >= 0 for node in members)
                    and [members[0], members[-1]] == [value[side][0] for value in outer]
                    and set(review["comparison"][side]) <= set(members),
                    "review escapes its declared cut population")
    row_order = population.get("row_order")
    if row_order is not None:
        require(population["kind"] == "matched_interval"
                and row_order["convention"] in (
                    "horizontal-row-boundaries-v1", "horizontal-paint-row-boundaries-v1")
                and (row_order["old"] is not None or row_order["new"] is not None),
                "unsupported row source order")
        for side in ("old", "new"):
            endpoints = row_order[side]
            if endpoints is None:
                continue
            require(len(endpoints) == 2 and [endpoint["node"] for endpoint in endpoints]
                    == [population[side][0], population[side][-1]],
                    "row endpoints escape the enclosing population")
            refs = [ref for endpoint in endpoints for ref in endpoint["sources"]]
            require(all(endpoint["sources"] for endpoint in endpoints)
                    and all(ref.get("origin") == "native" and type(ref.get("glyph")) is int
                            and ref["glyph"] >= 0 for ref in refs)
                    and len(historical.native_sources(refs, side)) == len(refs)
                    and not historical.native_sources(refs, side)
                    & historical.native_sources(review[side + "_sources"], side),
                    "row endpoints lack distinct native source evidence")
    refinement = cuts.get("edge_refinement")
    if refinement is not None:
        require(refinement["convention"] == "mandatory-literal-space-content-edges-v1"
                and len(refinement["enclosing"]) == 2, "unsupported content edge refinement")
        parents = [candidate for candidate in result.get("text_scope_reviews", [])
                   if candidate.get("source_cuts") is not None
                   and candidate["source_cuts"].get("edge_refinement") is None
                   and candidate["source_cuts"]["population"] == population
                   and [candidate["source_cuts"][name] for name in ("entry", "exit")]
                   == refinement["enclosing"]]
        require(len(parents) == 1, "content edge lacks its retained enclosing comparison")
        parent_review = parents[0]
        checked_source_cuts(parent_review, result)
        for side in ("old", "new"):
            padding_fragments = refinement[side + "_padding"]
            require(len(padding_fragments) == 2, "content edge lacks both padding partitions")
            refs = []
            for fragments in padding_fragments:
                for fragment in fragments:
                    require(type(fragment["node"]) is int and fragment["node"] >= 0
                            and len(fragment["tokens"]) == 2
                            and all(type(index) is int for index in fragment["tokens"])
                            and 0 <= fragment["tokens"][0] < fragment["tokens"][1]
                            and fragment["sources"], "invalid content edge fragment")
                    refs.extend(fragment["sources"])
            removed = historical.native_sources(refs, side)
            body = historical.native_sources(review[side + "_sources"], side)
            enclosing = historical.native_sources(parent_review[side + "_sources"], side)
            require(len(removed) == len(refs) and body and not removed & body
                    and removed | body == enclosing, "content edge loses or reuses enclosing sources")
            before = parent_review["comparison"]["operation"].get(side)
            after = review["comparison"]["operation"].get(side)
            require(isinstance(before, str) and isinstance(after, str)
                    and before.strip(" ") == after.strip(" ") and len(before) >= len(after)
                    and (padding_fragments[0] or before.startswith(after))
                    and (padding_fragments[1] or before.endswith(after)),
                    "content edge changes interior text")
        for index, name in enumerate(("entry", "exit")):
            changed = bool(refinement["old_padding"][index] or refinement["new_padding"][index])
            require((cuts[name]["evidence"]["kind"] == "corresponding_content_edge") if changed
                    else cuts[name] == refinement["enclosing"][index],
                    "content edge evidence disagrees with its padding")
            for side in ("old", "new"):
                fragments = refinement[side + "_padding"][index]
                if fragments:
                    fragment = fragments[-1] if index == 0 else fragments[0]
                    expected = {"node": fragment["node"],
                                "token_boundary": fragment["tokens"][1 if index == 0 else 0]}
                else:
                    expected = refinement["enclosing"][index][side]
                require(cuts[name][side] == expected, "content cut is not its padding edge")
        require(any(refinement["old_padding"]) or any(refinement["new_padding"]),
                "content refinement has no removed padding")
    for name in ("entry", "exit"):
        boundary = cuts[name]
        evidence = boundary["evidence"]
        require(evidence["kind"] in ("accepted_boundary", "unique_native_fragment")
                or (refinement is not None and evidence["kind"] == "corresponding_content_edge"),
                "unknown cut evidence")
        accepted_node = checked_boundary_proposal(result, evidence["proposal"]) if evidence["kind"] == "accepted_boundary" else None
        for side in ("old", "new"):
            cut = boundary[side]
            require(type(cut["node"]) is int and cut["node"] >= 0
                    and type(cut["token_boundary"]) is int and cut["token_boundary"] >= 0,
                    "invalid source-cut location")
            if population["kind"] in ("matched_interval", "anchored_page"):
                require(cut["node"] in population[side], "cut escapes its declared population")
            if accepted_node is not None:
                require(accepted_node[side] == [cut["node"]], "cut disagrees with accepted node")
            elif evidence["kind"] == "unique_native_fragment":
                fragment = evidence[side]
                if padding is not None:
                    require(not historical.native_sources(fragment["sources"], side)
                            & historical.native_sources(padding[side], side),
                            "uncertain padding supplies an equal boundary fragment")
                extent = fragment["tokens"]
                require(fragment["node"] == cut["node"] and len(extent) == 2
                        and all(type(position) is int for position in extent)
                        and 0 <= extent[0] < extent[1] and cut["token_boundary"] in extent
                        and historical.native_sources(fragment["sources"], side),
                        "cut lacks a finite source fragment")


def checked_native_regions(review, result):
    """Bind region declarations; retained source adjudication proves their premises."""
    chains = review["native_regions"]
    require(CONTRACT == "source-boundaries-v1" and set(chains) == {"old", "new"}
            and any(chains.values()), "unsupported native region contract")
    if review.get("source_cuts") is not None:
        population = review["source_cuts"]["population"]
        require(population["kind"] == "matched_interval"
                and population.get("native_regions") == chains, "region population differs")
        members = {side: population[side] for side in chains}
    else:
        require(len(review["boundaries"]) == 2, "region chain lacks matched endpoints")
        outer = [checked_boundary_proposal(result, index) for index in review["boundaries"]]
        members = {side: outer[0][side] + review["comparison"][side] + outer[1][side]
                   for side in chains}
    for side, chain in chains.items():
        if chain is None:
            continue
        require(chain["convention"] in ("native-tag-ordered-page-regions-v1",
                                        "native-k-parent-bound-page-regions-v1"),
                "unknown native transition profile")
        regions, transitions = chain["regions"], chain["transitions"]
        nodes = [node for region in regions for node in region["nodes"]]
        require(len(regions) >= 2 and len(transitions) == len(regions) - 1
                and nodes == members[side] and len(nodes) == len(set(nodes))
                and all(region["nodes"] and type(region["page"]) is int and region["page"] >= 0
                        and type(region["bounded_paint"]) is bool for region in regions)
                and all(a["page"] != b["page"] for a, b in zip(regions, regions[1:])),
                "invalid native page-region partition")
        last = 0
        structure = transitions[0]["structure"]
        require(structure.get("origin") == "structured" and type(structure.get("element")) is int
                and structure["element"] >= 0, "transition lacks native structure reference")
        for transition in transitions:
            require(transition["structure"] == structure and type(transition["position"]) is int
                    and transition["position"] > last
                    and transition["before"] != transition["after"]
                    and len(historical.native_sources([transition["before"], transition["after"]], side)) == 2,
                    "invalid declared structure adjacency")
            last = transition["position"]


def checked_interval_presence(review, result):
    presence = review["presence"]
    require(CONTRACT == "source-boundaries-v1"
            and presence["convention"] == "closed-native-interval-presence-v1"
            and presence["present"] in ("old", "new"), "unsupported interval presence")
    present = presence["present"]
    absent = "new" if present == "old" else "old"
    comparison = review["comparison"]
    operation = comparison["operation"]
    require(operation["kind"] == "text_changed" and operation[absent] == ""
            and isinstance(operation[present], str) and operation[present]
            and comparison[absent] == [] and comparison[present]
            and review[absent + "_sources"] == [] and review[present + "_sources"],
            "presence does not describe one empty source interval")
    require(all(len(review[side + "_boundaries"]) == 2 and all(review[side + "_boundaries"])
                for side in ("old", "new")), "empty interval lacks independent endpoints")
    if review.get("source_cuts") is None:
        require(len(review["boundaries"]) == 2 and len(set(review["boundaries"])) == 2,
                "empty interval lacks distinct boundary correspondences")
        for index in review["boundaries"]:
            checked_boundary_proposal(result, index)


def checked_spacing_change(review):
    """Check the explicit partial-mask contract; source reviews still bind raw evidence."""
    spacing = review.get("spacing")
    comparison = review["comparison"]
    operation = comparison["operation"]
    require(spacing and spacing["convention"] == "source-space-interpretations-v1"
            and operation["kind"] == "text_changed", "missing partial spacing proof contract")
    for side in ("old", "new"):
        text = operation[side]
        require(isinstance(text, str), "partial spacing proof lacks literal review text")
        positions = [boundary["position"] for boundary in spacing[side]]
        require(positions == [index for index, scalar in enumerate(text) if scalar in " \t\n\r\x0c"],
                "spacing provenance omits or duplicates a boundary")
        sources = historical.native_sources(review[side + "_sources"], side)
        for boundary in spacing[side]:
            require(boundary["origin"] in ("literal_glyph", "reconstructed_gap", "line_separator",
                                           "page_separator", "ambiguous")
                    and boundary["sources"]
                    and historical.native_sources(boundary["sources"], side) <= sources,
                    "spacing provenance lies outside the review sources")
    proof = comparison.get("text_change_proof")
    if proof is not None:
        token = proof["token"]
        require(isinstance(token, dict) and set(token) == {"Scalar"}
                and isinstance(token["Scalar"], str) and len(token["Scalar"]) == 1
                and token["Scalar"] not in " \t\n\r\x0c",
                "multiplicity witness has no reviewable scalar")
        for side in ("old", "new"):
            required, possible = proof[side + "_required"], proof[side + "_possible"]
            require(type(required) is int and type(possible) is int
                    and 0 <= required <= possible == operation[side].count(token["Scalar"]),
                    "multiplicity counts disagree with review text")
        require(proof["old_required"] > proof["new_possible"]
                or proof["new_required"] > proof["old_possible"], "multiplicity witness does not prove change")
    else:
        mask = comparison.get("text_mask")
        require(mask and mask["claims"]["changed_source_lower"] > 0,
                "partial spacing claim has no independent content proof")


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
            if review.get("source_cuts") is not None:
                checked_source_cuts(review, result)
            if review.get("presence") is not None:
                checked_interval_presence(review, result)
            if review.get("native_regions") is not None:
                checked_native_regions(review, result)
            category = "B" if comparison["interpretation"] == "conditional_on_correspondence" else "C"
            if category == "B":
                require(comparison["compared"], "B operation has no compared content")
                if comparison["unresolved"]:
                    require(CONTRACT == "source-boundaries-v1", "B operation retains unresolved comparison")
                    checked_spacing_change(review)
            sources = set().union(*(historical.native_sources(review[side + "_sources"], side)
                                    for side in ("old", "new")))
            rows.append({"category": category, "sources": sources,
                         "operation": comparison["operation"], "review": review,
                         "source_projection": {key: review[key] for key in (
                             "old_sources", "new_sources", "old_boundaries", "new_boundaries", "convention")},
                         "pointer": f"{prefix}/text_scope_reviews/{index}"})
            if review.get("spacing") is not None:
                rows[-1]["source_projection"]["spacing"] = review["spacing"]
            if review.get("source_cuts") is not None:
                rows[-1]["source_projection"]["source_cuts"] = review["source_cuts"]
            if review.get("presence") is not None:
                rows[-1]["source_projection"]["presence"] = review["presence"]
            if review.get("native_regions") is not None:
                rows[-1]["source_projection"]["native_regions"] = review["native_regions"]
            if comparison.get("text_change_proof") is not None:
                rows[-1]["source_projection"]["text_change_proof"] = comparison["text_change_proof"]
    counts = Counter(row["category"] for row in rows)
    require(counts["A"] == report["typed_changes"],
            "strict report contains an unhandled nonlocal event; extend the source adapter")
    require(counts["B"] == report.get("scope_content_changes", 0), "B event denominator mismatch")
    require(counts["C"] == report.get("inferred_changes", 0) + report.get("inferred_scope_changes", 0),
            "inferred report contains an unhandled event; extend the source adapter")
    return rows


def range_recovery(review, core, extent):
    """Retain finite source bounds while separating spacing from the proved change."""
    comparison = review["comparison"]
    if CONTRACT == "source-boundaries-v1" and comparison["unresolved"]:
        checked_spacing_change(review)
        # Only the historical local-change eligibility predicate is supplied its
        # separately checked premise. The report, boundaries and extents remain
        # untouched, and this never affects inventory or document completion.
        review = dict(review, comparison=dict(comparison, unresolved=[]))
    result = historical.range_recovery(review, core, extent)
    if CONTRACT == "source-boundaries-v1" and review.get("presence") is not None:
        present = review["presence"]["present"]
        sources = set().union(*(historical.native_sources(review[side + "_sources"], side)
                                for side in ("old", "new")))
        comparison = review["comparison"]
        result["source_range_hit"] = bool(core and all(side == present for side, _ in core | extent)
            and comparison["interpretation"] == "conditional_on_correspondence"
            and comparison["compared"] and not comparison["unresolved"]
            and comparison["operation"]["kind"] == "text_changed" and core <= sources <= extent)
    return result


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
        scored = [event for event in current if event["category"] == "A" and event["sources"] & extent]
        correct &= len(scored) <= 1 and all(event["sources"] == strict_gold["sources"]
                                           and event["operation"] == strict_gold["operation"] for event in scored)

    def hits(rows):
        found = set()
        for event in rows:
            if event["category"] == "B" and range_recovery(
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


def strict_control_evidence(detected, gold, target, maximum, reviews, partial_gold=False):
    """Check exact gold locally and require source review outside partial gold.

    The real-source mutations annotate one numeric target, not every change in
    the two annual forms. Their other outputs are never implicitly accepted.
    Authored numeric controls retain their complete event/source gold.
    """
    correct, counted, consumed = True, 0, set()
    for event in detected:
        if event["category"] != "A":
            continue
        covered = gold is not None and (not partial_gold or bool(event["sources"] & target))
        if covered:
            counted += 1
            correct &= event["sources"] == gold
        else:
            review = reviews.get(event["pointer"])
            correct &= bool(review and review["event_sha256"] == event_digest(event))
            if review:
                consumed.add(event["pointer"])
    if gold is not None:
        correct &= counted <= maximum
    return correct, consumed


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
        strict_correct, consumed = strict_control_evidence(
            detected, gold, target, expected[name]["strict_events"],
            {pointer: review for (pair, channel, pointer), review in adjudicated.items()
             if (pair, channel) == (name, route)},
            partial_gold=CONTRACT == "source-boundaries-v1" and name in real)
        for pointer in consumed:
            adjudicated.pop((name, route, pointer))
        unproved_strict = not strict_correct
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
        if CONTRACT == "source-boundaries-v1":
            premises = row.get("premises", {})
            require(set(premises) == {"acquisition", "normalization", "boundary_discovery",
                                     "boundary_correspondence", "source_order_closure", "competitor_closure",
                                     "local_change_proof", "finite_extent"}, "independent target premises are missing")
            for premise in premises.values():
                require(premise["status"] in ("proved", "failed", "unresolved", "not_evaluated")
                        and premise["reason"], "invalid independent premise status")
                require(premise["status"] == "not_evaluated" or premise["evidence"],
                        "observed premise has no evidence")
                for reference in premise["evidence"]:
                    historical.checked_path(reference)
        require(row["stage"] in ("acquisition", "normalization", "scope", "retrieval",
                                 "optimization", "counterpart", "localization", "reporting"),
                "unknown earliest blocker stage")
        require(row["reason"] and row["next_action"] and row["evidence"],
                "diagnosis lacks an observation or next action")
        for reference in row["evidence"]:
            historical.checked_path(reference)
        if row["stage"] != "reporting":
            require(row["counterexample"]["case"], "diagnosis lacks a minimal counterexample")
            historical.checked_path(row["counterexample"]["reference"])
    return data


def missing_gates():
    return gate_summary([], [], {}, {}, set(), set(), False, False)


def recovery_summary(rows):
    recovered = {row["pair"]: row["producer"] for row in rows if row["additional_categories"]}
    return recovered, {row["pair"] for row in rows if row["baseline_complete"]}, {
        row["pair"] for row in rows if row["current_complete"]}


def main():
    global DIRECTORY, CONTRACT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stage", choices=STAGES, required=True)
    parser.add_argument("--contract", choices=("historical", "source-boundaries-v1"), default="historical")
    parser.add_argument("--evidence-dir", type=Path,
                        help="Separate evidence directory for the source-boundaries contract")
    args = parser.parse_args()
    CONTRACT = args.contract
    if args.evidence_dir is not None:
        if CONTRACT == "historical":
            parser.error("historical evidence directory cannot be redirected")
        DIRECTORY = args.evidence_dir.resolve()
        if not DIRECTORY.is_relative_to(ROOT / "benchmark" / "realworld"):
            parser.error("evidence directory must remain inside benchmark/realworld")
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
        development_gates = ("G1", "G4") if CONTRACT == "source-boundaries-v1" else ("G1", "G3", "G4")
        require(all(gates[key]["passed"] for key in development_gates),
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
        require(freeze["annotation_completed_utc"] <= blind["comparison_started_utc"]
                and freeze["annotation_completed_utc"] <= blind["baseline_comparison_started_utc"],
                "blind comparisons began before source annotation was frozen")
        baseline = read_reference(freeze["baseline_observations"])
        blind_rows = phase(blind, panel, targets, baseline, sources["baseline"]["binary"])
        blind_recovered, _, _ = recovery_summary(blind_rows)
        gates["G2"] = historical.recovery_gate(blind_recovered, blind_recovered, 3, 2)
        gates["G4"]["passed"] &= all(row["correct"] for row in blind_rows)
        print(json.dumps({"blind": family_metrics(blind_rows)}, indent=2))
        require(gates["G2"]["passed"] and gates["G4"]["passed"], "blind recovery or correctness gate unmet")
        gates["G5"]["passed"] = True
        print(json.dumps(gates, indent=2))
        if CONTRACT == "source-boundaries-v1":
            print(json.dumps({"contract": CONTRACT, "recovery_track_complete": True,
                              "equivalence_track_complete": False, "goal_complete": False,
                              "note": "This entry point verifies R only; E and original-purpose cleanup remain separate."}))
        return 0
    except (OSError, ValueError, KeyError, TypeError, IndexError) as error:
        print(json.dumps(gates, indent=2))
        print(f"FAIL {args.stage}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
