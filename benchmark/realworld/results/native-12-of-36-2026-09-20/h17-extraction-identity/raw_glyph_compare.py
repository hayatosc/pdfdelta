#!/usr/bin/env python3
"""Executed raw-glyph comparator with invariant validation (v2).

Schema: complete model::Glyph serde JSON (baseline, bbox, crop_status,
direction, font_id, font_size, id, page, path_clip_status, provenance,
raw_code, render_mode, render_order, text).

font_id is part of the exact stable payload. Only `id` and `render_order` are
validated as numeric renumberings; any other field difference is a failure.
All invariants are hard failures (nonzero exit).
"""

import gzip
import hashlib
import json
import sys
from collections import Counter
from pathlib import Path

EPHEMERAL = ("id", "render_order")
HEADER_KEYS = {
    "complete",
    "issues",
    "glyphs",
    "max_operators",
    "max_total_decoded_bytes",
}
GLYPH_KEYS = {
    "baseline",
    "bbox",
    "crop_status",
    "direction",
    "font_id",
    "font_size",
    "id",
    "page",
    "path_clip_status",
    "provenance",
    "raw_code",
    "render_mode",
    "render_order",
    "text",
}
MIN_EXPECTED_GLYPHS = 200_000


class InvariantError(Exception):
    pass


def sha256(path):
    return hashlib.file_digest(Path(path).open("rb"), "sha256").hexdigest()


def load(path):
    with gzip.open(path, "rt") as stream:
        header = json.loads(next(stream))
        glyphs = [json.loads(line) for line in stream]
    return header, glyphs


def stable(glyph):
    return json.dumps(
        {key: value for key, value in glyph.items() if key not in EPHEMERAL},
        sort_keys=True,
        separators=(",", ":"),
    )


def validate_export(label, header, glyphs):
    if set(header) != HEADER_KEYS:
        raise InvariantError(f"{label}: header keys {sorted(header)}")
    if header["glyphs"] != len(glyphs):
        raise InvariantError(f"{label}: header glyph count {header['glyphs']} != {len(glyphs)}")
    if len(glyphs) < MIN_EXPECTED_GLYPHS:
        raise InvariantError(f"{label}: empty/truncated inventory {len(glyphs)}")
    ids = [glyph["id"] for glyph in glyphs]
    if len(set(ids)) != len(ids):
        raise InvariantError(f"{label}: duplicate glyph ids")
    orders = [glyph["render_order"] for glyph in glyphs]
    if any(not isinstance(value, int) for value in orders):
        raise InvariantError(f"{label}: non-integer render_order")
    if any(b <= a for a, b in zip(orders, orders[1:])):
        raise InvariantError(f"{label}: render_order not strictly increasing")
    for glyph in glyphs:
        if set(glyph) != GLYPH_KEYS:
            raise InvariantError(f"{label}: glyph schema keys {sorted(glyph)}")


def map_sequences(h16, h17):
    stable17 = [stable(glyph) for glyph in h17]
    mapping = []
    added = []
    j = 0
    for i, glyph in enumerate(h16):
        key = stable(glyph)
        while j < len(stable17) and stable17[j] != key:
            added.append(h17[j])
            j += 1
        if j == len(stable17):
            raise InvariantError(f"lost glyph at {i}: no remaining H17 occurrence")
        mapping.append((i, j))
        j += 1
    added.extend(h17[j:])
    return mapping, added


def run_validation(h16_header, h16, h17_header, h17):
    validate_export("h16", h16_header, h16)
    validate_export("h17", h17_header, h17)
    if h16_header["max_operators"] != h17_header["max_operators"] or (
        h16_header["max_total_decoded_bytes"] != h17_header["max_total_decoded_bytes"]
    ):
        raise InvariantError("extraction limits differ between exports")
    if h16_header["complete"] or not h17_header["complete"]:
        raise InvariantError("unexpected completion flags")
    if h16_header["issues"] != 1 or h17_header["issues"] != 0:
        raise InvariantError("unexpected extraction issue counts")
    mapping, added = map_sequences(h16, h17)
    # Injective numeric id mapping and render_order renumbering evidence.
    id_map = {}
    id_offset_histogram = Counter()
    order_offset_histogram = Counter()
    previous_after_order = None
    for i, k in mapping:
        before, after = h16[i], h17[k]
        mapped = id_map.setdefault(before["id"], after["id"])
        if mapped != after["id"]:
            raise InvariantError(f"non-injective id mapping at {before['id']}")
        id_offset_histogram[after["id"] - before["id"]] += 1
        order_offset_histogram[after["render_order"] - before["render_order"]] += 1
        if previous_after_order is not None and after["render_order"] <= previous_after_order:
            raise InvariantError("mapped render_order not strictly increasing")
        previous_after_order = after["render_order"]
    if len(id_map) != len(mapping):
        raise InvariantError("id mapping is not one-to-one on retained occurrences")
    return mapping, added, id_offset_histogram, order_offset_histogram


