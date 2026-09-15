#!/usr/bin/env python3
"""Audit whole-parent cut content and optional-space masks from a source bundle.

This bounded observation checks native readings, partition conservation and all
optimal masks for each spacing interpretation. It does not independently prove
boundary correspondence, paint closure, native acquisition or global inventory.
It rejects contracted glyphs, partial parents and non-spacing normalization.
"""
import argparse
import itertools
import json
from pathlib import Path

from audit_native_intervals import minimal_masks, native_ids, reference, self_test


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    source = json.loads(args.sources.read_text())
    report = json.loads(args.report.read_text())
    records = []
    for scope in report["comparison"]["scopes"]:
        for interval in scope["result"].get("native_text_intervals", []):
            comparison = interval["comparison"]
            assert comparison["compared"]
            assert comparison["interpretation"] == "conditional_on_correspondence"
            families, views = {}, {}
            record = {"sides": {}}
            for side in ("old", "new"):
                assert source[side]["summary"]["revision"] == report[side]["revision"]
                glyphs = {g["id"]: g for g in source[side]["native"]["items"]}
                assert len(glyphs) == report[side]["native_glyphs"]
                nodes = {n["id"]: n for n in source[side]["graph"]["nodes"]}
                owned = native_ids(interval[side + "_sources"])
                assert len(owned) == len(set(owned))
                selected = set()
                for remainder in interval["cut_partition"][side + "_remainder"]:
                    whole = native_ids(nodes[remainder["parent"]]["sources"])
                    rest = native_ids(remainder["sources"])
                    part = set(whole) & set(owned)
                    assert len(whole) == len(set(whole)) and len(rest) == len(set(rest))
                    assert part.isdisjoint(rest) and part | set(rest) == set(whole)
                    assert selected.isdisjoint(part)
                    selected.update(part)
                assert selected == set(owned)
                assert len(comparison[side]) == 1
                node = nodes[comparison[side][0]]
                assert native_ids(node["sources"]) == owned
                view = node["content"]["view"]
                assert view["normalization"]["kind"] == "exact"
                text = "".join(t["Scalar"] for t in view["tokens"])
                assert text == comparison["operation"][side]
                literal = []
                optional = []
                for pos, (char, origins, backed) in enumerate(zip(
                        text, view["origins"], view["source_backed"], strict=True)):
                    identifiers = native_ids(origins)
                    if backed:
                        assert len(identifiers) == 1
                        assert glyphs[identifiers[0]]["text"] == {"Mapped": char}
                        literal.extend(identifiers)
                    else:
                        assert char == " " and set(identifiers) <= set(owned)
                        optional.append(pos)
                assert literal == owned and len(optional) <= 8
                families[side] = []
                for bits in itertools.product((False, True), repeat=len(optional)):
                    removed = {pos for pos, present in zip(optional, bits) if not present}
                    positions = [i for i in range(len(text)) if i not in removed]
                    families[side].append(("".join(text[i] for i in positions), positions))
                views[side] = view
                record["sides"][side] = {"text": text, "owned_sources": len(owned),
                                          "optional_positions": optional}
            masks = {s: [True] * len(views[s]["tokens"]) for s in views}
            costs = []
            for (a, ap), (b, bp) in itertools.product(families["old"], families["new"]):
                cost, am, bm = minimal_masks(a, b)
                costs.append(cost)
                for side, positions, mask in (("old", ap, am), ("new", bp, bm)):
                    changed = {pos for pos, value in zip(positions, mask) if value}
                    masks[side] = [value and pos in changed for pos, value in enumerate(masks[side])]
            actual = comparison["text_mask"]
            claims = actual["claims"]
            assert claims["normalization_pairs"] == len(costs)
            assert (claims["changed_source_lower"], claims["changed_source_upper"]) == (min(costs), max(costs))
            for side, mask in masks.items():
                assert claims["mandatory_" + side] == mask
                expected = [{"position": pos, "sources": views[side]["origins"][pos]}
                            for pos, value in enumerate(mask)
                            if value and views[side]["source_backed"][pos]]
                assert actual[side] == expected
            record["costs"] = costs
            record["unresolved"] = comparison["unresolved"]
            records.append(record)
    assert records
    args.output.write_text(json.dumps({"scope": __doc__, "report": reference(args.report),
        "sources": reference(args.sources), "oracle_self_test_cases": self_test(),
        "intervals": records}, indent=2, ensure_ascii=False) + "\n")
    print(len(records), "optional intervals verified")


if __name__ == "__main__":
    main()
