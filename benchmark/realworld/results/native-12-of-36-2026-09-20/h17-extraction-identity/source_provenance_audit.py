#!/usr/bin/env python3
"""Projection-level stable-provenance comparison for the NASA pair.

Streams `assessment.new_resolution.item` from the bound H16 full006 and H17
full008 reports with ijson (a real JSON parser). For every JsonSpanSource of
kind `glyph` it records the stable payload

    (page, bbox min/max rounded, content_stream object+generation, operator_index)

keyed by the report glyph_id (exact ijson numeric values, no rounding). Repeated references to the same glyph_id are
context reuse, not extra physical glyphs: their payloads must agree, and the
glyph is counted once. Distinct glyph_ids sharing a stable payload keep their
multiplicity.

Output: lost/retained/added actual source-glyph occurrences plus added
(page, stream, operator) groups, bound to report hashes. JsonSpanSource does
not carry raw_code and block IDs are ephemeral, so this is projection-level
evidence only; raw-code/order proof against the frozen PDFs remains a separate
requirement.
"""

import gzip
import hashlib
import json
import sys
from collections import Counter, defaultdict
from pathlib import Path

import ijson


def stable_payload(source):
    bbox = source["bbox"]
    minimum = bbox["min"]
    maximum = bbox["max"]
    stream = source["content_stream"]
    return (
        source["page"],
        minimum["x"],
        minimum["y"],
        maximum["x"],
        maximum["y"],
        stream["object_number"],
        stream["generation"],
        source["operator_index"],
    )


def collect(report_path):
    glyph_payloads = {}  # glyph_id -> stable payload
    conflicts = 0
    non_glyph = Counter()
    with gzip.open(report_path, "rb") as stream:
        for item in ijson.items(stream, "assessment.new_resolution.item"):
            for source in item.get("sources", []):
                if source.get("kind") != "glyph":
                    non_glyph[source.get("kind")] += 1
                    continue
                glyph_id = source["glyph_id"]
                payload = stable_payload(source)
                known = glyph_payloads.get(glyph_id)
                if known is None:
                    glyph_payloads[glyph_id] = payload
                elif known != payload:
                    conflicts += 1
    occurrences = Counter(glyph_payloads.values())
    return glyph_payloads, occurrences, conflicts, non_glyph


def main():
    before_path, after_path, output = (Path(arg) for arg in sys.argv[1:4])
    before_glyphs, before, before_conflicts, before_non_glyph = collect(before_path)
    after_glyphs, after, after_conflicts, after_non_glyph = collect(after_path)

    lost = before - after
    retained = before & after
    added = after - before

    added_groups = Counter()
    for payload, count in added.items():
        page, *_rest, stream_object, generation, operator_index = payload
        added_groups[(page, stream_object, generation, operator_index)] += count
    added_group_rows = [
        {
            "page": group[0],
            "content_stream_object": group[1],
            "generation": group[2],
            "operator_index": group[3],
            "count": count,
        }
        for group, count in sorted(added_groups.items())
    ]

    result = {
        "scope": "projection-level assessment.new_resolution glyph sources",
        "not_claimed": [
            "raw_code evidence (JsonSpanSource has no raw_code)",
            "relative render order proof",
            "raw glyph extraction from frozen PDFs",
        ],
        "before_report": {
            "path": str(before_path),
            "stored_sha256": hashlib.file_digest(before_path.open("rb"), "sha256").hexdigest(),
        },
        "after_report": {
            "path": str(after_path),
            "stored_sha256": hashlib.file_digest(after_path.open("rb"), "sha256").hexdigest(),
        },
        "before_unique_glyphs": len(before_glyphs),
        "after_unique_glyphs": len(after_glyphs),
        "lost_glyph_occurrences": sum(lost.values()),
        "retained_glyph_occurrences": sum(retained.values()),
        "added_glyph_occurrences": sum(added.values()),
        "distinct_stable_payloads_before": len(before),
        "distinct_stable_payloads_after": len(after),
        "glyph_id_payload_conflicts_before": before_conflicts,
        "glyph_id_payload_conflicts_after": after_conflicts,
        "non_glyph_sources_before": dict(before_non_glyph),
        "non_glyph_sources_after": dict(after_non_glyph),
        "added_groups": added_group_rows,
    }
    output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps({key: result[key] for key in (
        "before_unique_glyphs", "after_unique_glyphs", "lost_glyph_occurrences",
        "retained_glyph_occurrences", "added_glyph_occurrences",
        "glyph_id_payload_conflicts_before", "glyph_id_payload_conflicts_after",
    )}, indent=1))
    for row in added_group_rows:
        print("added", row)
    if result["lost_glyph_occurrences"] != 0 or before_conflicts or after_conflicts:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
