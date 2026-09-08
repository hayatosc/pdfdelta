"""Verify the recorded ambiguity witnesses using exact character LCS tables."""
from array import array
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
record = json.loads((ROOT / "boundary-witness.json").read_text())


def suffix_table(old, new):
    rows = [array("H", [0]) * (len(new) + 1) for _ in range(len(old) + 1)]
    for i in range(len(old) - 1, -1, -1):
        for j in range(len(new) - 1, -1, -1):
            rows[i][j] = max(rows[i + 1][j], rows[i][j + 1],
                             rows[i + 1][j + 1] + (old[i] == new[j]))
    return rows


def restricted_length(old, new, forbidden):
    previous = [0] * (len(new) + 1)
    for left in old:
        current = [0]
        for j, right in enumerate(new):
            allowed = left == right and not forbidden[0] <= j < forbidden[1]
            current.append(max(previous[j + 1], current[-1],
                               previous[j] + 1 if allowed else 0))
        previous = current
    return previous[-1]


old = (ROOT / "old-paragraph.txt").read_text()
new = (ROOT / "new-paragraph.txt").read_text()
score = suffix_table(old, new)
assert score[0][0] == record["lcs_length"] == 123
assert restricted_length(old, new, record["gold_new_range"]) == 43
for witness in record["distinct_optimal_insertion_boundaries"]:
    start, end = witness["new_range"]
    boundary, old_end = witness["old_range"]
    assert boundary == old_end
    assert old[:boundary] == new[:start]
    assert new[start:end] == witness["inserted_text"]
    assert boundary + score[boundary][end] == score[0][0]

old = (ROOT / "old-introduction.txt").read_text()
new = (ROOT / "new-introduction.txt").read_text()
expected = record["introduction_counterfactual"]
score = suffix_table(old, new)
prefix = [array("H", [0]) * (len(new) + 1) for _ in range(len(old) + 1)]
for i, left in enumerate(old):
    for j, right in enumerate(new):
        prefix[i + 1][j + 1] = max(prefix[i][j + 1], prefix[i + 1][j],
                                   prefix[i][j] + (left == right))
assert score[0][0] == expected["lcs_length"] == 1097
assert restricted_length(old, new, expected["gold_range"]) == score[0][0]
possible = []
for j in range(*expected["gold_range"]):
    owners = [i for i, value in enumerate(old)
              if value == new[j] and prefix[i][j] + 1 + score[i + 1][j + 1] == score[0][0]]
    if owners:
        possible.append(dict(new_scalar=j, value=new[j], old_scalars=owners))
assert possible == expected["gold_scalars_with_equal_optimal_paths"]
assert len(possible) == 33
assert any(not item["value"].isspace() for item in possible)
print("Verified: two optimal paragraph boundaries; introduction permits both full-note insertion and equal-token alternatives within the note.")
