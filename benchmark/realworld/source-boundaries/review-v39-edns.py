"""Check the exposed EDNS page-cut pilot against its independent raw export."""

from collections import Counter
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'benchmark/realworld/remaining'))
import verify

verify.CONTRACT = 'source-boundaries-v1'
BASE = ROOT / 'benchmark/realworld/source-boundaries'
CACHE = ROOT / 'benchmark/realworld/cache/source-boundaries-unseen-v2'
PAIR = 'ietf-edns-2671-to-6891'


def read(path):
    return json.loads(path.read_text())


def ref(path):
    return dict(path=str(path.relative_to(ROOT)), sha256=hashlib.sha256(path.read_bytes()).hexdigest())


def node_sources(node, start, end):
    view = node['content']['view']
    assert 0 <= start <= end <= len(view['tokens'])
    selected = {source['glyph'] for index in range(start, end) if view['source_backed'][index]
                for source in view['origins'][index] if source['origin'] == 'native'}
    return [source for source in node['sources'] if source['glyph'] in selected]


def occurrence_count(text, needle):
    count = 0
    position = text.find(needle)
    while position >= 0:
        count += 1
        position = text.find(needle, position + 1)
    return count


def main():
    report_path = CACHE / 'development-v39-edns' / (PAIR + '-text.json')
    source_path = CACHE / 'diagnosis' / PAIR / 'source-review/sources.json'
    report = read(report_path)
    source = read(source_path)
    target = next(row for row in read(BASE / 'unseen-v2/targets.json')['targets'] if row['pair'] == PAIR)
    core, extent, controls = verify.historical.target_sources(target)
    assert core == extent
    result = report['comparison']['scopes'][0]['result']
    events = verify.events(report)
    reviewed = []
    for event in events:
        review = event.get('review', {})
        cuts = review.get('source_cuts')
        if cuts is None or cuts['population']['kind'] != 'anchored_page':
            continue
        population = cuts['population']
        for side in ('old', 'new'):
            original = source[side]
            assert original['summary']['revision'] == report[side]['revision']
            graph = original['graph']
            assert not graph['alternatives'] and not graph['source_conflicts']
            assert not original['native']['last_non_text_paint']
            assert not original['native']['non_text_paint_bounds']
            nodes = {node['id']: node for node in graph['nodes']}
            glyphs = {glyph['id']: glyph for glyph in original['native']['items']}
            page = population[side + '_page']
            members = population[side]
            assert all(nodes[node]['pages'] == [page] for node in members)
            raw = [glyph for glyph in original['native']['items'] if glyph['page'] == page]
            refs = [atom for node in members for atom in nodes[node]['sources']]
            assert refs == [{'origin': 'native', 'glyph': glyph['id']} for glyph in raw]
            assert len(refs) == len({atom['glyph'] for atom in refs})
            inventory = [row for row in original['inventories']
                         if row['channel'] == 'text' and row['page'] == page]
            assert len(inventory) == 1 and inventory[0]['complete']
            assert inventory[0]['sources'] == refs
            assert all(set(glyph['text']) == {'Mapped'}
                       and glyph['direction'] == {'x': 1.0, 'y': 0.0}
                       and glyph['crop_status'] == 'Inside'
                       and glyph['path_clip_status'] in ('Unclipped', 'Inside')
                       and glyph['render_mode'] in ('Fill', 'Stroke', 'FillAndStroke') for glyph in raw)
            assert all(a['render_order'] < b['render_order']
                       and (a['baseline']['y'] > b['baseline']['y']
                            or (a['baseline']['y'] == b['baseline']['y']
                                and a['baseline']['x'] <= b['baseline']['x']))
                       for a, b in zip(raw, raw[1:]))
            text = lambda atoms: ''.join(glyphs[atom['glyph']]['text']['Mapped'] for atom in atoms)
            full_text = text(refs)
            first, last = cuts['entry'][side], cuts['exit'][side]
            a, b = members.index(first['node']), members.index(last['node'])
            expected = []
            for index in range(a, b + 1):
                node = nodes[members[index]]
                start = first['token_boundary'] if index == a else 0
                end = last['token_boundary'] if index == b else len(node['content']['view']['tokens'])
                expected.extend(node_sources(node, start, end))
            assert expected == review[side + '_sources']
            assert text(expected) == review['comparison']['operation'][side]
            for name in ('entry', 'exit'):
                evidence = cuts[name]['evidence']
                if evidence['kind'] == 'unique_native_fragment':
                    fragment = evidence[side]
                    atoms = node_sources(nodes[fragment['node']], *fragment['tokens'])
                    assert atoms == fragment['sources']
                    assert occurrence_count(full_text, text(atoms)) == 1
            refinement = cuts.get('edge_refinement')
            if refinement:
                for edge in refinement[side + '_padding']:
                    for fragment in edge:
                        atoms = node_sources(nodes[fragment['node']], *fragment['tokens'])
                        assert atoms == fragment['sources'] and set(text(atoms)) == {' '}
            for index in population['external_boundaries']:
                proposal = result['candidates']['proposals'][index]
                foreign = {atom['glyph'] for node in proposal[side] for atom in nodes[node]['sources']}
                assert not foreign & {atom['glyph'] for atom in expected}
        reviewed.append(dict(pointer=event['pointer'], category=event['category'],
                             source_atoms=len(event['sources']), exact_target=event['sources'] == core,
                             verdict='source_supported', operation=event['operation'],
                             source_projection=event['source_projection']))
    assert len(reviewed) == 4 and sum(row['exact_target'] for row in reviewed) == 1
    assert all(not event['sources'] & controls for event in events if event['category'] == 'A')
    output = dict(version=1, pair=PAIR, development_only=True, report=ref(report_path),
                  raw_source_export=ref(source_path), target=target['references'],
                  source_page_review=ref(BASE / 'unseen-v2/source-review.json'),
                  categories=dict(Counter(event['category'] for event in events)),
                  checked_new_outputs=reviewed,
                  rationale='The registered first abstract paragraph changes clients/servers to requestors/responders and backward compatible to backward-compatible. All four additional ranges contain those prose changes; two retain outer raw spaces and two exclude only those spaces. The 584-glyph view equals the immutable target. Complete page source inventories, literal fragment uniqueness, original token cuts, raw text, source order and the moved Copyright Notice exclusion were checked independently. Previously existing three outputs are outside this new-output review.')
    (BASE / 'v39-edns-adjudication.json').write_text(json.dumps(output, indent=2) + '\n')
    print('Four new outputs source-supported; one exact 584-glyph target.')


if __name__ == '__main__':
    main()
