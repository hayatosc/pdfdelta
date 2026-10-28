"""Generate exact substring-count expectations, independently of Rust matchers."""
import itertools,json,pathlib

def occurrences(views, needle):
    out=[]
    if not needle:return out
    for view,text in enumerate(views):
        for start in range(len(text)-len(needle)+1):
            if text[start:start+len(needle)]==needle:
                out.append([view,start])
                if len(out)==2:return out
    return out

def expected(case):
    found=[]
    for old,new,seed_end in case['pairs']:
        if not case['old_bounded'][old] or not case['new_bounded'][new]:continue
        a,b=case['old'][old],case['new'][new];length=0
        for x,y in zip(reversed(a),reversed(b)):
            if x!=y:break
            length+=1
        if not length:continue
        i,j=len(a)-length,len(b)-length
        if not(i==j==0) and not(seed_end is not None and seed_end<=i and seed_end<=j):continue
        needle=a[i:];left=occurrences(case['old'],needle);right=occurrences(case['new'],needle)
        if left==[[old,i]] and right==[[new,j]]:found.append([old,new,i,len(a),j,len(b)])
    return found

words=[''.join(x) for n in range(5) for x in itertools.product('AB',repeat=n)]
cases=[]
for a,b in itertools.product(words,repeat=2):
    old=['Q'+a+'R','uniqueΩ','uniqueЖ','unique界'];new=['Q'+b+'R',*old[1:]]
    if a.endswith('A'):old+=['X'+old[0]+'Y']
    if b.endswith('B'):new+=[new[0]]
    old+=['','QQ','RR'];new+=['','QQ','RR']
    case={'old':old,'new':new,'old_bounded':[True]*4+[False]*(len(old)-4),'new_bounded':[True]*4+[False]*(len(new)-4),'pairs':[[i,i,1] for i in range(4)]}
    case['expected']=expected(case);cases.append(case)
for words_pair in [(['AA','AB','ABA',' '],['AA','AB','ABA',' ']),(['','\n','é','e\u0301'],['','\n','é','e\u0301']),(['A','B','AA','AB'],['XA','YB','ZAA','WAB'])]:
    old,new=map(list,words_pair);case={'old':old+['AAA','XABAX'],'new':new+['AAA','XABAX'],'old_bounded':[True]*4+[False]*2,'new_bounded':[True]*4+[False]*2,'pairs':[[i,i,None] for i in range(4)]};case['expected']=expected(case);cases.append(case)
p=pathlib.Path(__file__).with_name('source_end_oracle.json');p.write_text(json.dumps({'generator':'independent exact substring enumeration; overlapping matches and every view retained','cases':cases},ensure_ascii=False,separators=(',',':'))+'\n');print(len(cases),p.stat().st_size)
