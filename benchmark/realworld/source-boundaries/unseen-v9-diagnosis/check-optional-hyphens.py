import importlib.util,json
from pathlib import Path
base=Path("benchmark/realworld/cache/source-boundaries-unseen-v9")
spec=importlib.util.spec_from_file_location("review", "benchmark/realworld/source-boundaries/review-v75.py")
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
rows=[]
for name,indices in [("nist-131a-r1-to-r2",[4,5,12,14]),("ietf-snmp-2571-to-3411",[29,30])]:
 report=json.loads((base/"current-repetition1"/f"{name}-text.json").read_text())
 source=json.loads((base/"source-exports"/name/"review/sources.json").read_text())
 for index in indices:
  review=report["comparison"]["scopes"][0]["result"]["text_scope_reviews"][index]
  views={side:m.masks.expanded_view(review,side,source[side]) for side in ("old","new")}
  evidence={}
  for side in ("old","new"):
   glyphs={g["id"]:g for g in source[side]["native"]["items"]}
   optional=set(); witnesses=[]
   for node in source[side]["graph"]["nodes"]:
    if node["id"] not in review["source_cuts"]["population"][side]:continue
    view=node.get("content",{}).get("view")
    if view is None:continue
    backed=[i for i,b in enumerate(view["source_backed"]) if b]
    for a,b in zip(backed,backed[1:]):
     refs=view["origins"][a];following=view["origins"][b]
     if view["tokens"][a]!={"Scalar":"-"} or len(refs)!=1 or not following:continue
     left,right=(glyphs[r[0]["glyph"]] for r in (refs,following))
     if left["id"]==right["id"]:continue
     if left["baseline"]["y"]!=right["baseline"]["y"]:
      assert left["page"]==right["page"] and left["baseline"]["y"]>right["baseline"]["y"]
      assert left["text"]=={"Mapped":"-"}
      optional.add(left["id"])
   local=views[side]
   for position,(token,refs,backed) in enumerate(zip(local["tokens"],local["origins"],local["backed"])):
    if token=="-" and backed and len(refs)==1 and refs[0]["glyph"] in optional:
     local["optional"][position]=True;witnesses.append({"position":position,"glyph":refs[0]["glyph"]})
   evidence[side]=witnesses
  checked=m.masks.check_mask(review,views)
  rows.append({"pair":name,"review_index":index,"source_checked_optional_hyphens":evidence,"independent_mask":checked})
  print(name,index,"PASS",flush=True)
  (base/"optional-hyphen-mask-checks.json").write_text(json.dumps(rows,indent=2)+"\n")
