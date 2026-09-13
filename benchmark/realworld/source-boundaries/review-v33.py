"""Recheck all additional V33 outputs against original glyphs and reviewed pages."""
from pathlib import Path
from collections import Counter
import ast, json, sys, hashlib
ROOT = Path(__file__).resolve().parents[3]
import os
os.chdir(ROOT)
sys.path.insert(0, str(ROOT / 'benchmark/realworld/remaining'))
import verify
verify.CONTRACT = 'source-boundaries-v1'
helper = Path('benchmark/realworld/source-boundaries/review-v31.py')
module = ast.parse(helper.read_text())
# Import only the pure validators: executing the historical script would rewrite
# its frozen observations. The assembly record binds this helper file by hash.
ns = {'sys': sys, 'Counter': Counter}
for x in module.body:
    if isinstance(x, ast.FunctionDef) and x.name in ['node_sources', 'check_original_cuts', 'check_paint_population']:
        exec(compile(ast.Module(body=[x], type_ignores=[]), str(helper), 'exec'), ns)
base = Path('benchmark/realworld/cache')
directories = {'edpb-restrictions-v1-to-final': 'source-boundaries-native-order-v33/review', 'edpb-social-targeting-v1-to-v2': 'source-boundaries-native-order-v33/edpb-social-targeting-v1-to-v2-source-review', 'irs-w9-2018-to-2024': 'remaining-target-diagnosis/irs-w9-2018-to-2024', 'arxiv-gpt3-v1-to-v4': 'remaining-target-diagnosis/arxiv-gpt3-v1-to-v4', 'arxiv-mask-rcnn-v1-to-v3': 'resume-mask-spatial-bridge-review'}
for pair in ['irs-w4-english-2024-to-2025', 'irs-w2-2024-to-2025', 'arxiv-ddpm-v1-to-v2']:
    directories[pair] = 'source-boundaries-native-order-v33/' + pair + '-source-review'
observations = json.load(open('benchmark/realworld/source-boundaries/v33-panel-observations.json'))['observations']

def projected_content(review, side, source, selected, displayed):
    positions = {glyph: index for index, glyph in enumerate(selected)}
    assert len(positions) == len(selected)
    glyphs = {g['id']: g for g in source['native']['items']}
    nodes = [n for n in source['graph']['nodes'] if n['kind'] == 'paragraph' and n['basis'] == {'kind': 'native_layout'} and any((r.get('glyph') in positions for r in n['sources']))]
    nodes.sort(key=lambda n: min((positions[r['glyph']] for r in n['sources'] if r.get('glyph') in positions)))
    backed = {}
    ordered = []
    seen = set()
    newlines = []
    for node in nodes:
        native = {r['glyph'] for r in node['sources'] if r['origin'] == 'native'} & positions.keys()
        assert not seen & native
        seen.update(native)
        view = node['content']['view']
        for token, origins, is_backed in zip(view['tokens'], view['origins'], view['source_backed']):
            if not is_backed:
                if token.get('Scalar') == '\n' and any((r.get('glyph') in positions for r in origins)):
                    assert len(origins) == 2 and all((r.get('glyph') in positions for r in origins))
                    a, b = [glyphs[r['glyph']] for r in origins]
                    assert a['page'] != b['page'] or a['baseline']['y'] != b['baseline']['y']
                    newlines.append(origins)
                continue
            if token.get('Scalar') == ' ':
                continue
            chosen = [r['glyph'] for r in origins if r.get('glyph') in positions]
            if not chosen:
                continue
            assert len(origins) == len(chosen) == 1 and 'Scalar' in token
            backed.setdefault(chosen[0], []).append(token['Scalar'])
            ordered.append(token['Scalar'])
    assert seen == positions.keys()
    expand = str.maketrans({'ﬀ': 'ff', 'ﬁ': 'fi', 'ﬂ': 'fl', 'ﬃ': 'ffi', 'ﬄ': 'ffl', 'ﬅ': 'st', 'ﬆ': 'st'})
    for glyph in selected:
        raw = glyphs[glyph]['text']['Mapped'].translate(expand).replace(' ', '')
        assert ''.join(backed.get(glyph, [])) == raw, (glyph, raw, backed.get(glyph))
    assert len(newlines) == displayed.count('\n')
    assert ''.join(ordered) == displayed.replace(' ', '').replace('\n', ''), ('retained node token order', ''.join(ordered), displayed)
    return {'nodes': [n['id'] for n in nodes], 'glyph_projection_checked': True, 'layout_newlines': newlines, 'profile': 'Original native node order with the seven declared Latin presentation-ligature folds; source spaces retain separate provenance.'}

