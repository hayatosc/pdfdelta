#!/usr/bin/env python3
"""Executed negative checks for raw_glyph_compare.py invariants."""
import hashlib, importlib.util, json, sys
from pathlib import Path
res = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("cmp", res / "raw_glyph_compare.py")
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
mod.MIN_EXPECTED_GLYPHS = 0
def glyph(i, order):
    return {'baseline':{'x':0.0,'y':0.0},'bbox':{'min':{'x':0.0,'y':0.0},'max':{'x':1.0,'y':1.0}},
            'crop_status':'Complete','direction':{'x':1.0,'y':0.0},'font_id':0,'font_size':10.0,'id':i,
            'page':0,'path_clip_status':'None','provenance':{'content_stream':{'object_number':1,'generation':0},'operator_index':0},
            'raw_code':[65],'render_mode':'Fill','render_order':order,'text':{'Mapped':'A'}}
h16h={'complete':False,'issues':1,'glyphs':2,'max_operators':1,'max_total_decoded_bytes':1}
h17h={'complete':True,'issues':0,'glyphs':2,'max_operators':1,'max_total_decoded_bytes':1}
base=[glyph(0,0),glyph(1,1)]
mod.run_validation(h16h, base, h17h, base)
results={'baseline':'accepted'}
try:
    mod.run_validation(h16h, base, h17h, [glyph(0,1),glyph(1,0)])
    results['render_order_reversal']='NOT_REJECTED'
except mod.InvariantError as error:
    results['render_order_reversal']=f'rejected: {error}'
try:
    mod.run_validation(h16h, base, {'complete':True,'issues':0,'glyphs':3,'max_operators':1,'max_total_decoded_bytes':1}, base)
    results['header_count_corruption']='NOT_REJECTED'
except mod.InvariantError as error:
    results['header_count_corruption']=f'rejected: {error}'
out={'comparator_sha256':hashlib.sha256((res/'raw_glyph_compare.py').read_bytes()).hexdigest(),'results':results,
     'exit':'nonzero required on rejection; NOT_REJECTED outcomes are failures'}
(res/'raw-glyph-negative-checks.json').write_text(json.dumps(out,indent=2)+'\n')
print(json.dumps(out,indent=1))
failed=[k for k,v in results.items() if v=='NOT_REJECTED']
sys.exit(1 if failed else 0)
