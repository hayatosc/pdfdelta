use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Oracle {
    real: Vec<RealCase>,
    exhaustive: Vec<SmallCase>,
}

#[derive(Deserialize)]
struct RealCase {
    relation: usize,
    old: Vec<ComparableToken>,
    new: Vec<ComparableToken>,
    pairs: Vec<(usize, usize)>,
    dense_work: usize,
    initial_work: usize,
}

#[derive(Deserialize)]
struct SmallCase {
    old: String,
    new: String,
    pairs: Vec<(usize, usize)>,
}

fn oracle() -> Oracle {
    serde_json::from_str(include_str!("oracle.json")).expect("independent oracle fixture")
}

fn tokens(text: &str) -> Vec<ComparableToken> {
    text.chars().map(ComparableToken::Scalar).collect()
}

fn padded(text: &str, collide: bool) -> Vec<ComparableToken> {
    let prefix = (0..32).map(|i| char::from_u32(0x1000 + i).expect("prefix scalar"));
    let middle = text
        .chars()
        .map(|ch| if collide && ch == 'b' { '\u{161}' } else { ch });
    let suffix = (0..32).map(|i| char::from_u32(0x2000 + i).expect("suffix scalar"));
    prefix
        .chain(middle)
        .chain(suffix)
        .map(ComparableToken::Scalar)
        .collect()
}

fn pairs(result: Result<Option<MandatoryMatchAnalysis>>) -> Result<Option<Vec<(usize, usize)>>> {
    result.map(|analysis| analysis.map(|analysis| analysis.pairs().to_vec()))
}

#[test]
fn captured_real_inputs_match_independent_oracle_and_reduce_paid_work() {
    let cases = oracle().real;
    assert_eq!(cases.len(), 14);
    let (mut old_total, mut new_total) = (0, 0);
    for case in cases {
        let old = case.old.iter().collect::<Vec<_>>();
        let new = case.new.iter().collect::<Vec<_>>();
        let mut remaining = case.initial_work;
        let actual = pairs(mandatory_literal_analysis_with_sparse_preference(
            &old,
            &new,
            &mut remaining,
            MAX_SEMANTIC_MEMORY_BYTES,
        ))
        .expect("complete analysis")
        .expect("payable analysis");
        assert_eq!(actual, case.pairs, "relation {}", case.relation);
        assert_eq!(dense_work(old.len(), new.len()), Some(case.dense_work));
        let used = case.initial_work - remaining;
        assert!(used <= case.dense_work / 2, "relation {}", case.relation);
        old_total += case.dense_work;
        new_total += used;
        println!(
            "real relation={} dense={} preferred={used}",
            case.relation, case.dense_work
        );
    }
    assert_eq!(old_total, 3_864_788);
    assert!(new_total <= 763_452);
    println!("real total_dense={old_total} total_preferred={new_total}");
}

#[test]
fn padded_exhaustive_cases_preserve_all_ties_including_bucket_collisions() {
    let cases = oracle().exhaustive;
    assert_eq!(cases.len(), 961);
    for case in cases {
        for collide in [false, true] {
            let old = padded(&case.old, collide);
            let new = padded(&case.new, collide);
            let old = old.iter().collect::<Vec<_>>();
            let new = new.iter().collect::<Vec<_>>();
            let mut expected = (1..=32).map(|i| (i, i)).collect::<Vec<_>>();
            expected.extend(case.pairs.iter().map(|(a, b)| (a + 32, b + 32)));
            expected.extend((1..=32).map(|i| (i + 32 + case.old.len(), i + 32 + case.new.len())));
            let mut remaining = 1_000_000;
            let actual = pairs(mandatory_literal_analysis_with_sparse_preference(
                &old,
                &new,
                &mut remaining,
                MAX_SEMANTIC_MEMORY_BYTES,
            ))
            .expect("bounded")
            .expect("complete");
            assert_eq!(
                actual, expected,
                "old={} new={} collide={collide}",
                case.old, case.new
            );
            // This distinguishes an exercised sparse path from dense parity alone.
            assert!(1_000_000 - remaining < dense_work(old.len(), new.len()).expect("dense") / 2);
        }
    }
}

