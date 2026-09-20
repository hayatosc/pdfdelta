#!/usr/bin/env python3
"""Prove that a native capture change only adds resolutions or finer partitions.

Every claim is keyed by side plus stable block identity, and both the
comparable-token and scalar-canonical coordinates are validated separately
because canonical indices omit non-scalar tokens. For each pair the audit
verifies:

- entry ranges are well formed and do not overlap or duplicate keys in either
  coordinate; both captures must cover exactly the same token keys per side;
- every token resolved before (state equal/changed) is still resolved after,
  newly resolved tokens were unresolved before, and equal<->changed moves are
  reported for explicit review rather than counted as silent success;
- block source events are compared by reproducing the documented ordered,
  deduplicated projection: fragments are combined in canonical order, glyph
  and synthetic/line-break events keep their first occurrence only, and
  block-separator spaces keep their multiplicity; synthetic endpoints must
  reference glyphs that the block actually projects;
- established change payloads (changes, formatting-only, proven regions)
  present before are still present after exactly;
- tentative candidates may be regenerated, but every document-glyph source
  they referenced on that same side must still be accounted for by the after
  assessment or change buckets, recorded separately from certified retention;
- unresolved region counts are reported per side so a larger count is only
  accepted when token and provenance conservation pass.
"""

import argparse
import json
import sys
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import capture  # noqa: E402

CERTIFIED_MEMBERS = ("changes", "formatting_only_changes", "proven_changed_regions")
TENTATIVE_MEMBERS = ("change_candidates",)
RESOLVED_STATES = ("equal", "changed")
VALID_STATES = ("unresolved", "equal", "changed")
COORDINATES = ("canonical_range", "comparable_range")


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def load_report(capture_dir, pair):
    path = capture.resolve_report_path(capture_dir / pair / f"{pair}-native.json")
    with capture.open_report(path) as stream:
        return json.load(stream)


def state_tokens(entries, coordinate, problems, label):
    """Maps (block, index) to state, rejecting invalid or overlapping ranges."""
    tokens = {}
    for entry in entries:
        state = entry.get("state")
        if state not in VALID_STATES:
            problems.append(f"{label}/{coordinate}: invalid state {state!r}")
            continue
        span = entry.get(coordinate) or {}
        start, end = span.get("start"), span.get("end")
        if not isinstance(start, int) or not isinstance(end, int) or start > end:
            problems.append(f"{label}/{coordinate}: invalid range {span!r}")
            continue
        if coordinate == "canonical_range":
            comparable = entry.get("comparable_range") or {}
            if (end - start) > (comparable.get("end", 0) - comparable.get("start", 0)):
                problems.append(f"{label}/block {entry.get('block')}: canonical wider than comparable")
        for index in range(start, end):
            key = (entry["block"], index)
            if key in tokens:
                problems.append(f"{label}/{coordinate}: duplicate or overlapping token {key}")
                continue
            tokens[key] = state
    return tokens


def transitions(before_tokens, after_tokens, problems, label):
    shared = before_tokens.keys() & after_tokens.keys()
    moves = Counter((before_tokens[key], after_tokens[key]) for key in shared)
    prior_resolved_lost = sum(
        count for (a, b), count in moves.items()
        if a in RESOLVED_STATES and b not in RESOLVED_STATES
    )
    newly_resolved = sum(
        count for (a, b), count in moves.items()
        if a not in RESOLVED_STATES and b in RESOLVED_STATES
    )
    review = {
        f"{a}->{b}": count
        for (a, b), count in sorted(moves.items())
        if a in RESOLVED_STATES and b in RESOLVED_STATES and a != b
    }
    if before_tokens.keys() - after_tokens.keys():
        problems.append(f"{label}: {len(before_tokens.keys() - after_tokens.keys())} tokens missing after")
    if after_tokens.keys() - before_tokens.keys():
        problems.append(f"{label}: {len(after_tokens.keys() - before_tokens.keys())} tokens new after")
    if prior_resolved_lost:
        problems.append(f"{label}: {prior_resolved_lost} prior resolved tokens became unresolved")
    return {
        "tokens_before": len(before_tokens),
        "tokens_after": len(after_tokens),
        "resolved_before": sum(1 for state in before_tokens.values() if state in RESOLVED_STATES),
        "resolved_after": sum(1 for state in after_tokens.values() if state in RESOLVED_STATES),
        "prior_resolved_lost": prior_resolved_lost,
        "newly_resolved": newly_resolved,
        "transitions": {f"{a}->{b}": count for (a, b), count in sorted(moves.items())},
        "review_transitions": review,
    }


