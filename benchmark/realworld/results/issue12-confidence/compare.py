"""Compare every recorded field, allowing only the intended confidence count change."""
import hashlib
import json
import pathlib

root = pathlib.Path(__file__).resolve().parent
before_path = root / 'before/all.json'
after_path = root / 'after/all.json'
before = json.loads(before_path.read_text())
after = json.loads(after_path.read_text())
assert len(before) == len(after) == 29
assert before.keys() == after.keys()


def differences(left, right, path=''):
    if type(left) is not type(right):
        return [{'path': path, 'before': left, 'after': right}]
    if isinstance(left, dict):
        assert left.keys() == right.keys(), path
        return [difference for key in sorted(left)
                for difference in differences(left[key], right[key], path + '/' + key)]
    if isinstance(left, list):
        if len(left) == len(right):
            return [difference for i, (a, b) in enumerate(zip(left, right))
                    for difference in differences(a, b, path + '/' + str(i))]
    if left != right:
        return [{'path': path, 'before': left, 'after': right}]
    return []


records = []
for pair in sorted(before):
    changes = differences(before[pair], after[pair])
    assert all(change['path'] == '/records/0/reported_uncertain_changes'
               for change in changes), (pair, changes)
    a = before[pair]['records'][0]
    b = after[pair]['records'][0]
    records.append({
        'pair': pair,
        'status': b['status'],
        'limit_scale_used': b['limit_scale_used'],
        'compared': b['compared'],
        'resource_limit_failure': b['resource_limit_failure'],
        'coverage_before': {'old': a['coverage_old'], 'new': a['coverage_new']},
        'coverage_after': {'old': b['coverage_old'], 'new': b['coverage_new']},
        'differences': changes,
    })
report = {
    'issue_resolved': False,
    'comparison': 'HEAD plus baseline.patch versus HEAD plus candidate.patch',
    'pair_count': len(records),
    'parity_except_reported_uncertain_changes': True,
    'captures': {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in (before_path, after_path)},
    'limitations': [
        'No unavailable metric is interpreted as zero or as a precision guarantee.',
        'Resource-limit failures are recorded, not counted as successful comparisons.',
        'CSF still has two expected reading-order failures; this fix preserves inferred-order confidence.'
    ],
    'records': records,
}
(root / 'comparison.json').write_text(json.dumps(report, indent=2) + '\n')
print('All 29 captures match except reported_uncertain_changes.')