#[test]
fn small_and_empty_inputs_preserve_original_budget_and_results() {
    for case in oracle().exhaustive {
        let old = tokens(&case.old);
        let new = tokens(&case.new);
        let old = old.iter().collect::<Vec<_>>();
        let new = new.iter().collect::<Vec<_>>();
        let dense = dense_work(old.len(), new.len()).expect("dense");
        for budget in [0, dense - 1, dense, dense + 1, 1_000_000] {
            let (mut original, mut preferred) = (budget, budget);
            let expected = pairs(mandatory_match_analysis_with_memory_limit(
                &old,
                &new,
                &mut original,
                MAX_SEMANTIC_MEMORY_BYTES,
            ))
            .expect("original");
            let actual = pairs(mandatory_literal_analysis_with_sparse_preference(
                &old,
                &new,
                &mut preferred,
                MAX_SEMANTIC_MEMORY_BYTES,
            ))
            .expect("preferred");
            assert_eq!(actual, expected);
            assert_eq!(preferred, original);
            if let Some(actual) = actual {
                assert_eq!(actual, case.pairs);
            }
        }
    }
}

#[test]
fn admission_boundary_keeps_dense_fallback_and_never_overspends() {
    let old = padded("abba", false);
    let new = padded("baab", false);
    let old = old.iter().collect::<Vec<_>>();
    let new = new.iter().collect::<Vec<_>>();
    let dense = dense_work(old.len(), new.len()).expect("dense");
    let admission = dense + dense / 2 + probe_work(old.len(), new.len()).expect("probe");
    // Every stop point through admission, including an unaffordable dense run.
    let expected = pairs(mandatory_match_analysis_with_memory_limit(
        &old,
        &new,
        &mut 1_000_000,
        MAX_SEMANTIC_MEMORY_BYTES,
    ))
    .expect("original");
    for budget in 0..=admission + 1 {
        let mut remaining = budget;
        let actual = pairs(mandatory_literal_analysis_with_sparse_preference(
            &old,
            &new,
            &mut remaining,
            MAX_SEMANTIC_MEMORY_BYTES,
        ))
        .expect("bounded");
        if budget < dense {
            assert!(actual.is_none());
            assert_eq!(remaining, budget);
        } else {
            assert_eq!(actual, expected);
            if budget < admission {
                assert_eq!(remaining, budget - dense);
            } else {
                assert!(budget - remaining < dense / 2);
            }
        }
    }
}

#[test]
fn dense_repetition_and_opaque_tokens_pay_scout_then_use_original() {
    let mut old = vec![ComparableToken::Scalar('a'); 128];
    let mut new = old.clone();
    new[127] = ComparableToken::Scalar('b');
    for opaque in [false, true] {
        if opaque {
            let token = ComparableToken::Unmapped {
                font_hash: crate::model::FontProgramHash(vec![7; 32]),
                glyph_id: 42,
            };
            old[0] = token.clone();
            new[0] = token;
        }
        let old = old.iter().collect::<Vec<_>>();
        let new = new.iter().collect::<Vec<_>>();
        let (mut original, mut preferred) = (1_000_000, 1_000_000);
        let expected = pairs(mandatory_match_analysis_with_memory_limit(
            &old,
            &new,
            &mut original,
            MAX_SEMANTIC_MEMORY_BYTES,
        ))
        .expect("original");
        let actual = pairs(mandatory_literal_analysis_with_sparse_preference(
            &old,
            &new,
            &mut preferred,
            MAX_SEMANTIC_MEMORY_BYTES,
        ))
        .expect("preferred");
        assert_eq!(actual, expected);
        assert_eq!(
            original - preferred,
            probe_work(old.len(), new.len()).expect("probe")
        );
    }
}

#[test]
fn memory_refusal_cannot_grant_partial_analysis() {
    let old = padded("abba", false);
    let new = padded("baab", false);
    let old = old.iter().collect::<Vec<_>>();
    let new = new.iter().collect::<Vec<_>>();
    for memory in [0, 2047, 2048, 4096, 8192] {
        let mut remaining = 1_000_000;
        assert!(
            mandatory_literal_analysis_with_sparse_preference(&old, &new, &mut remaining, memory)
                .is_err()
        );
        assert_eq!(
            1_000_000 - remaining,
            if memory < 2048 {
                0
            } else {
                probe_work(old.len(), new.len()).expect("probe")
            }
        );
    }
    let mut remaining = 1_000_000;
    assert!(
        mandatory_literal_analysis_with_sparse_preference(
            &old,
            &new,
            &mut remaining,
            MAX_SEMANTIC_MEMORY_BYTES
        )
        .expect("bounded")
        .is_some()
    );
}

#[test]
fn arithmetic_overflow_refuses_the_scout() {
    assert_eq!(dense_work(usize::MAX, 1), None);
    assert_eq!(dense_work(1, usize::MAX), None);
    assert_eq!(probe_work(usize::MAX, 0), None);
    assert_eq!(probe_work(0, usize::MAX), None);
}