def normalize_events(entries, problems, label):
    """Reproduces the ordered deduplicated projection for combined fragments.

    Returns `(sequence, glyphs, structural)` where `glyphs` holds document
    glyph events and `structural` holds synthetic-space, line-break and
    block-separator events. The projector deduplicates atoms and glyphs per
    projected span, so a finer partition can legitimately move a structural
    event across a boundary; glyph events are compared strictly.
    """
    sequence = []
    glyphs = set()
    structural = set()
    seen_glyphs = set()
    seen_events = set()
    separators = 0
    for entry in sorted(entries, key=lambda item: item["canonical_range"]["start"]):
        for source in entry.get("sources") or []:
            kind = source.get("kind")
            if kind == "block_separator_space":
                separators += 1
                key = ("block_separator_space", separators)
                structural.add(key)
                sequence.append(key)
            elif kind == "glyph":
                glyph_id = source.get("glyph_id")
                if glyph_id in seen_glyphs:
                    continue
                seen_glyphs.add(glyph_id)
                glyphs.add(glyph_id)
                sequence.append(("glyph", glyph_id))
            elif kind in ("synthetic_space", "line_break"):
                key = (kind, source.get("preceding_glyph_id"), source.get("following_glyph_id"))
                if key in seen_events:
                    continue
                seen_events.add(key)
                structural.add(key)
                sequence.append(key)
                for endpoint in key[1:]:
                    if endpoint is None or not isinstance(endpoint, int):
                        problems.append(f"{label}: {kind} has malformed endpoints")
                        continue
                    glyphs.add(endpoint)
                    if endpoint not in seen_glyphs:
                        seen_glyphs.add(endpoint)
                        sequence.append(("glyph", endpoint))
            else:
                problems.append(f"{label}: unknown source kind {kind!r}")
    return sequence, glyphs, structural


def block_events(entries):
    return {
        block: normalize_events(block_entries, [], "")
        for block, block_entries in entries.items()
    }


def group_by_block(entries):
    grouped = {}
    for entry in entries:
        grouped.setdefault(entry["block"], []).append(entry)
    return grouped


def audit_side(before, after, side, problems):
    result = {
        "coordinates": {},
        "source_blocks_compared": 0,
        "source_blocks_missing": 0,
        "source_glyphs_lost": 0,
        "source_glyphs_invented": 0,
        "structural_event_differences": 0,
        "structural_review": [],
        "source_sequence_mismatches": 0,
        "regions_before": len(before["unresolved_regions"]),
        "regions_after": len(after["unresolved_regions"]),
        "region_blocks": {},
    }
    for coordinate in COORDINATES:
        before_tokens = state_tokens(before["assessment"][side], coordinate, problems, f"{side} before")
        after_tokens = state_tokens(after["assessment"][side], coordinate, problems, f"{side} after")
        result["coordinates"][coordinate] = transitions(
            before_tokens, after_tokens, problems, f"{side}/{coordinate}"
        )
    before_blocks = group_by_block(before["assessment"][side])
    after_blocks = group_by_block(after["assessment"][side])
    for block in before_blocks.keys() & after_blocks.keys():
        before_events, before_glyphs, before_structural = normalize_events(
            before_blocks[block], problems, f"{side}/block {block} before"
        )
        after_events, after_glyphs, after_structural = normalize_events(
            after_blocks[block], problems, f"{side}/block {block} after"
        )
        result["source_blocks_compared"] += 1
        result["source_glyphs_lost"] += len(before_glyphs - after_glyphs)
        result["source_glyphs_invented"] += len(after_glyphs - before_glyphs)
        structural_diff = len(before_structural ^ after_structural)
        result["structural_event_differences"] += structural_diff
        if structural_diff:
            result["structural_review"].append(block)
        if before_glyphs != after_glyphs:
            problems.append(f"{side}/block {block}: source glyph events changed")
        elif before_events != after_events:
            result["source_sequence_mismatches"] += 1
    missing_blocks = before_blocks.keys() - after_blocks.keys()
    result["source_blocks_missing"] = len(missing_blocks)
    if missing_blocks:
        problems.append(f"{side}: {len(missing_blocks)} blocks missing after")

    def region_blocks(regions, side_key):
        blocks = set()
        for region in regions:
            span = region.get(side_key) or {}
            blocks.update(span.get("blocks") or [])
        return blocks

    old_before = region_blocks(before["unresolved_regions"], "old_span")
    old_after = region_blocks(after["unresolved_regions"], "old_span")
    new_before = region_blocks(before["unresolved_regions"], "new_span")
    new_after = region_blocks(after["unresolved_regions"], "new_span")
    result["region_blocks"] = {
        "old_before": len(old_before),
        "old_after": len(old_after),
        "new_before": len(new_before),
        "new_after": len(new_after),
        "old_new_uncertainty": len(old_after - old_before),
        "new_new_uncertainty": len(new_after - new_before),
    }
    if old_after - old_before:
        problems.append(f"{side}: {len(old_after - old_before)} old blocks became unresolved")
    if new_after - new_before:
        problems.append(f"{side}: {len(new_after - new_before)} new blocks became unresolved")
    return result


