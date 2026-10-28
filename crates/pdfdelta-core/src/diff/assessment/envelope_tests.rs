use std::collections::BTreeSet;

use super::{
    EnvelopeUnavailable, LocalTextEnvelope, LocalTextEnvelopeLimits, LocalTextEnvelopeResult,
    LocalTextSide, local_text_claims, local_text_envelope,
};
use crate::normalize::ComparableToken;

type Matching = Vec<[usize; 2]>;

// Independent small oracle: enumerate monotone equal-pair subsequences and
// then select maximum length. This does not use prefix/suffix recurrences.
fn oracle(a: &[ComparableToken], b: &[ComparableToken]) -> BTreeSet<Matching> {
    fn visit(
        a: &[ComparableToken],
        b: &[ComparableToken],
        i: usize,
        j: usize,
        current: &mut Matching,
        output: &mut BTreeSet<Matching>,
    ) {
        output.insert(current.clone());
        for x in i..a.len() {
            for y in j..b.len() {
                if a[x] == b[y] {
                    current.push([x, y]);
                    visit(a, b, x + 1, y + 1, current, output);
                    current.pop();
                }
            }
        }
    }
    let mut output = BTreeSet::new();
    visit(a, b, 0, 0, &mut Vec::new(), &mut output);
    let length = output
        .iter()
        .map(Vec::len)
        .max()
        .expect("empty matching exists");
    output.retain(|path| path.len() == length);
    output
}

fn graph_paths(envelope: &LocalTextEnvelope) -> BTreeSet<Matching> {
    fn visit(
        e: &LocalTextEnvelope,
        i: usize,
        j: usize,
        path: &mut Matching,
        output: &mut BTreeSet<Matching>,
    ) {
        if i == e.dag.old_length && j == e.dag.new_length {
            output.insert(path.clone());
            return;
        }
        let row = &e.dag.rows[i];
        let edge = row.edges[j - row.new_start];
        if edge & 1 != 0 {
            path.push([i, j]);
            visit(e, i + 1, j + 1, path, output);
            path.pop();
        }
        if edge & 2 != 0 {
            visit(e, i + 1, j, path, output);
        }
        if edge & 4 != 0 {
            visit(e, i, j + 1, path, output);
        }
    }
    let mut output = BTreeSet::new();
    visit(envelope, 0, 0, &mut Vec::new(), &mut output);
    output
}

fn envelope(a: &str, b: &str) -> Box<LocalTextEnvelope> {
    let a: Vec<_> = a.chars().map(ComparableToken::Scalar).collect();
    let b: Vec<_> = b.chars().map(ComparableToken::Scalar).collect();
    let outcome = local_text_envelope(
        LocalTextSide {
            tokens: &a,
            source: &vec![true; a.len()],
            optional: &vec![false; a.len()],
        },
        LocalTextSide {
            tokens: &b,
            source: &vec![true; b.len()],
            optional: &vec![false; b.len()],
        },
        &mut 1_000_000,
        LocalTextEnvelopeLimits::default(),
    )
    .expect("bounded envelope");
    let LocalTextEnvelopeResult::Complete { envelope } = outcome else {
        panic!("expected complete local model")
    };
    envelope
}