def main():
    (
        h16_path,
        h17_path,
        h16_archive,
        h17_archive,
        pdf_path,
        out_final,
        out_prefix,
    ) = sys.argv[1:8]
    h16_header, h16 = load(h16_path)
    h17_header, h17 = load(h17_path)

    index = 0
    while index < min(len(h16), len(h17)) and h16[index] == h17[index]:
        index += 1
    Path(out_prefix).write_text(
        json.dumps(
            {
                "result": "failed positional prefix comparison (preserved)",
                "matched_prefix": index,
                "lost_count": 1 if index < len(h16) else 0,
                "added_count": len(h17) - index,
                "reason": "positional comparison cannot align an insertion",
            },
            indent=2,
        )
        + "\n"
    )

    mapping, added, id_offset_histogram, order_offset_histogram = run_validation(
        h16_header, h16, h17_header, h17
    )

    anchor = None
    if index < len(h16):
        anchor_i = index
        anchor_k = next(k for i, k in mapping if i == anchor_i)
        before, after = h16[anchor_i], h17[anchor_k]
        anchor = {
            "positional_index": anchor_i,
            "mapped_h17_index": anchor_k,
            "differing_fields": sorted(
                field
                for field in set(before) | set(after)
                if before.get(field) != after.get(field)
            ),
            "id_offset": after["id"] - before["id"],
            "render_order_offset": after["render_order"] - before["render_order"],
        }

    added_groups = Counter()
    for glyph in added:
        provenance = glyph["provenance"]
        stream = provenance["content_stream"]
        added_groups[
            (stream["object_number"], stream["generation"], provenance["operator_index"])
        ] += 1

    result = {
        "evidence_level": "executed raw-glyph audit (complete Glyph schema, invariant-validated)",
        "failed_prefix_attempt": out_prefix,
        "before_header": h16_header,
        "after_header": h17_header,
        "before_records": len(h16),
        "after_records": len(h17),
        "mapped_records": len(mapping),
        "lost_count": len(h16) - len(mapping),
        "added_count": len(added),
        "ephemeral_fields": list(EPHEMERAL),
        "stable_payload_includes_font_id": True,
        "id_offset_histogram": {str(k): v for k, v in sorted(id_offset_histogram.items())},
        "render_order_offset_histogram": {
            str(k): v for k, v in sorted(order_offset_histogram.items())[:20]
        },
        "render_order_offset_distinct": len(order_offset_histogram),
        "anchor": anchor,
        "added_groups": [
            {
                "stream_object": group[0],
                "generation": group[1],
                "operator_index": group[2],
                "count": count,
            }
            for group, count in sorted(added_groups.items())
        ],
        "added_raw_codes": sorted({bytes(g["raw_code"]).hex() for g in added}),
        "id_map_size": len({g["id"] for g in h16}),
        "binding": {
            "h16_export_sha256": sha256(h16_path),
            "h17_export_sha256": sha256(h17_path),
            "h16_source_archive_sha256": sha256(h16_archive),
            "h17_source_archive_sha256": sha256(h17_archive),
            "pdf_sha256": sha256(pdf_path),
            "comparator_sha256": sha256(__file__),
            "probe_sha256": sha256(Path(__file__).with_name("raw_glyph_stream.rs")),
        },
    }
    Path(out_final).write_text(json.dumps(result, indent=2) + "\n")
    print(
        json.dumps(
            {
                "mapped_records": len(mapping),
                "lost_count": result["lost_count"],
                "added_count": len(added),
                "id_offsets": result["id_offset_histogram"],
                "render_order_offsets_distinct": result["render_order_offset_distinct"],
                "anchor": anchor,
            },
            indent=1,
        )
    )


if __name__ == "__main__":
    main()
