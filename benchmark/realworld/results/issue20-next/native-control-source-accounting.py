"""Bind identical-input native controls to their declared text-resolution evidence."""
import argparse
import hashlib
import json
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def glyphs(span):
    return {source['glyph_id'] for source in (span or {}).get('sources', [])
            if source['kind'] == 'glyph'}


def check(root, form, side, year, freeze):
    report_path = root / 'trials' / f'irs{form}' / f'{side}-control' / 'native' / 'report.json'
    selector_path = root / f'irs{form}-selectors.json'
    pdf_path = root / f'f{form}--{year}.pdf'
    report = json.loads(report_path.read_text())
    selectors = json.loads(selector_path.read_text())
    scalars = [source for expected in selectors['selectors']['expectations']
               for source in expected[side]['sources']]
    expected = {atom['id'] for scalar in scalars for atom in scalar['atoms']
                if atom['kind'] == 'glyph'}
    assessment = report['assessment']
    resolved, conflicting = {}, {}
    for direction in ('old', 'new'):
        equal, other = set(), set()
        for resolution in assessment[direction + '_resolution']:
            (equal if resolution['state'] == 'equal' else other).update(glyphs(resolution))
        resolved[direction] = sorted(expected & equal)
        conflicting[direction] = sorted(expected & other)
    witnesses = [dict(relation=index, assumptions=relation['assumptions'])
                 for index, relation in enumerate(assessment['relations'])
                 if relation['outcome'] == 'established' and relation['search'] == 'complete'
                 and not relation['reasons']
                 and all(expected <= glyphs(relation[direction + '_span'])
                         for direction in ('old', 'new'))]
    checks = dict(
        input_hash=sha(pdf_path) == selectors[side]['sha256'],
        frozen_binary=sha(Path(freeze['directory']) / 'pdfdelta') == freeze['pdfdelta_sha256'],
        nonempty_literal_sources=bool(expected) and all(
            scalar['atoms'] and all(atom['kind'] == 'glyph' for atom in scalar['atoms'])
            for scalar in scalars),
        every_reviewed_glyph_equal=all(set(resolved[s]) == expected for s in ('old', 'new')),
        no_conflicting_resolution=not any(conflicting.values()),
        established_complete_scope_relation=bool(witnesses),
        extraction_complete=report['extraction']['old_complete']
            and report['extraction']['new_complete'] and not report['extraction']['issues'],
        zero_established_changes=report['summary']['established_changes'] == 0)
    return dict(pair=f'irs{form}', control=side, checks=checks,
                inputs={str(p): sha(p) for p in (report_path, selector_path, pdf_path)},
                reviewed_scalars=len(scalars), reviewed_glyphs=len(expected),
                equal_glyphs=resolved, conflicting_glyphs=conflicting, witnesses=witnesses,
                native_scoped_text_accounting_supported=all(checks.values()),
                document_complete=report['summary']['comparison_complete'])


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    parser.add_argument('freeze', type=Path)
    parser.add_argument('forms', nargs='+', type=int)
    args = parser.parse_args()
    freeze = json.loads(args.freeze.read_text())
    rows = [check(args.root, form, side, year, freeze) for form in args.forms
            for side, year in zip(('old', 'new'), freeze['source_years'][str(form)])]
    print(json.dumps(dict(
        contract='Identical-input controls under the native supported-text contract. Each reviewed glyph is equal on both sides, with no conflicting resolution, complete extraction, and an established complete enclosing relation. Relation assumptions remain explicit; broader document completeness is separate.',
        rows=rows), indent=2))
