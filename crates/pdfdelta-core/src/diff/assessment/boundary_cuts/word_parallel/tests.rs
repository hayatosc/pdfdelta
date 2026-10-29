use std::{cell::Cell, cmp::Ordering, ops::Range};

use serde::Deserialize;

use super::{Outcome, certify, certify_planned, plan, prepare, score, selected_plan};

fn check_matrix(masks: &[u64], m: usize, n: usize, avoid: Option<[usize; 2]>) {
    let words = n.div_ceil(64);
    let rows = (0..m).map(|i| i * words).collect::<Vec<_>>();
    let mut state = vec![0; words];
    let mut previous = vec![0usize; n + 1];
    for i in 0..m {
        let mut next = vec![0; n + 1];
        for j in 0..n {
            next[j + 1] = previous[j + 1].max(next[j]);
            if masks[i * words + j / 64] & (1u64 << (j % 64)) != 0
                && avoid.is_none_or(|point| point != [i, j] && point != [i + 1, j + 1])
            {
                next[j + 1] = next[j + 1].max(previous[j] + 1);
            }
        }
        let value = score(masks, &rows[..=i], avoid, &mut state);
        assert_eq!(value, next[n]);
        let mut prefix = 0;
        for j in 0..words * 64 {
            prefix += ((state[j / 64] >> (j % 64)) & 1) as usize;
            assert_eq!(prefix, next[(j + 1).min(n)], "row {i} column {j}");
        }
        previous = next;
    }
    assert_eq!(score(masks, &rows, avoid, &mut state), previous[n]);
}

