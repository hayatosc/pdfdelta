import ijson,gzip,sys
from pathlib import Path
cache=Path(sys.argv[1])
for pair in ('arxiv-faster-rcnn-v1-to-v3','irs-schedule-c-2024-to-2025'):
    p=cache/pair/f'{pair}-native.json.gz'
    agg={'regions':0,'old_unmapped':0,'new_unmapped':0,'regions_with_unmapped':0,'old_absent':0,'new_absent':0}
    issues={}
    with gzip.open(p,'rb') as f:
        for prefix,event,val in ijson.parse(f):
            if prefix=='unresolved_regions.item' and event=='start_map': agg['regions']+=1; cur={'ou':0,'nu':0,'oa':False,'na':False}
            elif prefix=='unresolved_regions.item' and event=='end_map':
                agg['old_unmapped']+=cur['ou']; agg['new_unmapped']+=cur['nu']
                if cur['ou'] or cur['nu']: agg['regions_with_unmapped']+=1
                agg['old_absent']+=cur['oa']; agg['new_absent']+=cur['na']
            elif prefix=='unresolved_regions.item.old_span' and event=='start_map': cur['oa']=False
            elif prefix=='unresolved_regions.item.old_span' and event=='end_map': pass
            elif prefix=='unresolved_regions.item.new_span' and event=='start_map': pass
            elif prefix=='unresolved_regions.item.old_span.unmapped_tokens' and event=='number': cur['ou']=val
            elif prefix=='unresolved_regions.item.new_span.unmapped_tokens' and event=='number': cur['nu']=val
            elif prefix=='extraction.issues.item.kind' and event=='string': issues[val]=issues.get(val,0)+1
    print('==',pair,agg,'issues',issues)