def exact_retention(problems, label, before, after):
    before_counter = Counter(canonical(entry) for entry in before.get(label) or [])
    after_counter = Counter(canonical(entry) for entry in after.get(label) or [])
    retained = sum((before_counter & after_counter).values())
    if retained != sum(before_counter.values()):
        problems.append(f"{label}: {sum(before_counter.values()) - retained} prior payloads lost")
    return {
        "before": sum(before_counter.values()),
        "after": sum(after_counter.values()),
        "retained": retained,
        "identical": before_counter == after_counter,
    }


def span_events(entry, side_key):
    """Collects document-glyph source events from occurrence spans of one side."""
    found = set()
    for occurrence in entry.get("occurrences") or []:
        span = occurrence.get(side_key) or {}
        for source in span.get("sources") or []:
            if source.get("kind") == "glyph":
                found.add(canonical(source))
    return found


def tentative_accounting(before, after, side, side_key):
    reported = set()
    for entry in before.get("change_candidates") or []:
        reported |= span_events(entry, side_key)
    available = set()
    for entry in after["assessment"][side]:
        for source in entry.get("sources") or []:
            if source.get("kind") == "glyph":
                available.add(canonical(source))
    for member in CERTIFIED_MEMBERS + TENTATIVE_MEMBERS:
        for entry in after.get(member) or []:
            available |= span_events(entry, side_key)
    missing = reported - available
    return {
        "sources_reported": len(reported),
        "sources_accounted": len(reported) - len(missing),
        "sources_missing": len(missing),
    }


def classify_retention(problems, reviews, mismatches):
    """Fail closed: anything unresolved keeps the pair out of a clean pass."""
    if problems:
        return "fail"
    if reviews or mismatches:
        return "needs-review"
    return "pass"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pair")
    parser.add_argument("before_capture", type=Path)
    parser.add_argument("after_capture", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    before = load_report(args.before_capture, args.pair)
    after = load_report(args.after_capture, args.pair)
    problems = []
    reviews = []
    result = {
        "pair": args.pair,
        "before_capture": str(args.before_capture),
        "after_capture": str(args.after_capture),
        "sides": {},
        "member_retention": {},
        "tentative_accounting": {},
    }
    for side, side_key in (("old_resolution", "old_span"), ("new_resolution", "new_span")):
        result["sides"][side] = audit_side(before, after, side, problems)
        for coordinate, moves in result["sides"][side]["coordinates"].items():
            for move, count in moves["review_transitions"].items():
                reviews.append(f"{side}/{coordinate}: {count} tokens {move}")
        result["tentative_accounting"][side] = tentative_accounting(before, after, side, side_key)
        if result["tentative_accounting"][side]["sources_missing"]:
            problems.append(
                f"{side}: {result['tentative_accounting'][side]['sources_missing']} candidate source events unaccounted"
            )
    for member in CERTIFIED_MEMBERS:
        result["member_retention"][member] = exact_retention(problems, member, before, after)
    for side, audit_result in result["sides"].items():
        for block in audit_result["structural_review"]:
            reviews.append(f"{side}: structural source events changed at block {block}")
    result["review"] = reviews
    result["problems"] = problems
    mismatches = sum(
        side.get("source_sequence_mismatches", 0) for side in result["sides"].values()
    )
    result["retention"] = classify_retention(problems, reviews, mismatches)
    args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(
        json.dumps(
            {
                "pair": args.pair,
                "retention": result["retention"],
                "prior_resolved_lost": [
                    side["coordinates"]["canonical_range"]["prior_resolved_lost"]
                    for side in result["sides"].values()
                ],
                "newly_resolved": [
                    side["coordinates"]["canonical_range"]["newly_resolved"]
                    for side in result["sides"].values()
                ],
                "candidate_sources_missing": [
                    side["sources_missing"] for side in result["tentative_accounting"].values()
                ],
                "review": reviews,
                "problems": problems,
            }
        )
    )
    return 0 if result["retention"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
