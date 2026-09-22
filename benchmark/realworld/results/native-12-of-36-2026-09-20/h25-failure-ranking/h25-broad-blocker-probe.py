import ijson,gzip,sys
from pathlib import Path
cache=Path(sys.argv[1])
pairs=('nist-authentication-63b-to-63b4','nist-incident-handling-r2-to-r3','edpb-controller-processor-v1-to-v2-1','edpb-restrictions-v1-to-final','irs-w9-2018-to-2024')
for pair in pairs:
    p=cache/pair/f'{pair}-native.json.gz'
    st={'summary':{},'two_sided':0,'one_sided':0,'reasons':{},'issues':{}}
    with gzip.open(p,'rb') as f:
        for prefix,event,val in ijson.parse(f):
            if prefix.startswith('summary.') and prefix.count('.')==1 and event in ('string','number','boolean'):
                st['summary'][prefix[8:]]=val if not (event=='string' and len(str(val))>80) else str(val)[:80]
            if prefix=='unresolved_regions.item' and event=='start_map': cur={'o':0,'n':0}
            elif prefix=='unresolved_regions.item' and event=='end_map':
                st['two_sided']+= (cur['o']>0 and cur['n']>0); st['one_sided']+= (cur['o']==0 or cur['n']==0)
            elif prefix=='unresolved_regions.item.old_span' and event=='start_map': cur['o']+=1
            elif prefix=='unresolved_regions.item.new_span' and event=='start_map': cur['n']+=1
            elif prefix.startswith('assessment.relations.item.reasons') and event=='string': st['reasons'][val]=st['reasons'].get(val,0)+1
            elif prefix=='extraction.issues.item.kind' and event=='string': st['issues'][val]=st['issues'].get(val,0)+1
    print('==',pair)
    print(' summary',st['summary'])
    print(' regions two_sided',st['two_sided'],'one_sided',st['one_sided'],'reasons',dict(sorted(st['reasons'].items(),key=lambda x:-x[1])[:5]),'issues',st['issues'])
