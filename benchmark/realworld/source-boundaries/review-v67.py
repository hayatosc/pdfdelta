"""Adjudicate the eight additional development outputs from continuation lookup."""

import ast
from collections import Counter
import copy
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
BASE = ROOT / 'benchmark/realworld/source-boundaries'
sys.path.insert(0, str(ROOT / 'benchmark/realworld/remaining'))
import verify

verify.CONTRACT = 'source-boundaries-v1'


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return {'path': str(path.relative_to(ROOT)),
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def validators():
    namespace = {'sys': sys, 'Counter': Counter}
    path = BASE / 'review-v31.py'
    for node in ast.parse(path.read_text()).body:
        if isinstance(node, ast.FunctionDef) and node.name in ('node_sources', 'check_original_cuts'):
            exec(compile(ast.Module(body=[node], type_ignores=[]), str(path), 'exec'), namespace)
    return namespace


def side_check(review, side, source, root, check_cuts):
    glyphs = {g['id']: g for g in source['native']['items']}
    nodes = {n['id']: n for n in source['graph']['nodes']}
    cuts = review.get('source_cuts')
    if cuts:
        population = cuts['population']
        assert population['kind'] == 'matched_interval'
        assert not any(population.get(k) for k in ('row_order', 'native_regions', 'boundary_padding'))
        path = [nodes[n] for n in population[side]]
        check_cuts(review, side, source['graph'], glyphs)
    else:
        refs = review[side + '_boundaries'][0] + review[side + '_sources'] + review[side + '_boundaries'][1]
        ids = {r['glyph'] for r in refs}
        path = [n for n in nodes.values() if n['kind'] == 'paragraph'
                and n['basis'] == {'kind': 'native_layout'}
                and any(r.get('glyph') in ids for r in n['sources'])]
        path.sort(key=lambda n: -max(glyphs[r['glyph']]['baseline']['y'] for r in n['sources']))
        assert [r for n in path for r in n['sources']] == refs
    refs = [r for n in path for r in n['sources']]
    assert all(r['origin'] == 'native' for r in refs)
    ids = [r['glyph'] for r in refs]
    assert len(ids) == len(set(ids))
    selected = [glyphs[g] for g in ids]
    pages = {g['page'] for g in selected}
    assert len(pages) == 1
    page = pages.pop()
    assert all(n['pages'] == [page] for n in path)
    assert all(set(g['text']) == {'Mapped'}
               and g['direction'] == {'x': 1.0, 'y': 0.0}
               and g['crop_status'] == 'Inside'
               and g['path_clip_status'] in ('Inside', 'Unclipped')
               and g['render_mode'] in ('Fill', 'Stroke', 'FillAndStroke') for g in selected)
    bands = [(min(glyphs[r['glyph']]['baseline']['y'] for r in n['sources']),
              max(glyphs[r['glyph']]['baseline']['y'] for r in n['sources'])) for n in path]
    assert all(a[0] > b[1] for a, b in zip(bands, bands[1:]))
    x0 = min(g['bbox']['min']['x'] for g in selected)
    x1 = max(g['bbox']['max']['x'] for g in selected)
    y0, y1 = min(a for a, _ in bands), max(b for _, b in bands)
    census = {g['id'] for g in glyphs.values() if g['page'] == page
              and y0 <= g['baseline']['y'] <= y1
              and g['bbox']['max']['x'] >= x0 and g['bbox']['min']['x'] <= x1}
    assert census == set(ids)
    members = {n['id'] for n in path}
    for alternative in source['graph']['alternatives']:
        assert alternative['parent'] not in members | {root}
        assert all(not set(partition) & members for partition in alternative['partitions'])
    for conflict in source['graph']['source_conflicts']:
        assert not {r.get('glyph') for r in conflict['sources']} & census
    expected = {g['id'] for g in glyphs.values() if g['page'] == page}
    inventories = [i for i in source['inventories'] if i['channel'] == 'text' and i['page'] in (None, page)]
    assert inventories
    for inventory in inventories:
        assert inventory['page'] == page
        assert source['summary']['backends'][inventory['backend']]['kind'] == 'native_parser'
        assert len(inventory['sources']) == len(expected)
        assert {r['glyph'] for r in inventory['sources']} == expected
    assert not [i for i in source['summary']['issues'] if i['channel'] == 'text' and i['page'] in (None, page)]
    ink0 = min(g['bbox']['min']['y'] for g in selected)
    ink1 = max(g['bbox']['max']['y'] for g in selected)
    paints = [p for p in source['native']['non_text_paint_bounds'] if p['page'] == page]
    for paint in paints:
        bounds = paint['bounds']
        assert bounds is not None
        assert (bounds['max']['x'] < x0 or bounds['min']['x'] > x1
                or bounds['max']['y'] < ink0 or bounds['min']['y'] > ink1)
    if not all(i['complete'] for i in inventories):
        assert paints and str(page) in source['native']['last_non_text_paint']
    body = review[side + '_sources']
    raw = ''.join(glyphs[r['glyph']]['text']['Mapped'] for r in body)
    display = review['comparison']['operation'][side]
    folds = str.maketrans({'ﬀ': 'ff', 'ﬁ': 'fi', 'ﬂ': 'fl', 'ﬃ': 'ffi', 'ﬄ': 'ffl', 'ﬅ': 'st', 'ﬆ': 'st'})
    assert raw.translate(folds).replace(' ', '').replace('\n', '') == display.replace(' ', '').replace('\n', '')
    return {'page': page, 'glyphs': len(body), 'raw': raw, 'display': display,
            'population_nodes': [n['id'] for n in path], 'population_glyphs': len(ids),
            'source_census_exact': True, 'strictly_descending_node_bands': True,
            'original_cut_projection_checked': bool(cuts),
            'page_text_inventory_complete': all(i['complete'] for i in inventories),
            'disjoint_non_text_paint': paints}


def main():
    current = read(BASE / 'v67-panel-observations.json')
    targets = {r['pair']: r for r in read(ROOT / 'benchmark/realworld/followup/targets.json')['targets']}
    prior = {r['pair']: r for r in read(BASE / 'v65-panel-observations.json')['observations']}
    sources = {
        'irs-w9-2018-to-2024': ROOT / 'benchmark/realworld/cache/remaining-target-diagnosis/irs-w9-2018-to-2024',
        'arxiv-bert-v1-to-v2': ROOT / 'benchmark/realworld/cache/source-boundaries-baseline-bert-review',
    }
    restored_path = BASE / 'v51-adjudications.json'
    restored = {r['pair']: r for r in read(restored_path)['observations'] if r['repetition'] == 1}
    check_cuts = validators()['check_original_cuts']
    records = []
    for observation in current['observations']:
        pair = observation['pair']
        before = {verify.event_digest(e) for e in verify.events(verify.read_reference(prior[pair]['report']))}
        report = verify.read_reference(observation['report'])
        added = [e for e in verify.events(report) if verify.event_digest(e) not in before]
        if not added:
            continue
        directory = sources[pair]
        image_source = read(directory / 'sources.json')
        source_directory = ROOT / 'benchmark/realworld/cache/source-boundaries-continuation-stream-v67/source-exports' / pair / 'review'
        source = read(source_directory / 'sources.json')
        old_report = verify.read_reference(restored[pair]['report'])
        old_events = {verify.event_digest(e): e for e in verify.events(old_report)}
        old_reviews = {r['event_sha256']: r for r in restored[pair]['events']}
        for event in added:
            digest = verify.event_digest(event)
            assert event['category'] == 'B'
            if digest in old_reviews:
                assert event['review'] == old_events[digest]['review']
                row = copy.deepcopy(old_reviews[digest])
                row['pointer'] = event['pointer']
                row['source_evidence'] += [ref(restored_path), restored[pair]['report']]
                records.append({'pair': pair, 'report': observation['report'], 'event': row,
                                'restored_complete_review': True})
                continue
            for side in ('old', 'new'):
                assert report[side]['revision'] == source[side]['summary']['revision']
                assert report[side]['revision'] == image_source[side]['summary']['revision']
            review = event['review']
            result = report['comparison']['scopes'][0]['result']
            root = result['matching']['scope']
            checks = {side: side_check(review, side, source[side], root[side], check_cuts)
                      for side in ('old', 'new')}
            proof = review['comparison'].get('text_change_proof')
            if proof:
                token = proof['token']['Scalar']
                for side in ('old', 'new'):
                    count = Counter(checks[side]['raw'])[token]
                    assert count == proof[side + '_required'] == proof[side + '_possible']
                assert proof['old_required'] != proof['new_required']
            else:
                assert checks['old']['raw'].replace(' ', '') != checks['new']['raw'].replace(' ', '')
            pages = []
            for side, check in checks.items():
                rendering = next(r for r in image_source[side]['summary']['rendered_sources'] if r['page'] == check['page'])
                pages.append(ref(directory / f"{side}-region-{rendering['id']}.png"))
            rationale = ('The complete BERT abstract visibly changes model wording and reported benchmark results; '
                         'the original source has four versus five opening parentheses, independently matching the multiplicity proof.') if pair.startswith('arxiv-') else (
                         'The displayed W-9 source range visibly changes credit-report wording, telephone prefixes, '
                         'Visit/Go to wording, or the inline third-party hyphen. Raw parent and literal-space refinement views '
                         'are separate non-owning ranges, not additional author changes.')
            row = {'pointer': event['pointer'], 'event_sha256': digest, 'verdict': 'source_supported',
                   'source_content_rationale': rationale,
                   'correspondence_rationale': 'Accepted native endpoints retain the same checked source interval. Original node cuts, complete source census, strictly descending node bands and disjoint paint were checked independently against the source export. This is a conditional finite range, not a semantic paragraph identity.',
                   'source_evidence': [ref(source_directory / 'sources.json'), ref(source_directory / 'manifest.json'),
                                       ref(directory / 'sources.json'), ref(directory / 'manifest.json'),
                                       targets[pair]['references']['annotation'],
                                       targets[pair]['references']['resolution'],
                                       ref(BASE / 'review-v31.py'), ref(Path(__file__)), *pages]}
            records.append({'pair': pair, 'report': observation['report'], 'event': row,
                            'source_checks': checks, 'reviewed_pages': pages})
    assert len(records) == 8
    assert sum(r.get('restored_complete_review', False) for r in records) == 1
    output = {'version': 67, 'build': current['build'], 'reproducer': ref(Path(__file__)),
              'scope': 'Seven newly source-reviewed finite B ranges and one restored complete V51 review. All six newly inspected W-9/BERT page images were checked against their source text. No additional natural target pair is claimed.',
              'records': records}
    (BASE / 'v67-added-source-reviews.json').write_text(json.dumps(output, indent=2) + '\n')
    print('Eight additional development outputs checked against original source evidence.')


if __name__ == '__main__':
    main()
