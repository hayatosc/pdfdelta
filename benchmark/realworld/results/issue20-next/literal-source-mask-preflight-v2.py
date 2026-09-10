"""Independently check full literal source masks before comparison output exists."""
import argparse
import json
from pathlib import Path


def analyze(a, b, source_a, source_b):
    n, m = len(a), len(b)
    if (n + 1) * (m + 1) > 1_000_000:
        raise ValueError('source mask matrix exceeds one million cells')
    forward = [[0] * (m + 1) for _ in range(n + 1)]
    reverse = [[0] * (m + 1) for _ in range(n + 1)]
    for i in range(n + 1):
        forward[i][0], reverse[i][m] = i, n - i
    for j in range(m + 1):
        forward[0][j], reverse[n][j] = j, m - j
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            forward[i][j] = min(forward[i-1][j]+1, forward[i][j-1]+1,
                                forward[i-1][j-1] if a[i-1] == b[j-1] else n+m+1)
    for i in range(n-1, -1, -1):
        for j in range(m-1, -1, -1):
            reverse[i][j] = min(reverse[i+1][j]+1, reverse[i][j+1]+1,
                                reverse[i+1][j+1] if a[i] == b[j] else n+m+1)
    cost = forward[n][m]
    bounds = [[None] * (m + 1) for _ in range(n + 1)]
    bounds[0][0] = (0, 0)
    matched, possible = [set(), set()], [set(), set()]
    for i in range(n + 1):
        for j in range(m + 1):
            edges = []
            if i < n:
                edges.append((i+1, j, 1, int(source_a[i]), 0, i))
            if j < m:
                edges.append((i, j+1, 1, int(source_b[j]), 1, j))
            if i < n and j < m and a[i] == b[j]:
                edges.append((i+1, j+1, 0, 0, None, None))
            for x, y, edit, weight, side, position in edges:
                if forward[i][j] + edit + reverse[x][y] != cost:
                    continue
                if side is None:
                    matched[0].add(i)
                    matched[1].add(j)
                elif weight:
                    possible[side].add(position)
                if bounds[i][j] is not None:
                    low, high = bounds[i][j]
                    previous = bounds[x][y]
                    bounds[x][y] = (low+weight, high+weight) if previous is None else (
                        min(previous[0], low+weight), max(previous[1], high+weight))
    mandatory = [{i for i, backed in enumerate(source) if backed} - matched[side]
                 for side, source in enumerate((source_a, source_b))]
    lower, upper = bounds[n][m]
    return dict(edit_cost=cost, changed_source_lower=lower, changed_source_upper=upper,
                mandatory=[sorted(s) for s in mandatory], possible=[sorted(s) for s in possible],
                fully_localized=mandatory == possible and lower == upper == sum(map(len, mandatory)))


def check(root, form, years=(2024, 2025)):
    views, raws, glyph_maps, source_flags, raw_ids, identities = [], [], [], [], [], []
    for year in years:
        inventory = json.loads((root / f'f{form}--{year}.native-identities.json').read_text())
        keyed = [(key, node) for key, node in inventory['nodes'].items()
                 if node.get('identity') and node['identity']['namespace'].startswith('terminal-catalog-form-v1/')]
        if len(keyed) != 1:
            raise ValueError('source inventory does not contain exactly one terminal identity')
        key, node = keyed[0]
        mapping = json.loads((root / f'f{form}--{year}.scalar-bindings.json').read_text())['nodes'][key]
        token_glyphs, glyph_values = {}, {}
        for glyph in node['glyphs']:
            binding = mapping['single_scalar_bindings'].get(str(glyph), [])
            if len(binding) != 1:
                raise ValueError('terminal glyph lacks one literal source binding')
            position, value = binding[0]['token'], binding[0]['value']
            if position in token_glyphs or node['text'][position] != value:
                raise ValueError('terminal token has merged or conflicting source bindings')
            token_glyphs[position], glyph_values[glyph] = glyph, value
        if len(node['text']) != mapping['token_count'] or any(
                not value.isspace() for i, value in enumerate(node['text']) if i not in token_glyphs):
            raise ValueError('unbound token is not a synthetic layout separator')
        views.append(node['text'])
        raws.append(''.join(glyph_values[g] for g in node['glyphs']))
        source_flags.append([i in token_glyphs for i in range(len(node['text']))])
        glyph_maps.append(token_glyphs)
        raw_ids.append(node['glyphs'])
        identities.append(node['identity'])
    raw = analyze(*raws, [True]*len(raws[0]), [True]*len(raws[1]))
    view = analyze(*views, *source_flags)
    raw_masks = [{raw_ids[s][i] for i in raw['mandatory'][s]} for s in range(2)]
    view_masks = [{glyph_maps[s][i] for i in view['mandatory'][s]} for s in range(2)]
    return dict(form=form, identities_match=identities[0] == identities[1], raw_quotes=raws,
                raw=raw, view=view, raw_view_source_masks_agree=raw_masks == view_masks,
                eligible=identities[0] == identities[1] and raw['fully_localized']
                and view['fully_localized'] and raw_masks == view_masks)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('forms', nargs='+', type=int)
    parser.add_argument('--years', nargs=2, type=int, default=(2024, 2025))
    args = parser.parse_args()
    rows = []
    for form in args.forms:
        try:
            rows.append(check(args.root, form, args.years))
        except (ValueError, KeyError, IndexError) as error:
            rows.append(dict(form=form, eligible=False, source_error=str(error)))
    print(json.dumps(dict(contract='literal_source_and_synthetic_separator_preflight_v1',
                          rows=rows), indent=2))
