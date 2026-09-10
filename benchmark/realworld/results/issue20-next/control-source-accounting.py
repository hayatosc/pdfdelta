"""Bind identical-input control scopes to completed source-backed comparisons."""
import argparse
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check(root, form, side, year, route, freeze):
    selectors_path = root / f'irs{form}-selectors.json'
    map_path = root / f'f{form}--{year}.scalar-bindings.json'
    identity_path = root / f'f{form}--{year}.native-identities.json'
    report_path = root / 'trials' / f'irs{form}' / f'{side}-control' / route / 'report.json'
    selectors = json.loads(selectors_path.read_text())
    source_map = json.loads(map_path.read_text())
    identities = json.loads(identity_path.read_text())['nodes']
    terminals = [(key, node) for key, node in identities.items() if node.get('identity')
                 and node['identity']['namespace'].startswith('terminal-catalog-form-v1/')]
    if len(terminals) != 1:
        raise ValueError('control needs exactly one source-backed terminal identity')
    node_id, node = terminals[0]
    bindings = source_map['nodes'][node_id]
    report = json.loads(report_path.read_text())
    scalar = json.loads((root / f'irs{form}-{side}-{route}-scalar-coverage.json').read_text())
    witnesses = []
    for scope_index, scope in enumerate(report['comparison']['scopes']):
        if scope['interpretation'] != 'conditional_on_correspondence':
            continue
        result = scope['result']
        for proposal_index, proposal in enumerate(result['candidates']['proposals']):
            if (proposal['old'] != [int(node_id)] or proposal['new'] != [int(node_id)]
                    or proposal['basis'] != 'scoped_identity'):
                continue
            components = [c for c in result['matching']['components']
                          if proposal_index in c['proposals']]
            for comparison_index, comparison in enumerate(result['comparisons']):
                if comparison['old'] != proposal['old'] or comparison['new'] != proposal['new']:
                    continue
                mask = comparison['text_mask']
                claims = mask['claims'] if mask else {}
                checks = dict(
                    accepted=proposal_index in result['accepted_correspondences'],
                    source_only_mandatory=proposal_index in result['matching']['source_only_mandatory'],
                    certified_mandatory=len(components) == 1
                        and proposal_index in components[0]['mandatory'],
                    non_inferred=comparison['interpretation'] == 'conditional_on_correspondence',
                    compared=comparison['compared'], no_local_unresolved=not comparison['unresolved'],
                    zero_change_bound=claims.get('changed_source_lower') == 0
                        and claims.get('changed_source_upper') == 0,
                    full_view_lengths=all(len(claims.get('mandatory_'+s, [])) == bindings['token_count']
                                          for s in ('old', 'new')),
                    no_extraction_dependencies=not result['extraction_dependencies'])
                witnesses.append(dict(scope=scope_index, proposal=proposal_index,
                                      comparison=comparison_index, checks=checks,
                                      residual_search_complete=len(components) == 1
                                          and components[0]['exhaustive']))
    input_hash = sha(root / f'f{form}--{year}.pdf')
    checks = dict(
        frozen_binary_hash=sha(Path(freeze['directory']) / 'pdfdelta') == freeze['pdfdelta_sha256'],
        input_hashes=report['old']['revision'] == report['new']['revision']
            == selectors[side]['sha256'] == input_hash,
        all_reviewed_scalars=scalar['reviewed_scalars'] == scalar['covered_scalars'],
        no_text_extraction_issues=all(not [i for i in report[s]['issues'] if i['channel'] == 'text']
                                     for s in ('old', 'new')),
        all_terminal_glyphs_bound=all(str(g) in bindings['single_scalar_bindings'] for g in node['glyphs']),
        zero_established_changes=report['typed_changes'] == 0)
    return dict(pair=f'irs{form}', control=side, route=route,
                inputs={str(p): sha(p) for p in (report_path, map_path, identity_path, selectors_path)},
                identity=node['identity'], node=int(node_id), reviewed_scalars=scalar['reviewed_scalars'],
                terminal_glyphs=len(node['glyphs']), source_checks=checks, witnesses=witnesses,
                scoped_source_accounting_supported=all(checks.values())
                    and any(all(w['checks'].values()) for w in witnesses),
                document_complete=report['comparison_complete'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('freeze', type=Path)
    parser.add_argument('forms', type=int, nargs='+')
    args = parser.parse_args()
    freeze = json.loads(args.freeze.read_text())
    rows = [check(args.root, form, side, year, route, freeze) for form in args.forms
            for side, year in zip(('old', 'new'),
                                  freeze.get('source_years', {}).get(str(form), (2024, 2025)))
            for route in ('shared_text', 'shared_all')]
    print(json.dumps(dict(status='scoped_text_source_accounting_evidence', candidate=str(args.freeze),
                          contract='Complete literal terminal views under accepted source-only mandatory identity proposals; no text extraction issues or extraction dependencies. Broader document completeness is separate.',
                          mapping_configuration=freeze.get('mapping_configuration', 'See candidate freeze for source-map library and configuration.'),
                          run_script_sha256=sha(args.root / 'run-trial.sh'), rows=rows), indent=2))
