import ijson,gzip,sys
from pathlib import Path
cache=Path(sys.argv[1])
for pair in ('faa-maintenance-records-c-to-d','irs-schedule-c-2024-to-2025','nasa-buckling-8007-1968-to-2020'):
    p=cache/pair/f'{pair}-native.json.gz'
    print('==',pair)
    regions=[]; cur=None; span=None
    with gzip.open(p,'rb') as f:
        for prefix,event,val in ijson.parse(f):
            if prefix=='unresolved_regions.item' and event=='start_map': cur={}
            elif prefix=='unresolved_regions.item' and event=='end_map': regions.append(cur); cur=None
            elif cur is not None:
                if prefix.endswith('_span') and event=='start_map': span=prefix.rsplit('.',1)[1][:3]; cur.setdefault(span,{'n':0,'canon':None,'comp':None})
                elif span and prefix.endswith('_span.canonical_range.start') and event=='number': cur[span]['canon']=[val,None]
                elif span and prefix.endswith('_span.canonical_range.end') and event=='number': cur[span]['canon'][1]=val
                elif span and prefix.endswith('_span.comparable_range.start') and event=='number': cur[span]['comp']=[val,None]
                elif span and prefix.endswith('_span.comparable_range.end') and event=='number': cur[span]['comp'][1]=val
                elif span and prefix.endswith('_span.sources.item') and event=='start_map': cur[span]['n']+=1
                elif prefix=='unresolved_regions.item.new_span' and event=='end_map': span=None
                elif prefix=='unresolved_regions.item.old_span' and event=='end_map': span=None
    for i,r in enumerate(regions):
        o=r.get('old',{}); n=r.get('new',{})
        print(i,'old',('c=%s'%(o.get('canon'),)) if o else 'ABSENT','srcs',o.get('n',0),
              '| new',('c=%s'%(n.get('canon'),)) if n else 'ABSENT','srcs',n.get('n',0))
