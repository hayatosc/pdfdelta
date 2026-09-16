from pathlib import Path
from collections import Counter
import copy
import hashlib
import json
import shutil
import sys

root = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(root / 'benchmark/realworld/remaining'))
import verify
verify.CONTRACT = 'source-boundaries-v1'
base = root / 'benchmark/realworld/source-boundaries'
def read(path):
    return json.loads(path.read_text())
def ref(path):
    return {'path': str(path.relative_to(root)), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}
def save(path, data):
    temp = path.with_suffix('.tmp')
    temp.write_text(json.dumps(data, indent=2) + '\n')
    temp.replace(path)
    assert read(path) == data

build = read(root / 'benchmark/realworld/cache/source-boundaries-local-cuts-v12/build.json')
assert build['production_sha256'] == verify.source_fingerprint()
evaluator = root / 'benchmark/realworld/cache/source-boundaries-local-cuts-v12-evaluator'
evaluator.mkdir(exist_ok=True)
shutil.copy2(root / 'benchmark/realworld/remaining/verify.py', evaluator / 'verify.py')
shutil.copy2(root / 'benchmark/realworld/remaining/test_verify.py', evaluator / 'test_verify.py')
indices = [read(base / name) for name in ('v12-pilot-observations.json', 'v12-pilot-repetition2.json')]
observations = [row for index in indices for row in index['observations']]
edpb_pair = 'edpb-controller-processor-v1-to-v2-1'
observations.append(next(row for row in read(base / 'v12-panel-observations.json')['observations'] if row['pair'] == edpb_pair))
observations.extend(read(base / 'v12-edpb-repeat-observations.json')['observations'])
sources_path = root / 'benchmark/realworld/cache/source-boundaries-sha-v9-review/sources.json'
sources = read(sources_path)
glyphs = {side: {g['id']: g for g in sources[side]['native']['items']} for side in ('old', 'new')}
targets = {t['pair']: t for t in read(root / 'benchmark/realworld/followup/targets.json')['targets']}
baselines = {
    'historical': read(root / 'benchmark/realworld/followup/baseline-observations.json'),
    '599855d': read(base / 'baseline-observations.json'),
}
bert = {row['event_sha256']: row for row in read(base / 'bert-v7-adjudications.json')['observations'][0]['events']}
# These rationales record manual page-image adjudication of frozen captures.
reasons = {
    0: 'The named Commerce Secretary changes from Carlos M. Gutierrez to Penny Pritzker on the corresponding cover. Both source strings agree exactly with their glyph projections.',
    1: 'The title words are visually unchanged. This is only a mandatory source-space difference at the title edges; it is not a prose-target recovery or a semantic title change.',
    2: 'The corresponding message-block item adds SHA-512/224 and SHA-512/256. Page images verify the addition. Retained normalization contracts some spaces, but the added non-space algorithm names are source-backed.',
    3: 'The corresponding SHR applicability sentence adds SHA-512/224 and SHA-512/256; both page images and raw glyph strings support the change.',
    4: 'The padding paragraph removes the before-computation requirement and adds permission to pad during computation before the affected blocks. The page images verify both prose spans; source-space contractions do not supply the content witness.',
    5: 'The SHA-1 preprocessing list changes from three steps to two, initializing the hash first and referring padding/parsing to Section 5. The page images verify the reordered/replaced list text.',
    6: 'The security paragraph adds SHA-512/224 and SHA-512/256. The source still says five algorithms; the implementation neither repairs nor infers the author intent.',
    7: 'The corresponding SP 800-57 reference tail changes August 2005 to (Draft) May 2011. Both page images verify the actual date and draft qualifier.',
    10: 'The abstract removes five before hash algorithms. This enclosing review also retains three old and two new trailing literal space glyphs, so its 388 atoms exceed the fixed 383-atom target.',
    12: 'The abstract removes five before hash algorithms. All 194 old and 189 new glyphs exactly match the unchanged registered prose extent. The five trailing source-space glyphs are recorded outside this additional view and remain in the enclosing 388-atom review.',
    14: 'The title words remain unchanged. This source-cut view records only the two old trailing literal spaces; no lexical title change or fixed prose-target gain is claimed.',
    15: 'The message-block item adds SHA-512/224 and SHA-512/256, retaining three literal trailing spaces on each side in this raw source projection.',
    16: 'The same message-block item adds SHA-512/224 and SHA-512/256. The additional 269-atom content view excludes the three trailing literal spaces on each side while retaining the enclosing review.',
}
space_reasons = {
    8: 'Two literal spaces precede U.S. Department of Commerce only in the old corresponding source interval.',
    9: 'One literal space precedes National Institute of Standards and Technology only in the old interval.',
    11: 'One literal space remains between the corresponding keyword endpoint and footer only on the old side.',
    13: 'One literal space precedes SECURE inside the corresponding Specifications/Table of Contents interval only on the old side.',
    17: 'One literal space precedes the corresponding SP 800-57 reference only on the old side.',
    18: 'One literal space precedes the corresponding SP 800-107 reference only on the old side.',
    19: 'One literal space lies between the corresponding Appendix A and A.1 headings only on the old side.',
}
edpb_reasons = {
    0: 'Paragraph 12 adds the sentence stating that allocation follows factual circumstances and is not negotiable. The complete 375 old and 611 new glyphs reproduce the fixed 986-atom target already recovered by 599855d.',
    1: 'Paragraph 14 adds the effective/complete protection qualifier, a footnote reference, and the qualifier about not diminishing the processor role. The following section headings remain in this larger outside-target interval.',
    2: 'The first line of the fourth-building-block paragraph is renumbered from 30 to 32. This is a source-supported numbering change, not another prose-target recovery.',
    3: 'The dictionary-definition paragraph is renumbered from 31 to 33. This source-supported numbering change does not count as a new prose target.',
    4: 'The collection-purpose paragraph is renumbered from 32 to 34. Its displayed first-line words otherwise agree; source spacing uncertainty is not the content witness.',
    5: 'The why/how paragraph changes numbering 33 to 35 and footnote references 10/11 to 14/15; the following first line changes 34 to 36. These visible source labels prove a difference within this finite range, without implying a new semantic prose change.',
    6: 'The data-access paragraph changes numbering 42 to 45 and reference 13 to 17, and adds the period before that reference. All are visible at the corresponding paragraph start.',
    7: 'Joint-participation paragraph 52 becomes 55, and the old separate paragraph number 53 is absent before the converging-decisions continuation. The finite reviewed source interval contains these visible label/presence changes.',
    8: 'The platform/infrastructure paragraph changes 63 to 65 and reference 24 to 29, adding a period before it; the following paragraph start changes 64 to 66. The page images support those source changes.',
}
adjudicated = []
scores = []
for observation in observations:
    pair = observation['pair']
    report = read(root / observation['report']['path'])
    assert ref(root / observation['report']['path']) == observation['report']
    events = verify.events(report)
    if not pair.startswith('arxiv'):
        source_directory = 'source-boundaries-baseline-edpb-review' if pair == edpb_pair else 'source-boundaries-sha-v9-review'
        sources_path = root / 'benchmark/realworld/cache' / source_directory / 'sources.json'
        sources = read(sources_path)
        glyphs = {side: {g['id']: g for g in sources[side]['native']['items']} for side in ('old', 'new')}
    rows = []
    for event in events:
        digest = verify.event_digest(event)
        if pair.startswith('arxiv'):
            record = copy.deepcopy(bert[digest])
            record['pointer'] = event['pointer']
            record['identity_revalidation'] = 'Frozen v12 event digest equals the fully source-reviewed v7 event digest.'
        else:
            review = event['review']
            index = int(event['pointer'].rsplit('/', 1)[1])
            evidence = {'sources': ref(sources_path), 'sides': {}}
            for side in ('old', 'new'):
                assert report[side]['revision'] == sources[side]['summary']['revision']
                selected = [glyphs[side][source['glyph']] for source in review[side + '_sources']]
                raw = ''.join(g['text']['Mapped'] for g in selected)
                displayed = review['comparison']['operation'][side]
                assert raw.replace(' ', '') == displayed.replace(' ', '')
                assert all(g['crop_status'] == 'Inside' and g['path_clip_status'] in ('Inside', 'Unclipped') for g in selected)
                if review.get('source_cuts') is not None:
                    assert raw == displayed
                pages = sorted({g['page'] for g in selected})
                evidence['sides'][side] = {'pages': pages, 'raw_text': raw, 'displayed_text': displayed,
                    'raw_source_spaces': raw.count(' '), 'images': [ref(sources_path.parent / f'{side}-region-{page}.png') for page in pages]}
            if pair == edpb_pair:
                rationale = edpb_reasons[index] + ' Every non-space source scalar matches the displayed operation; reconstructed and contracted spaces are not used to infer an exact spacing mask.'
            elif index in space_reasons:
                assert review['comparison']['operation']['old'].strip(' ') == ''
                assert review['comparison']['operation']['new'] == ''
                rationale = space_reasons[index] + ' The claim is source-space presence within this interval only; no visible lexical change or document-wide deletion is claimed.'
            else:
                rationale = reasons[index]
            correspondence = 'The retained accepted boundary texts identify the same source interval on both revisions; the source-band/paint closure is checked independently of the content witness.'
            if review.get('source_cuts') is not None:
                correspondence += ' The cut census is conditional on the declared outer population and retains native order and complete source accounting.'
            if (review.get('source_cuts') or {}).get('edge_refinement'):
                correspondence += ' The additional content edges are bound to the retained enclosing comparison. Only mandatory source-backed ASCII spaces are moved outside this view; source fragments partition the original extent without overlap.'
            record = {'pointer': event['pointer'], 'event_sha256': digest, 'verdict': 'source_supported',
                      'source_content_rationale': rationale, 'correspondence_rationale': correspondence,
                      'source_evidence': evidence}
        rows.append(record)
    adjudicated.append({**observation, 'events': rows})
    core, extent, controls = verify.historical.target_sources(targets[pair])
    for name, index in baselines.items():
        previous = next(x for x in index['observations'] if x['pair'] == pair and x['repetition'] == observation['repetition'])
        missing = None
        if not (root / previous['report']['path']).exists():
            missing = previous['report']
            previous = next(x for x in index['observations'] if x['pair'] == pair and (root / x['report']['path']).exists())
        before = read(root / previous['report']['path'])
        assert ref(root / previous['report']['path']) == previous['report']
        counts = Counter(verify.event_digest(event) for event in verify.events(before))
        needed = []
        for event, review in zip(events, rows):
            digest = verify.event_digest(event)
            if counts[digest]:
                counts[digest] -= 1
            else:
                needed.append(review)
        score = verify.pair_recovery(before, report, core, extent, controls, needed)
        expected = [] if pair == edpb_pair and name == '599855d' else ['B']
        assert score['correct'] and score['additional_categories'] == expected, (pair, name, score)
        scores.append({'pair': pair, 'repetition': observation['repetition'], 'baseline': name,
                       'baseline_repetition': previous['repetition'], 'unavailable_registered_report': missing,
                       'baseline_report': previous['report'], 'target_atoms': len(extent), **score})
save(base / 'v12-adjudications.json', {'version': 1, 'scope': 'All A/B outputs for three selected development pairs, each with two observations. Source-space-only and label-only changes are explicit and do not establish extra prose recovery.', 'observations': adjudicated})
save(base / 'v12-target-scores.json', {'version': 1, 'evaluator': ref(evaluator / 'verify.py'),
    'adjudications': ref(base / 'v12-adjudications.json'), 'target_references': {pair: targets[pair]['references'] for pair in ('arxiv-bert-v1-to-v2', 'nist-sha-1803-to-1804', edpb_pair)},
    'scores': scores, 'status': 'Three reviewed natural pairs from three producers score B against the surviving historical repetition 2. EDPB is retained baseline capability, BERT is retained source-cut recovery, and NIST is one additional pair with a fixed 383-atom target. Repetitions and baselines are not additional pairs. The six-pair development and new unseen gates remain unmet.'})
print(json.dumps(scores, indent=2))
