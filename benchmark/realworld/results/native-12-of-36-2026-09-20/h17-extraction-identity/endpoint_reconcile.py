#!/usr/bin/env python3
"""Endpoint reconciliation through the proven complete-Glyph id map.

Streams both report resolutions, maps every synthetic_space/line_break
preceding/following glyph id through the executed raw-audit id mapping, and
compares endpoint pairs as multisets. Also compares the projected glyph
inventory with the raw inventory and proves the excluded raw ids correspond
on both sides. Rejects unmapped/conflicting endpoints.
"""
import gzip, hashlib, importlib.util, json, sys
from collections import Counter
from pathlib import Path

res_dir = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("cmp", res_dir / "raw_glyph_compare.py")
cmp_mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(cmp_mod)

h16_export, h17_export = sys.argv[1], sys.argv[2]
h16_report, h17_report = sys.argv[3], sys.argv[4]
out_path = Path(sys.argv[5])

h16h, h16 = cmp_mod.load(h16_export)
h17h, h17 = cmp_mod.load(h17_export)
mapping, added, _, _ = cmp_mod.run_validation(h16h, h16, h17h, h17)
id_map = {h16[i]["id"]: h17[k]["id"] for i, k in mapping}

def stream_report(report):
    glyph_ids = []
    endpoints = Counter()
    blocks = []
    with gzip.open(report, "rb") as f:
        import ijson
        for item in ijson.items(f, "assessment.new_resolution.item"):
            block = item.get("block")
            blocks.append(block)
            for source in item.get("sources", []):
                kind = source.get("kind")
                if kind == "glyph":
                    glyph_ids.append(source["glyph_id"])
                elif kind in ("synthetic_space", "line_break"):
                    endpoints[(kind, source["preceding_glyph_id"], source["following_glyph_id"])] += 1
    return glyph_ids, endpoints, blocks

h16_glyphs, h16_endpoints, h16_blocks = stream_report(h16_report)
h17_glyphs, h17_endpoints, h17_blocks = stream_report(h17_report)

mapped_endpoints = Counter()
unmapped = 0
for (kind, before, after), count in h16_endpoints.items():
    if before not in id_map or after not in id_map:
        unmapped += count
        continue
    mapped_endpoints[(kind, id_map[before], id_map[after])] += count
missing = mapped_endpoints - h17_endpoints
added_endpoints = h17_endpoints - mapped_endpoints

projected16 = set(h16_glyphs); projected17 = set(h17_glyphs)
raw16 = {g["id"] for g in h16}; raw17 = {g["id"] for g in h17}
if (projected16 - raw16) or (projected17 - raw17):
    raise SystemExit("projected ids absent from raw inventory")
if any(pre not in raw17 or fol not in raw17 for (_k, pre, fol) in h17_endpoints):
    raise SystemExit("after endpoints absent from raw inventory")
excluded16 = raw16 - projected16
excluded17 = raw17 - projected17
mapped_excluded = {id_map[g] for g in excluded16 if g in id_map}

result = {
 "id_map_size": len(id_map),
 "endpoints_before": sum(h16_endpoints.values()),
 "endpoints_after": sum(h17_endpoints.values()),
 "endpoints_mapped": sum(mapped_endpoints.values()),
 "endpoints_unmapped": unmapped,
 "endpoints_missing_after_mapping": sum(missing.values()),
 "endpoints_added": sum(added_endpoints.values()),
 "missing_examples": [list(k) for k in list(missing)[:5]],
 "added_examples": [list(k) for k in list(added_endpoints)[:5]],
 "projected_before": len(projected16),
 "projected_after": len(projected17),
 "raw_before": len(raw16),
 "raw_after": len(raw17),
 "excluded_raw_before": len(excluded16),
 "excluded_raw_after": len(excluded17),
 "excluded_mapped_equal": mapped_excluded == excluded17,
 "excluded_before_ids": sorted(excluded16)[:16],
 "excluded_after_ids": sorted(excluded17)[:16],
 "blocks_before": len(h16_blocks),
 "blocks_after": len(h17_blocks),
 "binding": {
   "h16_report_sha256": hashlib.file_digest(Path(h16_report).open("rb"), "sha256").hexdigest(),
   "h17_report_sha256": hashlib.file_digest(Path(h17_report).open("rb"), "sha256").hexdigest(),
   "h16_export_sha256": hashlib.file_digest(Path(h16_export).open("rb"), "sha256").hexdigest(),
   "h17_export_sha256": hashlib.file_digest(Path(h17_export).open("rb"), "sha256").hexdigest(),
   "comparator_sha256": hashlib.sha256((res_dir / "raw_glyph_compare.py").read_bytes()).hexdigest(),
   "probe_sha256": hashlib.sha256((res_dir / "raw_glyph_stream.rs").read_bytes()).hexdigest(),
   "raw_validation": "run_validation applied to both exports before joining",
   "projected_ids_absent_from_raw": 0,
   "after_endpoints_absent_from_raw": 0,
   "script_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
 },
}
out_path.write_text(json.dumps(result, indent=2) + "\n")
print(json.dumps({k: v for k, v in result.items() if k not in ("binding",) and not k.endswith("_ids")}, indent=1))
ok = (unmapped == 0 and sum(missing.values()) == 0 and sum(added_endpoints.values()) == 0
      and mapped_excluded == excluded17)
sys.exit(0 if ok else 1)
