"""Compare actual covered source identities with a reconstructed frozen baseline."""
import argparse
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def covered_glyphs(report, mapping, side):
    nodes = {str(node) for scope in report['comparison']['scopes']
             for pair in scope['result']['comparisons']
             if pair['compared'] and pair['interpretation'] == 'conditional_on_correspondence'
             for node in pair[side]}
    return {glyph for node in nodes if node in mapping['nodes']
            for glyph in mapping['nodes'][node]['glyphs']}


def field_sources(report, side):
    return {field['id']: dict(object=field['object'], name=field['value']['name'],
                              field_type=field['value']['field_type'],
                              value=field['value']['value'])
            for field in report[side]['form_fields']}


def check(frozen, baseline, candidate, baseline_maps, candidate_maps):
    pair, route = frozen['pair'], frozen['route']
    paths = [root / 'trials' / pair / route / 'report.json' for root in (baseline, candidate)]
    old, new = [json.loads(path.read_text()) for path in paths]
    old_summary = json.loads((paths[0].parent / 'operational.json').read_text())
    metric_keys = ('conditional_operations', 'inferred_operations',
                   'conditional_text_mask_positions', 'inferred_text_mask_positions')
    metric_parity = all(old_summary[key] == frozen['observations'][key] for key in metric_keys)
    channels = []
    for before in frozen['observations']['coverage']:
        channel = before['channel']
        reconstructed = next(c for c in old['coverage'] if c['channel'] == channel)
        after = next(c for c in new['coverage'] if c['channel'] == channel)
        for side in ('old', 'new'):
            count_key = side + '_compared_sources'
            if before[count_key] == 0:
                continue
            checks = dict(frozen_counts_reproduced=reconstructed[count_key] == before[count_key],
                          frozen_operation_metrics_reproduced=metric_parity,
                          same_input=old[side]['revision'] == new[side]['revision'])
            if channel == 'text':
                maps = [root / f'{pair}-{side}.json' for root in (baseline_maps, candidate_maps)]
                snapshots = [json.loads(path.read_text()) for path in maps]
                sets = [covered_glyphs(report, mapping, side)
                        for report, mapping in zip((old, new), snapshots)]
                checks['projection_counts_match'] = all(
                    len(sources) == coverage[count_key]
                    for sources, coverage in zip(sets, (reconstructed, after)))
                source_kind = 'native_glyph_id'
            elif channel == 'forms':
                # These baseline rows compare every discovered stored-field slot.
                # Incomplete appearance inventories remain incomplete.
                sets = [{field['id'] for field in report[side]['form_fields']} for report in (old, new)]
                checks['all_discovered_fields_compared'] = all(
                    len(sources) == coverage[count_key] == coverage[side + '_discovered_sources']
                    for sources, coverage in zip(sets, (reconstructed, after)))
                checks['field_sources_identical'] = field_sources(old, side) == field_sources(new, side)
                source_kind = 'structured_field_id'
                maps = []
            else:
                raise ValueError(f'No source-identity projection for baseline channel {channel}')
            lost = sets[0] - sets[1]
            checks['no_baseline_source_loss'] = not lost
            channels.append(dict(channel=channel, side=side, source_kind=source_kind,
                                 baseline_sources=sorted(sets[0]), candidate_source_count=len(sets[1]),
                                 lost_sources=sorted(lost), checks=checks,
                                 map_hashes={str(path): sha(path) for path in maps}))
    return dict(pair=pair, route=route, channels=channels,
                report_hashes={str(path): sha(path) for path in paths},
                supported=all(all(row['checks'].values()) for row in channels))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('frozen_routes', 'baseline', 'candidate', 'baseline_maps', 'candidate_maps'):
        parser.add_argument(name, type=Path)
    args = parser.parse_args()
    frozen = json.loads(args.frozen_routes.read_text())['routes']
    selected = [row for row in frozen if row['route'] != 'native' and row['observations']
                and any(c[s + '_compared_sources'] for c in row['observations']['coverage']
                        for s in ('old', 'new'))]
    rows = [check(row, args.baseline, args.candidate, args.baseline_maps, args.candidate_maps)
            for row in selected]
    print(json.dumps(dict(
        contract='Supplemental source-ID retention evidence. The exact baseline revision is reconstructed; original frozen measurements remain authoritative. Native graph nodes precede appended provider nodes in both builds. Projection counts must equal reported text coverage. Stored-field checks require identical source records and full discovered-slot coverage. Missing inventories are not certified complete.',
        frozen_routes_sha256=sha(args.frozen_routes), rows=rows,
        supported=all(row['supported'] for row in rows)), indent=2))
