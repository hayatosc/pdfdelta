#!/usr/bin/env python3
"""Independently check literal interval masks against frozen native worker output.

This checks readings, source membership and all optimal literal edit masks. It
does not certify boundary correspondence, paint closure or complete inventory.
Only worker format 8 and literal, unnormalized native readings are admitted.
"""
import argparse
import base64
import gzip
import hashlib
import io
import itertools
import json
from pathlib import Path


def reference(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "sha256": digest}


def native_ids(sources):
    assert all(source["origin"] == "native" for source in sources)
    return [source["glyph"] for source in sources]


def read_native(path, summary):
    response = json.loads(path.read_text())
    value = response["Ok"]
    assert value["version"] == 8
    store = value["store"]
    assert store["revision"] == summary["revision"]
    block = store["native"]["items"]
    assert block["uncompressed_bytes"] <= 128 * 1024 * 1024
    with gzip.GzipFile(fileobj=io.BytesIO(base64.b64decode(block["compressed"], validate=True))) as stream:
        raw = stream.read(block["uncompressed_bytes"] + 1)
    assert len(raw) == block["uncompressed_bytes"]
    items = json.loads(raw)
    assert len(items) == block["glyph_count"] == summary["native_glyphs"]
    glyphs = {}
    for identifier, atom, *_ in items:
        assert identifier not in glyphs
        text, code = store["atoms"][atom] if isinstance(atom, int) else atom
        glyphs[identifier] = {"text": text, "raw_code": code}
    return glyphs


def minimal_masks(old, new):
    def distances(a, b):
        rows = [list(range(len(b) + 1))]
        for i, left in enumerate(a):
            row = [i + 1]
            for j, right in enumerate(b):
                cost = min(rows[-1][j + 1] + 1, row[-1] + 1)
                if left == right:
                    cost = min(cost, rows[-1][j])
                row.append(cost)
            rows.append(row)
        return rows
    assert (len(old) + 1) * (len(new) + 1) <= 1_000_000
    forward = distances(old, new)
    reverse = distances(old[::-1], new[::-1])
    cost = forward[-1][-1]
    mandatory_old = [True] * len(old)
    mandatory_new = [True] * len(new)
    for i, left in enumerate(old):
        for j, right in enumerate(new):
            if left == right and forward[i][j] + reverse[len(old) - i - 1][len(new) - j - 1] == cost:
                mandatory_old[i] = False
                mandatory_new[j] = False
    return cost, mandatory_old, mandatory_new


def self_test():
    def paths(a, b, i=0, j=0):
        if i == len(a) and j == len(b):
            return [(0, set(), set())]
        result = []
        if i < len(a):
            result.extend((cost + 1, old | {i}, new) for cost, old, new in paths(a, b, i + 1, j))
        if j < len(b):
            result.extend((cost + 1, old, new | {j}) for cost, old, new in paths(a, b, i, j + 1))
        if i < len(a) and j < len(b) and a[i] == b[j]:
            result.extend(paths(a, b, i + 1, j + 1))
        return result
    strings = ["".join(chars) for length in range(4) for chars in itertools.product("ab", repeat=length)]
    for a, b in itertools.product(strings, repeat=2):
        all_paths = paths(a, b)
        best = min(cost for cost, _, _ in all_paths)
        optimal = [(old, new) for cost, old, new in all_paths if cost == best]
        expected = (best, [all(i in old for old, _ in optimal) for i in range(len(a))],
                    [all(j in new for _, new in optimal) for j in range(len(b))])
        assert minimal_masks(a, b) == expected
    return len(strings) ** 2


def audit(report_path, old_path, new_path, output):
    report = json.loads(report_path.read_text())
    native = {side: read_native(path, report[side]) for side, path in [("old", old_path), ("new", new_path)]}
    seen = {"old": set(), "new": set()}
    records = []
    for scope in report["comparison"]["scopes"]:
        for interval in scope["result"].get("native_text_intervals", []):
            comparison = interval["comparison"]
            assert comparison["compared"] and not comparison["unresolved"]
            assert comparison["interpretation"] == "conditional_on_correspondence"
            operation = comparison["operation"]
            assert operation["kind"] == "text_changed"
            mask = comparison["text_mask"]
            assert mask["convention"] == "literal-minimal-source-tokens-v1"
            assert mask["claims"]["normalization_pairs"] == 1
            reviews = [review for review in scope["result"]["text_scope_reviews"]
                       if review["boundaries"] == interval["boundaries"] and review.get("source_cuts") is None
                       and review["comparison"]["old"] == comparison["old"]
                       and review["comparison"]["new"] == comparison["new"]]
            assert len(reviews) == 1
            texts = {}
            record = {"boundaries": interval["boundaries"], "sides": {}}
            for side in ("old", "new"):
                owned = native_ids(interval[side + "_sources"])
                assert len(owned) == len(set(owned)) and seen[side].isdisjoint(owned)
                seen[side].update(owned)
                assert set(owned) == set(native_ids(reviews[0][side + "_sources"]))
                boundary_sources = [source for boundary in reviews[0][side + "_boundaries"] for source in boundary]
                assert set(owned).isdisjoint(native_ids(boundary_sources))
                readings = [native[side][identifier]["text"]["Mapped"] for identifier in owned]
                text = "".join(readings)
                assert text == operation[side], (side, text, operation[side])
                texts[side] = text
                token_sources = [identifier for identifier, reading in zip(owned, readings) for _ in reading]
                expected_positions = [i for i, changed in enumerate(mask["claims"]["mandatory_" + side]) if changed]
                assert [entry["position"] for entry in mask[side]] == expected_positions
                for entry in mask[side]:
                    assert native_ids(entry["sources"]) == [token_sources[entry["position"]]]
                record["sides"][side] = {"text": text, "owned_sources": len(owned),
                    "changed_positions": expected_positions,
                    "changed_text": "".join(text[i] for i in expected_positions)}
            cost, old_mask, new_mask = minimal_masks(texts["old"], texts["new"])
            assert cost == mask["claims"]["changed_source_lower"] == mask["claims"]["changed_source_upper"]
            assert old_mask == mask["claims"]["mandatory_old"]
            assert new_mask == mask["claims"]["mandatory_new"]
            records.append(record)
    result = {"status": "literal_readings_and_masks_verified", "scope": __doc__,
              "oracle_self_test_cases": self_test(), "report": reference(report_path),
              "native": {"old": reference(old_path), "new": reference(new_path)}, "intervals": records}
    output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")
    print(len(records), "intervals verified")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("report", type=Path)
    parser.add_argument("old_native", type=Path)
    parser.add_argument("new_native", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    audit(args.report, args.old_native, args.new_native, args.output)
