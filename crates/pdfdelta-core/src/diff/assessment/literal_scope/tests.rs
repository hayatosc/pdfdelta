use std::{cell::Cell, ops::Range};

use serde::Deserialize;

use super::{LocalOutcome, unique_changed};
use crate::diff::assessment::boundary_cuts;

#[derive(Deserialize)]
struct Pair {
    old: String,
    new: String,
    outcomes: String,
}

#[derive(Deserialize)]
struct Oracle {
    binary: Vec<Pair>,
    space_unicode: Vec<Pair>,
}

fn ranges(n: usize) -> impl Iterator<Item = Range<usize>> {
    (0..=n).flat_map(move |a| (a..=n).map(move |b| a..b))
}

#[test]
fn factorization_matches_all_global_matching_signatures() {
    let oracle: Oracle = serde_json::from_str(include_str!("oracle.json")).expect("oracle");
    for (pairs, expected_count, expected_positive) in [
        (oracle.binary, 123_201, 992),
        (oracle.space_unicode, 4096, 204),
    ] {
        let mut count = 0;
        let mut positive = 0;
        for p in pairs {
            let old: Vec<_> = p.old.chars().collect();
            let new: Vec<_> = p.new.chars().collect();
            let mut expected = p.outcomes.bytes();
            for a in ranges(old.len()) {
                for b in ranges(new.len()) {
                    let mut remaining = 100_000;
                    let local = unique_changed(&old[a.clone()], &new[b.clone()], &mut remaining)
                        .expect("local proof");
                    assert_ne!(local, LocalOutcome::BudgetExceeded);
                    let accepted = local == LocalOutcome::UniqueChanged
                        && boundary_cuts::certify(&old, &new, &[a.clone(), b], &mut remaining)
                            .expect("cuts")
                            == boundary_cuts::Outcome::Proven;
                    assert_eq!(accepted, expected.next().expect("range") == b'1');
                    count += 1;
                    positive += usize::from(accepted);
                }
            }
            assert_eq!(expected.next(), None);
        }
        assert_eq!(count, expected_count);
        assert_eq!(positive, expected_positive);
    }
}

#[derive(Deserialize)]
struct RealLocal {
    case: String,
    id: usize,
    old: String,
    new: String,
    local_unique: bool,
    cut_verified_c39: bool,
    factorized_strict: bool,
}

#[test]
fn captured_local_cases_keep_positional_ambiguity() {
    let inputs: Vec<RealLocal> =
        serde_json::from_str(include_str!("real-local.json")).expect("real local inputs");
    let mut positives = 0;
    for input in inputs {
        let old: Vec<_> = input.old.chars().collect();
        let new: Vec<_> = input.new.chars().collect();
        let mut work = 32_000_000;
        let local = unique_changed(&old, &new, &mut work).expect("real local proof");
        assert_ne!(local, LocalOutcome::BudgetExceeded);
        assert_eq!(
            local == LocalOutcome::UniqueChanged,
            input.local_unique && old != new
        );
        assert_eq!(
            local == LocalOutcome::UniqueChanged && input.cut_verified_c39,
            input.factorized_strict
        );
        positives += usize::from(input.factorized_strict);
        if input.case == "ecb-annual-2023-to-2024" && input.id == 3
            || input.case == "nist-risk-management-37-r1-to-r2"
        {
            eprintln!(
                "local literal {}:{} work={}",
                input.case,
                input.id,
                32_000_000 - work
            );
        }
    }
    assert_eq!(positives, 4);
}

#[test]
fn empty_equal_repeated_and_every_budget_cut_preserve_refusals() {
    assert_eq!(
        unique_changed::<u8>(&[], &[], &mut 0).expect("empty"),
        LocalOutcome::Unavailable
    );
    assert_eq!(
        unique_changed(b"", b"x", &mut 0).expect("one side"),
        LocalOutcome::UniqueChanged
    );
    assert_eq!(
        unique_changed(b"aa", b"a", &mut 1000).expect("repeat"),
        LocalOutcome::Unavailable
    );
    assert_eq!(
        unique_changed(b"a a", b"a  a", &mut 1000).expect("spaces"),
        LocalOutcome::Unavailable
    );
    let mut ample = 1000;
    assert_eq!(
        unique_changed(b"ab", b"ac", &mut ample).expect("changed"),
        LocalOutcome::UniqueChanged
    );
    let used = 1000 - ample;
    for initial in 0..=used {
        let mut work = initial;
        let result = unique_changed(b"ab", b"ac", &mut work).expect("bounded");
        assert!(work <= initial);
        assert_eq!(
            result == LocalOutcome::UniqueChanged,
            initial == used,
            "budget={initial}"
        );
    }
}

struct Counted<'a>(u8, &'a Cell<usize>);
impl PartialEq for Counted<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.1.set(self.1.get() + 1);
        self.0 == other.0
    }
}
impl Eq for Counted<'_> {}

#[test]
fn table_admission_refuses_before_any_cell_comparison() {
    let comparisons = Cell::new(0);
    let old = [Counted(b'a', &comparisons), Counted(b'b', &comparisons)];
    let new = [Counted(b'a', &comparisons)];
    assert_eq!(
        unique_changed(&old, &new, &mut 9).expect("refused"),
        LocalOutcome::BudgetExceeded
    );
    assert_eq!(comparisons.get(), 0);
}
