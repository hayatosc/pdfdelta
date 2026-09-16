#!/usr/bin/env python3
"""Check reported native-domain projections against a frozen source bundle.

This checks raw native equality and source conservation, not domain admission,
visibility, exhaustive acquisition or the full comparison completion predicate.
"""
import argparse
import hashlib
import json
from pathlib import Path


def ref(path):
    with path.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": str(path), "sha256": digest}


def ids(sources):
    assert all(source["origin"] == "native" for source in sources)
    return [source["glyph"] for source in sources]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", type=Path)
    parser.add_argument("report", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    sources = json.loads(args.sources.read_text())
    report = json.loads(args.report.read_text())
    indices = {}
    for side in ("old", "new"):
        assert report[side]["revision"] == sources[side]["summary"]["revision"]
        assert report[side]["native_glyphs"] == len(sources[side]["native"]["items"])
        indices[side] = ({node["id"]: node for node in sources[side]["graph"]["nodes"]},
                         {glyph["id"]: glyph for glyph in sources[side]["native"]["items"]})
    rows = []
    for scope in report["comparison"]["scopes"]:
        for domain in scope["result"]["native_text_domains"]:
            row = {"proposal": domain["proposal"], "sides": {}}
            for side in ("old", "new"):
                fragment = domain[side]
                nodes, glyphs = indices[side]
                parent = nodes[fragment["node"]]
                view = parent["content"]["view"]
                start, end = fragment["tokens"]
                assert 0 <= start < end <= len(view["tokens"])
                assert all(view["source_backed"][start:end])
                owned = ids(fragment["sources"])
                complement = ids(domain[side + "_complement"])
                whole = ids(parent["sources"])
                assert len(whole) == len(set(whole))
                assert len(owned) == len(set(owned))
                assert len(complement) == len(set(complement))
                assert set(owned).isdisjoint(complement)
                assert set(owned) | set(complement) == set(whole)
                projected = {glyph for origins in view["origins"][start:end] for glyph in ids(origins)}
                assert projected == set(owned)
                for position, origins in enumerate(view["origins"]):
                    if view["source_backed"][position] and not start <= position < end:
                        assert set(ids(origins)).isdisjoint(owned)
                text = "".join(token["Scalar"] for token in view["tokens"][start:end])
                native = "".join(glyphs[glyph]["text"]["Mapped"] for glyph in owned)
                assert native == text
                row["sides"][side] = {"node": fragment["node"], "tokens": fragment["tokens"],
                    "owned_sources": len(owned), "complement_sources": len(complement), "text": text}
            assert row["sides"]["old"]["text"] == row["sides"]["new"]["text"]
            rows.append(row)
    result = {"status": "native_equality_and_source_conservation_verified",
              "scope": __doc__, "source_bundle": ref(args.sources), "report": ref(args.report),
              "domains": rows}
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n")
    print(len(rows), "domains verified")


if __name__ == "__main__":
    main()
