"""Prepare the second measured run; retain the unsuccessful first run unchanged."""
from pathlib import Path

fixture = Path('crates/pdfdelta-core/tests/evidence_document_fixture.rs')
old = '''Some(if nested {
                    (0.0, 0.0, 100.0, 100.0)
                } else {
                    (10.0, 10.0, 20.0, 20.0)
                })'''
new = 'Some((10.0, 10.0, 20.0, 20.0))'
s = fixture.read_text()
assert s.count(old) == 1
fixture.write_text(s.replace(old, new))

path = Path('.experiment/iterate.py')
s = path.read_text().replace('paint-locality-v1', 'paint-locality-v2')
s = s.replace('changed = core+[tests,cache]', "changed = core+[tests,cache,Path('crates/pdfdelta-core/tests/evidence_document_fixture.rs')]")
s = s.replace("STATE['stage'] = 'quality_failed'; save(); return", "STATE['stage'] = 'quality_failed'; save(); raise RuntimeError('quality gate failed')")
s = s.replace("if number == len(pilot_ids): publish('test(experiment): publish measured six-pair paint-locality pilot')", "if number == len(pilot_ids):\n            STATE['evaluated_pair_count'] = number\n            STATE['scope'] = 'six-pair ablation; not a full-panel rerun'\n            break")
s = s.replace("STATE['stage'] = 'finished'; save()", "STATE['stage'] = 'finished_pilot'; save()")
s = s.replace("publish('test(experiment): publish fixed-panel paint-locality comparison results')", "publish('test(experiment): publish six-pair paint-locality comparison results')")
path.write_text(s)