#[test]
fn envelope_matches_all_961_binary_oracles_and_prior_claims() {
    let words: Vec<Vec<_>> = super::all_words(4)
        .iter()
        .map(|word| {
            word.iter()
                .map(|x| ComparableToken::Scalar(if *x == 0 { 'A' } else { 'B' }))
                .collect()
        })
        .collect();
    let mut checked = 0;
    for a in &words {
        for b in &words {
            let os: Vec<_> = (0..a.len()).map(|i| i % 2 == 0).collect();
            let ns: Vec<_> = (0..b.len()).map(|i| i % 2 != 0).collect();
            let oo = vec![false; a.len()];
            let no = vec![false; b.len()];
            let side_a = || LocalTextSide {
                tokens: a,
                source: &os,
                optional: &oo,
            };
            let side_b = || LocalTextSide {
                tokens: b,
                source: &ns,
                optional: &no,
            };
            let prior = local_text_claims(side_a(), side_b(), &mut 1_000_000)
                .expect("prior kernel")
                .expect("prior complete");
            let LocalTextEnvelopeResult::Complete { envelope: got } = local_text_envelope(
                side_a(),
                side_b(),
                &mut 1_000_000,
                LocalTextEnvelopeLimits::default(),
            )
            .expect("envelope") else {
                panic!("bounded test input")
            };
            assert_eq!(got.claims, prior);
            let paths = oracle(a, b);
            assert_eq!(graph_paths(&got), paths, "{a:?} -> {b:?}");
            let costs: Vec<_> = paths
                .iter()
                .map(|p| {
                    os.iter()
                        .enumerate()
                        .filter(|(i, s)| **s && !p.iter().any(|pair| pair[0] == *i))
                        .count()
                        + ns.iter()
                            .enumerate()
                            .filter(|(j, s)| **s && !p.iter().any(|pair| pair[1] == *j))
                            .count()
                })
                .collect();
            assert_eq!(
                got.claims.changed_source_lower,
                *costs.iter().min().expect("path")
            );
            assert_eq!(
                got.claims.changed_source_upper,
                *costs.iter().max().expect("path")
            );
            for (index, side) in [&got.old, &got.new].iter().enumerate() {
                for (position, alternatives) in side.iter().enumerate() {
                    let expected: BTreeSet<_> = paths
                        .iter()
                        .flatten()
                        .filter(|p| p[index] == position)
                        .map(|p| p[1 - index])
                        .collect();
                    assert_eq!(
                        alternatives.matches,
                        expected.into_iter().collect::<Vec<_>>()
                    );
                    assert_eq!(
                        alternatives.can_be_unmatched,
                        paths
                            .iter()
                            .any(|p| p.iter().all(|pair| pair[index] != position))
                    );
                }
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 961);
}

#[test]
fn envelope_field14_retains_four_joint_space_choices() {
    let got = envelope("14 Excess golden parachute payments", "14 ");
    assert_eq!(got.claims.changed_source_lower, 32);
    assert_eq!(got.claims.changed_source_upper, 32);
    assert_eq!(got.claims.mandatory_old.iter().filter(|v| **v).count(), 29);
    assert_eq!(got.ambiguous_old, [2, 9, 16, 26]);
    assert_eq!(got.ambiguous_new, [2]);
    assert_eq!(got.new[2].matches, [2, 9, 16, 26]);
    assert!(!got.new[2].can_be_unmatched);
    assert_eq!(got.fixed_equal_pairs, [[0, 0], [1, 1]]);
    assert_eq!(graph_paths(&got).len(), 4);
}

#[test]
fn envelope_crossing_choices_cannot_be_combined() {
    let got = envelope("AB", "BA");
    assert_eq!(
        graph_paths(&got),
        BTreeSet::from([vec![[0, 1]], vec![[1, 0]]])
    );
    assert!(got.fixed_equal_pairs.is_empty());
}

#[test]
fn envelope_equal_fast_path_is_linear_and_empty_inputs_are_explicit() {
    let got = envelope(&"A".repeat(1000), &"A".repeat(1000));
    assert_eq!(
        got.dag.rows.iter().map(|r| r.edges.len()).sum::<usize>(),
        1001
    );
    assert_eq!(got.fixed_equal_pairs.len(), 1000);
    assert_eq!(graph_paths(&envelope("", "")), BTreeSet::from([vec![]]));
    assert_eq!(envelope("", "AB").claims.mandatory_new, [true, true]);
    assert_eq!(envelope("AB", "").claims.mandatory_old, [true, true]);
}

#[test]
fn envelope_repeated_tokens_do_not_enumerate_exponentially_many_paths() {
    let got = envelope(&"A".repeat(100), &"A".repeat(50));
    assert_eq!(got.claims.changed_source_lower, 50);
    assert_eq!(got.ambiguous_old.len(), 100);
    assert_eq!(got.ambiguous_new.len(), 50);
    assert_eq!(got.new[25].matches.len(), 51);
}

#[test]
fn envelope_narrow_band_retains_every_tied_deletion_at_both_boundaries() {
    let got = envelope(&"A".repeat(100), &"A".repeat(99));
    assert!(
        got.dag
            .rows
            .iter()
            .map(|row| row.edges.len())
            .sum::<usize>()
            < 300
    );
    let expected = (0..100)
        .map(|deleted| {
            (0..100)
                .filter(|i| *i != deleted)
                .enumerate()
                .map(|(j, i)| [i, j])
                .collect()
        })
        .collect::<BTreeSet<Matching>>();
    assert_eq!(graph_paths(&got), expected);
    assert_eq!(got.ambiguous_old.len(), 100);
    assert_eq!(got.ambiguous_new.len(), 99);
    assert!(got.claims.mandatory_old.iter().all(|value| !value));
}

#[test]
fn envelope_refuses_budget_cells_and_normalization_without_partial_claims() {
    let a = [ComparableToken::Scalar('A')];
    let side = || LocalTextSide {
        tokens: &a,
        source: &[true],
        optional: &[false],
    };
    assert_eq!(
        local_text_envelope(side(), side(), &mut 0, LocalTextEnvelopeLimits::default())
            .expect("refusal"),
        LocalTextEnvelopeResult::Unavailable {
            reason: EnvelopeUnavailable::WorkLimit
        }
    );
    assert_eq!(
        local_text_envelope(
            side(),
            side(),
            &mut 100,
            LocalTextEnvelopeLimits { max_cells: 1 }
        )
        .expect("refusal"),
        LocalTextEnvelopeResult::Unavailable {
            reason: EnvelopeUnavailable::CellLimit
        }
    );
    assert_eq!(
        local_text_envelope(
            LocalTextSide {
                tokens: &a,
                source: &[true],
                optional: &[true]
            },
            side(),
            &mut 100,
            LocalTextEnvelopeLimits::default()
        )
        .expect("refusal"),
        LocalTextEnvelopeResult::Unavailable {
            reason: EnvelopeUnavailable::NormalizationAlternatives
        }
    );
    assert!(
        local_text_envelope(
            LocalTextSide {
                tokens: &a,
                source: &[],
                optional: &[false]
            },
            side(),
            &mut 100,
            LocalTextEnvelopeLimits::default()
        )
        .is_err()
    );
}

#[test]
fn envelope_document_adapter_keeps_correspondence_conditional() {
    use crate::document::{
        InterpretationStatus, SourceRef, TextNormalization, TextView, explain_local_text_views,
    };
    let old = TextView {
        tokens: vec![ComparableToken::Scalar('A')],
        origins: vec![vec![SourceRef::Native {
            glyph: crate::model::GlyphId(1),
        }]],
        source_backed: vec![true],
        normalization: TextNormalization::Exact,
    };
    let mut new = old.clone();
    new.tokens[0] = ComparableToken::Scalar('B');
    let explanation =
        explain_local_text_views(&old, &new, &mut 10000, LocalTextEnvelopeLimits::default())
            .expect("local explanation");
    assert_eq!(
        explanation.interpretation,
        InterpretationStatus::ConditionalOnCorrespondence
    );
    let LocalTextEnvelopeResult::Complete { envelope } = explanation.result else {
        panic!("local literal model")
    };
    assert_eq!(envelope.claims.changed_source_lower, 2);
    assert_eq!(envelope.claims.mandatory_old, [true]);
    let refused = explain_local_text_views(
        &old,
        &new,
        &mut 10000,
        LocalTextEnvelopeLimits { max_cells: 1 },
    )
    .expect("view cap refusal");
    assert_eq!(
        refused.result,
        LocalTextEnvelopeResult::Unavailable {
            reason: EnvelopeUnavailable::CellLimit,
        }
    );
    new.origins.clear();
    assert!(
        explain_local_text_views(&old, &new, &mut 10000, LocalTextEnvelopeLimits::default())
            .is_err()
    );
}