#[test]
fn every_small_match_matrix_preserves_every_prefix_score() {
    let mut cases = 0;
    for m in 0..=4 {
        for n in 0..=4 {
            for bits in 0..1u64 << (m * n) {
                let masks = if n == 0 {
                    Vec::new()
                } else {
                    (0..m).map(|i| (bits >> (i * n)) & ((1 << n) - 1)).collect()
                };
                check_matrix(&masks, m, n, None);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 74_963);
}

#[test]
fn carry_borrow_padding_and_forbidden_edges_across_words() {
    let mut seed = 0xC47u64;
    let mut matrices = 0;
    let mut blocked = 0;
    for n in [0usize, 1, 2, 63, 64, 65, 127, 128, 129, 191, 192, 193] {
        let words = n.div_ceil(64);
        for m in [0usize, 1, 2, 3, 8, 65, 129] {
            for pattern in 0..9 {
                let mut masks = vec![0u64; m * words];
                for i in 0..m {
                    for j in 0..n {
                        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                        let allowed = match pattern {
                            0 => true,
                            1 => false,
                            2 => j + 1 == n,
                            3 => j + i + 1 == n,
                            4 => j == i,
                            5 => (i + j) % 2 == 0,
                            6 => seed >> 61 == 0,
                            7 => seed >> 63 == 0,
                            _ => seed >> 61 != 0,
                        };
                        if allowed {
                            masks[i * words + j / 64] |= 1u64 << (j % 64);
                        }
                    }
                }
                check_matrix(&masks, m, n, None);
                matrices += 1;
                let mut cuts = vec![
                    [0, 0],
                    [m, n],
                    [m / 2, n / 2],
                    [m.min(1), n.min(63)],
                    [m.min(65), n.min(64)],
                    [m.min(64), n.min(65)],
                    [m, n.min(128)],
                ];
                cuts.sort_unstable();
                cuts.dedup();
                for cut in cuts {
                    check_matrix(&masks, m, n, Some(cut));
                    blocked += 1;
                }
            }
        }
    }
    assert_eq!(matrices, 756);
    assert_eq!(blocked, 4059);
}

fn forced_word<T: Ord>(old: &[T], new: &[T], target: &[Range<usize>; 2]) -> Outcome {
    certify_planned(
        old,
        new,
        target,
        &mut 32_000_000,
        plan(old.len(), new.len(), 3).expect("plan"),
    )
    .expect("word predicate")
}

#[test]
fn independent_all_optimal_matching_oracle_also_exercises_word_path() {
    #[derive(Deserialize)]
    struct Pair {
        old: String,
        new: String,
        outcomes: String,
    }
    #[derive(Deserialize)]
    struct Oracle {
        pairs: Vec<Pair>,
        range_cases: usize,
    }
    let oracle: Oracle = serde_json::from_str(include_str!("../oracle.json")).expect("oracle");
    let ranges = |n| (0..=n).flat_map(move |start| (start..=n).map(move |end| start..end));
    let mut cases = 0;
    let mut accepted = 0;
    for pair in oracle.pairs {
        let tokens = |text: &str| {
            text.chars()
                .map(|x| if x == 'a' { '雪' } else { ' ' })
                .collect::<Vec<_>>()
        };
        let old = tokens(&pair.old);
        let new = tokens(&pair.new);
        let mut expected = pair.outcomes.bytes();
        for a in ranges(old.len()) {
            for b in ranges(new.len()) {
                let present = (expected.next().expect("range") - b'0') & 1 != 0;
                let outcome = forced_word(&old, &new, &[a.clone(), b]);
                assert_ne!(outcome, Outcome::BudgetExceeded);
                assert_eq!(outcome == Outcome::Proven, present);
                cases += 1;
                accepted += usize::from(present);
            }
        }
        assert_eq!(expected.next(), None);
    }
    assert_eq!(cases, oracle.range_cases);
    assert_eq!(cases, 123_201);
    assert_eq!(accepted, 1870);
}

#[test]
fn real_text_mask_lookup_and_boundary_predicates_match_scalar() {
    #[derive(Deserialize)]
    struct Real {
        case: String,
        id: usize,
        old: String,
        new: String,
        ranges: [[usize; 2]; 2],
        expected: bool,
    }
    let real: Vec<Real> = serde_json::from_str(include_str!("../real.json")).expect("real");
    assert_eq!(real.len(), 16);
    let mut selected = 0;
    for input in real {
        let old = input.old.chars().collect::<Vec<_>>();
        let new = input.new.chars().collect::<Vec<_>>();
        let target = input.ranges.map(|[a, b]| a..b);
        let mut before = 32_000_000;
        let mut after = before;
        let scalar = super::super::certify(&old, &new, &target, &mut before).expect("scalar");
        let word = certify(&old, &new, &target, &mut after).expect("selected");
        assert_eq!(word, scalar);
        assert_eq!(word == Outcome::Proven, input.expected);
        assert_eq!(forced_word(&old, &new, &target), scalar);
        assert!(after >= before);
        selected += usize::from(after > before);
        eprintln!(
            "word_boundary_cost {} id={} scalar={} word={}",
            input.case,
            input.id,
            32_000_000 - before,
            32_000_000 - after
        );
    }
    assert!(selected > 0);
}

#[test]
fn unicode_large_alphabets_and_absent_symbols_match_independent_scalar() {
    for n in [0usize, 1, 63, 64, 65, 127, 128, 129, 193] {
        for alphabet in [
            vec![' ', '雪', 'a'],
            vec!['é', 'e', '\u{301}', '𐀀'],
            (0..200)
                .map(|i| char::from_u32(0x4000 + i).expect("char"))
                .collect(),
        ] {
            let old = (0..=n)
                .map(|i| {
                    if i % 11 == 0 {
                        '🦀'
                    } else {
                        alphabet[(i * 13 + 7) % alphabet.len()]
                    }
                })
                .collect::<Vec<_>>();
            let new = (0..n)
                .map(|i| alphabet[(i * 17 + 5) % alphabet.len()])
                .collect::<Vec<_>>();
            let words = n.div_ceil(64);
            let (masks, rows) = prepare(&old, &new, words).expect("masks");
            let mut state = vec![0; words];
            for avoid in [
                None,
                Some([0, 0]),
                Some([old.len(), new.len()]),
                Some([old.len() / 2, new.len() / 2]),
            ] {
                let mut scalar_buffer = vec![0; 2 * (n + 1)];
                assert_eq!(
                    score(&masks, &rows, avoid, &mut state),
                    super::super::score(&old, &new, avoid, &mut scalar_buffer)
                );
            }
        }
    }
}

#[test]
fn every_prepaid_budget_cut_refuses_before_mask_comparisons() {
    struct Counted<'a>(u8, &'a Cell<usize>);
    impl PartialEq for Counted<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.1.set(self.1.get() + 1);
            self.0 == other.0
        }
    }
    impl Eq for Counted<'_> {}
    impl PartialOrd for Counted<'_> {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Counted<'_> {
        fn cmp(&self, other: &Self) -> Ordering {
            self.1.set(self.1.get() + 1);
            self.0.cmp(&other.0)
        }
    }
    let calls = Cell::new(0);
    let old = (0..256).map(|_| Counted(b'a', &calls)).collect::<Vec<_>>();
    let new = (0..257).map(|_| Counted(b'a', &calls)).collect::<Vec<_>>();
    let target = [0..1, 0..2];
    let expected = selected_plan(256, 257, 1, 64 * 1024 * 1024).expect("word selected");
    for allowance in 0..expected.work {
        let mut remaining = allowance;
        assert_eq!(
            certify(&old, &new, &target, &mut remaining).expect("budget"),
            Outcome::BudgetExceeded
        );
        assert_eq!(remaining, 0);
        assert_eq!(calls.get(), 0);
    }
    let mut remaining = expected.work;
    assert_eq!(
        certify(&old, &new, &target, &mut remaining).expect("complete"),
        Outcome::Rejected
    );
    assert_eq!(remaining, 0);
    assert!(calls.get() > 0);
    assert!(selected_plan(256, 257, 1, expected.bytes - 1).is_none());
    assert!(plan(usize::MAX, 100, 3).is_none());
    assert!(plan(100, usize::MAX, 3).is_none());
}

#[test]
fn fallback_empty_identity_and_invalid_ranges_preserve_contracts() {
    for (old, new, target) in [
        (b"".as_slice(), b"".as_slice(), [0..0, 0..0]),
        (b"".as_slice(), b"aaa".as_slice(), [0..0, 0..3]),
        (b"abc".as_slice(), b"abc".as_slice(), [0..1, 0..2]),
        (b"a".as_slice(), b"aa".as_slice(), [0..1, 0..2]),
    ] {
        let mut a = 1000;
        let mut b = a;
        assert_eq!(
            certify(old, new, &target, &mut a).expect("selected"),
            super::super::certify(old, new, &target, &mut b).expect("scalar")
        );
        assert_eq!(a, b);
    }
    let old = vec!['a'; 512];
    let new = vec!['b'; 513];
    for target in [
        [0..600, 0..1],
        [0..1, 0..600],
        [Range { start: 3, end: 2 }, 0..1],
    ] {
        let mut remaining = 1_000_000;
        assert!(certify(&old, &new, &target, &mut remaining).is_err());
        assert_eq!(remaining, 1_000_000);
    }
    assert_eq!(forced_word(&old, &old, &[0..1, 0..2]), Outcome::Rejected);
}
