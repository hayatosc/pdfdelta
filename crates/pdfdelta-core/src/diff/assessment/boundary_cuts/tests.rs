use std::{cell::Cell, ops::Range};

use serde::Deserialize;

use super::{Outcome, certify, certify_with_memory_limit};
use crate::{Error, diff::AtomicEdit};

#[derive(Deserialize)]
struct Oracle {
    pairs: Vec<Pair>,
    range_cases: usize,
    optimal_matchings: usize,
}

#[derive(Deserialize)]
struct Pair {
    old: String,
    new: String,
    outcomes: String,
}

#[derive(Deserialize)]
struct RealInput {
    case: String,
    id: usize,
    old: String,
    new: String,
    ranges: [[usize; 2]; 2],
    expected: bool,
}

fn ranges(length: usize) -> impl Iterator<Item = Range<usize>> {
    (0..=length).flat_map(move |start| (start..=length).map(move |end| start..end))
}

#[test]
fn independent_all_matching_range_oracle() {
    let oracle: Oracle = serde_json::from_str(include_str!("oracle.json")).expect("oracle");
    assert_eq!(oracle.pairs.len(), 961);
    assert_eq!(oracle.optimal_matchings, 2711);
    let mut count = 0;
    let mut accepted = 0;
    for pair in oracle.pairs {
        let tokens = |text: &str| {
            text.chars()
                .map(|c| if c == 'a' { '雪' } else { ' ' })
                .collect::<Vec<_>>()
        };
        let old = tokens(&pair.old);
        let new = tokens(&pair.new);
        let mut expected = pair.outcomes.bytes();
        for a in ranges(old.len()) {
            for b in ranges(new.len()) {
                let present = (expected.next().expect("range") - b'0') & 1 != 0;
                let outcome =
                    certify(&old, &new, &[a.clone(), b], &mut 100_000).expect("bounded oracle");
                assert_ne!(outcome, Outcome::BudgetExceeded);
                assert_eq!(outcome == Outcome::Proven, present);
                accepted += usize::from(present);
                count += 1;
            }
        }
        assert_eq!(expected.next(), None);
    }
    assert_eq!(count, oracle.range_cases);
    assert_eq!(count, 123_201);
    assert_eq!(accepted, 1870);
}

#[test]
fn saved_real_inputs_and_positive_enumeration_cost() {
    use super::super::{charge, point_on_script, semantic, target_hunk_signature};

    let inputs: Vec<RealInput> =
        serde_json::from_str(include_str!("real.json")).expect("real inputs");
    assert_eq!(inputs.len(), 16);
    let mut accepted = 0;
    let mut measured = 0;
    for input in inputs {
        let old = input.old.chars().collect::<Vec<_>>();
        let new = input.new.chars().collect::<Vec<_>>();
        let target = input.ranges.map(|[a, b]| a..b);
        let mut remaining = 32_000_000;
        let outcome = certify(&old, &new, &target, &mut remaining).expect("real input");
        assert_ne!(outcome, Outcome::BudgetExceeded);
        assert_eq!(outcome == Outcome::Proven, input.expected);
        accepted += usize::from(input.expected);
        let used = 32_000_000 - remaining;
        let problem = (input.case == "ecb-annual-2023-to-2024" && input.id == 3)
            || input.case == "nist-risk-management-37-r1-to-r2";
        if problem {
            assert_eq!(outcome, Outcome::Proven);
            let lengths = [old.len(), new.len()];
            let token_work = old.len() + new.len();
            let mut ordinary_remaining = 32_000_000;
            let ordinary = semantic::check_hunks(
                &old,
                &new,
                &mut ordinary_remaining,
                |edits: &[AtomicEdit], remaining: &mut usize| {
                    if !charge(remaining, token_work + edits.len()) {
                        return Ok(None);
                    }
                    let present =
                        point_on_script([target[0].start, target[1].start], edits, lengths)
                            && point_on_script([target[0].end, target[1].end], edits, lengths)
                            && target_hunk_signature(&target[0], &target[1], edits).is_some();
                    Ok(Some(present.then_some(())))
                },
            )
            .expect("ordinary checker");
            let ordinary_used = 32_000_000 - ordinary_remaining;
            assert!(matches!(
                ordinary,
                semantic::Outcome::BudgetExceeded
                    | semantic::Outcome::Unique {
                        signature: Some(()),
                        ..
                    }
            ));
            assert!(used * 2 < ordinary_used);
            measured += 1;
            eprintln!(
                "boundary_cost {} id={} reserved_work={} ordinary_used={} ordinary_incomplete={}",
                input.case,
                input.id,
                used,
                ordinary_used,
                matches!(ordinary, semantic::Outcome::BudgetExceeded)
            );
        }
    }
    assert_eq!(accepted, 5);
    assert_eq!(measured, 2);
}

#[test]
fn every_budget_cut_and_memory_refusal_precede_dp_comparisons() {
    struct Counted<'a>(u8, &'a Cell<usize>);
    impl PartialEq for Counted<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.1.set(self.1.get() + 1);
            self.0 == other.0
        }
    }
    impl Eq for Counted<'_> {}

    let calls = Cell::new(0);
    let old = [Counted(b'a', &calls), Counted(b'b', &calls)];
    let new = [
        Counted(b'a', &calls),
        Counted(b'a', &calls),
        Counted(b'b', &calls),
    ];
    let target = [0..1, 0..2];
    for allowance in 0..=33 {
        calls.set(0);
        let mut remaining = allowance;
        let actual = certify(&old, &new, &target, &mut remaining).expect("budget");
        if allowance < 32 {
            assert_eq!(actual, Outcome::BudgetExceeded);
            assert_eq!(calls.get(), 0);
            assert_eq!(remaining, 0);
        } else {
            assert_eq!(actual, Outcome::Proven);
            assert_eq!(calls.get(), 12);
            assert_eq!(remaining, allowance - 32);
        }
    }
    calls.set(0);
    let mut remaining = 100;
    assert!(matches!(
        certify_with_memory_limit(&old, &new, &target, &mut remaining, 63),
        Err(Error::LimitExceeded { .. })
    ));
    assert_eq!(remaining, 100);
    assert_eq!(calls.get(), 0);
}

#[test]
fn empty_identity_and_invalid_ranges_cannot_fabricate_edits() {
    assert_eq!(
        certify(b"", b"", &[0..0, 0..0], &mut 0).expect("empty"),
        Outcome::Rejected
    );
    assert_eq!(
        certify(b"", b"aaa", &[0..0, 0..3], &mut 0).expect("one sided"),
        Outcome::Proven
    );
    assert_eq!(
        certify(b"abc", b"abc", &[0..1, 0..2], &mut 100).expect("identity"),
        Outcome::Rejected
    );
    for target in [
        [0..4, 0..1],
        [0..1, 0..4],
        [Range { start: 2, end: 1 }, 0..1],
    ] {
        assert!(certify(b"abc", b"abc", &target, &mut 100).is_err());
    }
}