def check_strict_footer(event, report, source):
    """Independently enumerate the only two-token edit of the exact footer views."""
    scope, index = [int(event['pointer'].split('/')[position]) for position in (3, 6)]
    comparison = report['comparison']['scopes'][scope]['result']['comparisons'][index]
    views = {}
    texts = {}
    projected = set()
    for side in ['old', 'new']:
        assert len(comparison[side]) == 1
        node = next((n for n in source[side]['graph']['nodes'] if n['id'] == comparison[side][0]))
        assert node['kind'] == 'footer' and node['basis'] == {'kind': 'native_layout'}
        view = views[side] = node['content']['view']
        assert view['normalization'] == {'kind': 'exact'}
        assert all((set(t) == {'Scalar'} and len(t['Scalar']) == 1 for t in view['tokens']))
        texts[side] = ''.join((t['Scalar'] for t in view['tokens']))
        assert texts[side] == event['operation'][side]
    a, b = (texts['old'], texts['new'])
    assert len(a) == len(b) and a != b
    solutions = [(i, j) for i in range(len(a)) for j in range(len(b)) if a[:i] + a[i + 1:] == b[:j] + b[j + 1:]]
    assert len(solutions) == 1
    mask = event['source_projection']
    claims = mask['claims']
    assert claims['changed_source_lower'] == claims['changed_source_upper'] == 2
    for side, position in zip(['old', 'new'], solutions[0]):
        view = views[side]
        assert claims['mandatory_' + side] == [i == position for i in range(len(view['tokens']))]
        assert view['source_backed'][position]
        refs = view['origins'][position]
        assert len(refs) == 1 and refs[0]['origin'] == 'native'
        assert mask[side] == [{'position': position, 'sources': refs}]
        glyph = next((g for g in source[side]['native']['items'] if g['id'] == refs[0]['glyph']))
        assert glyph['text'] == {'Mapped': texts[side][position]} and glyph['page'] == 0
        assert glyph['crop_status'] == 'Inside' and glyph['path_clip_status'] in ['Inside', 'Unclipped']
        assert glyph['render_mode'] in ['Fill', 'Stroke', 'FillAndStroke']
        projected.add((side, glyph['id']))
    assert projected == event['sources']
    return {'profile': 'Exact original footer views; unique minimum one-token deletion per side.', 'old_position': solutions[0][0], 'new_position': solutions[0][1], 'changed_sources': sorted(projected), 'minimum_changed_source_atoms': 2}
records = []
for obs in observations:
    pair = obs['pair']
    if pair not in directories:
        continue
    directory = base / directories[pair]
    source = json.loads((directory / 'sources.json').read_text())
    report = json.load(open(obs['report']['path']))
    glyphs = {side: {g['id']: g for g in source[side]['native']['items']} for side in ['old', 'new']}
    for e in verify.events(report):
        row = {'pair': pair, 'pointer': e['pointer'], 'category': e['category'], 'digest': verify.event_digest(e), 'source_path': str(directory / 'sources.json'), 'sides': {}, 'errors': []}
        review = e.get('review')
        row['cut_profile'] = review.get('source_cuts', {}).get('projection') if review and review.get('source_cuts') else None
        if review:
            row['enclosing'] = review.get('source_cuts') or review['boundaries']
        for side in ['old', 'new']:
            assert report[side]['revision'] == source[side]['summary']['revision']
            ids = [r['glyph'] for r in review[side + '_sources']] if review else sorted((i for s, i in e['sources'] if s == side))
            raw = [glyphs[side][i] for i in ids]
            text = ''.join((g['text'].get('Mapped', '<?>') for g in raw))
            operation = e['operation']
            display = operation[side]
            row['sides'][side] = {'pages': sorted(set((g['page'] for g in raw))), 'glyphs': len(ids), 'raw': text, 'display': display}
            if review:
                if text.replace(' ', '') != display.replace(' ', ''):
                    try:
                        row['sides'][side]['retained_projection'] = projected_content(review, side, source[side], ids, display)
                    except (AssertionError, KeyError, TypeError, ValueError) as ex:
                        row['errors'].append(side + ': raw projection ' + str(ex))
                if not all((g['crop_status'] == 'Inside' and g['path_clip_status'] in ['Inside', 'Unclipped'] and (g['render_mode'] in ['Fill', 'Stroke', 'FillAndStroke']) for g in raw)):
                    row['errors'].append(side + ': source visibility')
                try:
                    ns['check_original_cuts'](review, side, source[side]['graph'], glyphs[side])
                    cuts = review.get('source_cuts')
                    order = cuts['population'].get('row_order') if cuts else None
                    if order:
                        if order['convention'] == 'horizontal-paint-row-boundaries-v1':
                            si = int(e['pointer'].split('/')[3])
                            root = report['comparison']['scopes'][si]['result']['matching']['scope'][side]
                            row['sides'][side]['paint_population'] = ns['check_paint_population'](review, side, source[side], root)
                        else:
                            row['errors'].append(side + ': spatial row needs independent review')
                except (AssertionError, KeyError, TypeError, ValueError) as ex:
                    row['errors'].append(side + ': ' + type(ex).__name__ + ' ' + str(ex))
        if e['category'] == 'A':
            row['strict_source_proof'] = check_strict_footer(e, report, source)
        records.append(row)
Path('benchmark/realworld/source-boundaries/v33-extra-source-checks.json').write_text(json.dumps(records, indent=2) + '\n')
print(len(records), 'events;', sum((bool(r['errors']) for r in records)), 'requiring further source checks')
for r in records:
    if r['errors']:
        print(r['pair'], r['pointer'], r['errors'])
