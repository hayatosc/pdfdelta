use super::*;

fn view(text: &str, bounded: bool) -> View {
    let tokens = text
        .chars()
        .map(ComparableToken::Scalar)
        .collect::<Vec<_>>();
    let length = tokens.len();
    View {
        kind: ViewKind::Untrusted(0),
        source_order: 0,
        block_indices: vec![0],
        group: GroupText::try_new(vec![BlockId(1)], None, tokens, None, None, None, None)
            .expect("valid test view"),
        source_bounded: bounded,
        order_certified: false,
        horizontal_text: true,
        block_ranges: std::iter::once(0..length).collect(),
        block_candidates: vec![bounded],
        position_signatures: Vec::new(),
        page: None,
        token_positions: vec![None; length],
        token_pages: vec![None; length],
        deny_positions: Vec::new(),
        deny_pages: Vec::new(),
    }
}

type PairMap = BTreeMap<(usize, usize), Vec<AnchorHit>>;

fn endings(pairs: &PairMap) -> Vec<[usize; 6]> {
    pairs
        .values()
        .flatten()
        .filter(|a| a.input_index.query_index().is_none())
        .map(|a| {
            assert!(
                anchors::completed_seed(*a).is_none(),
                "source ends must not become seed witnesses"
            );
            [
                a.old_view,
                a.new_view,
                a.old_start,
                a.old_end,
                a.new_start,
                a.new_end,
            ]
        })
        .collect()
}

#[test]
fn batched_source_end_matches_python_and_scanning_oracles() -> Result<()> {
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/data/source_end_oracle.json"
    ))
    .expect("independent oracle JSON");
    for (case_index, case) in corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .enumerate()
    {
        let make = |side: &str| {
            case[side]
                .as_array()
                .expect("strings")
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    view(
                        s.as_str().expect("text"),
                        case[format!("{side}_bounded")][i]
                            .as_bool()
                            .expect("bounded"),
                    )
                })
                .collect::<Vec<_>>()
        };
        let old = make("old");
        let new = make("new");
        let mut pairs = PairMap::new();
        for p in case["pairs"].as_array().expect("pairs") {
            let old_view = p[0].as_u64().expect("old index") as usize;
            let new_view = p[1].as_u64().expect("new index") as usize;
            let mut hits = Vec::new();
            if let Some(end) = p[2].as_u64() {
                hits.push(AnchorHit {
                    input_index: anchors::AnchorOrigin::explicit(0),
                    old_view,
                    new_view,
                    old_start: 0,
                    new_start: 0,
                    old_end: end as usize,
                    new_end: end as usize,
                });
            }
            pairs.insert((old_view, new_view), hits);
        }
        let expected: Vec<[usize; 6]> =
            serde_json::from_value(case["expected"].clone()).expect("oracle endings");
        let mut scanning = pairs.clone();
        assert!(add_source_end_anchors_scanning(
            [&old, &new],
            &mut scanning,
            &mut 10_000_000
        )?);
        assert_eq!(endings(&scanning), expected, "KMP case {case_index}");
        for width in [0, 1, 2, 4, 8] {
            let mut batch = pairs.clone();
            assert!(add_source_end_anchors(
                [&old, &new],
                &mut batch,
                &mut 10_000_000,
                width
            )?);
            assert_eq!(
                endings(&batch),
                expected,
                "batch case {case_index}, width {width}"
            );
        }
    }
    Ok(())
}

#[test]
fn batched_source_end_budget_cut_never_publishes_a_partial_batch() -> Result<()> {
    let old = [
        view("AA", true),
        view("AB", true),
        view("BA", true),
        view("BB", true),
        view("AAA", false),
    ];
    let new = [
        view("AA", true),
        view("AB", true),
        view("BA", true),
        view("BB", true),
        view("AAA", false),
    ];
    let pairs = (0..4).map(|i| ((i, i), Vec::new())).collect::<PairMap>();
    let mut full = pairs.clone();
    let mut work = 100_000;
    assert!(add_source_end_anchors(
        [&old, &new],
        &mut full,
        &mut work,
        2
    )?);
    assert!(
        endings(&full).iter().all(|a| a[0] != 0),
        "overlapping competitor vetoes AA"
    );
    let used = 100_000 - work;
    for budget in 0..=used {
        let mut actual = pairs.clone();
        let mut remaining = budget;
        let complete = add_source_end_anchors([&old, &new], &mut actual, &mut remaining, 2)?;
        if complete {
            assert_eq!(endings(&actual), endings(&full));
        } else {
            assert!(
                endings(&actual).is_empty(),
                "cut {budget} published anchors"
            );
        }
    }
    Ok(())
}

#[test]
fn batched_source_end_removes_repeated_full_text_searches() -> Result<()> {
    let texts = (0..100)
        .map(|i| format!("Unique complete source line {i:04} Ω"))
        .collect::<Vec<_>>();
    let old = texts.iter().map(|s| view(s, true)).collect::<Vec<_>>();
    let new = texts.iter().map(|s| view(s, true)).collect::<Vec<_>>();
    let pairs = (0..100).map(|i| ((i, i), Vec::new())).collect::<PairMap>();
    let mut before = pairs.clone();
    let mut before_work = 10_000_000;
    assert!(add_source_end_anchors_scanning(
        [&old, &new],
        &mut before,
        &mut before_work
    )?);
    let mut after = pairs;
    let mut after_work = 10_000_000;
    assert!(add_source_end_anchors(
        [&old, &new],
        &mut after,
        &mut after_work,
        8
    )?);
    assert_eq!(endings(&before), endings(&after));
    assert!(
        after_work > before_work,
        "shared scan must reduce real comparisons for unique long endings"
    );
    Ok(())
}

#[test]
fn source_end_repeated_long_queries_keep_streaming_fallback() -> Result<()> {
    // Repeated long endings need little live KMP memory, although summing the
    // borrowed token lengths exceeds the batch's retained-payload allowance.
    let text = "A".repeat(1_000);
    let old = [view(&text, true)];
    let new = (0..3_000).map(|_| view(&text, true)).collect::<Vec<_>>();
    let pairs = (0..new.len())
        .map(|i| ((0, i), Vec::new()))
        .collect::<PairMap>();
    let mut before = pairs.clone();
    let mut before_work = 32_000_000;
    assert!(add_source_end_anchors_scanning(
        [&old, &new],
        &mut before,
        &mut before_work
    )?);
    let mut after = pairs;
    let mut after_work = 32_000_000;
    let complete = add_source_end_anchors([&old, &new], &mut after, &mut after_work, 8)?;
    println!(
        "source_end memory counterexample: old_work={} new_work={} complete={complete}",
        32_000_000 - before_work,
        32_000_000 - after_work
    );
    assert!(
        complete,
        "bounded batch planning must retain a viable streaming route"
    );
    assert_eq!(endings(&after), endings(&before));
    Ok(())
}

#[test]
fn sparse_source_end_queries_keep_streaming_search_cost() -> Result<()> {
    // Most proposed pairs cannot supply a source-bounded ending. They must
    // remain searched competitors without forcing a multi-pattern full scan.
    let mut old = (0..2)
        .map(|i| view(&format!("Complete unique source ending {i:04} Ω"), true))
        .collect::<Vec<_>>();
    let background = "untrusted background competitor ".repeat(20);
    old.extend((0..150).map(|_| view(&background, false)));
    let mut new = (0..2)
        .map(|i| view(&format!("Complete unique source ending {i:04} Ω"), true))
        .collect::<Vec<_>>();
    new.extend((0..150).map(|_| view(&background, false)));
    let pairs = (0..old.len())
        .map(|i| ((i, i), Vec::new()))
        .collect::<PairMap>();
    let mut reference = pairs.clone();
    let mut work = 10_000_000;
    assert!(add_source_end_anchors_scanning(
        [&old, &new],
        &mut reference,
        &mut work
    )?);
    let reference_cost = 10_000_000 - work;
    let budget = reference_cost + reference_cost / 100;
    let mut remaining = budget;
    let mut actual = pairs;
    let complete = add_source_end_anchors([&old, &new], &mut actual, &mut remaining, 16)?;
    println!(
        "sparse endings: old_work={reference_cost} candidate_work={} budget={budget} complete={complete}",
        budget - remaining
    );
    assert!(
        complete,
        "sparse queries must not pay for a full rolling scan"
    );
    assert_eq!(endings(&reference), endings(&actual));
    Ok(())
}

#[test]
fn short_queries_do_not_force_a_sparse_long_query_into_a_rolling_scan() -> Result<()> {
    let make = || {
        let mut views = (0..4).map(|_| view(" ", true)).collect::<Vec<_>>();
        views.push(view("Complete unique source ending Ω", true));
        let background = "untrusted background competitor ".repeat(40);
        views.extend((0..147).map(|_| view(&background, false)));
        views
    };
    let old = make();
    let new = make();
    let pairs = (0..old.len())
        .map(|i| ((i, i), Vec::new()))
        .collect::<PairMap>();
    let mut reference = pairs.clone();
    let mut work = 10_000_000;
    assert!(add_source_end_anchors_scanning(
        [&old, &new],
        &mut reference,
        &mut work
    )?);
    let reference_cost = 10_000_000 - work;
    let budget = reference_cost + reference_cost / 100;
    let mut remaining = budget;
    let mut actual = pairs;
    let complete = add_source_end_anchors([&old, &new], &mut actual, &mut remaining, 16)?;
    println!(
        "mixed endings: old_work={reference_cost} candidate_work={} budget={budget} complete={complete}",
        budget - remaining
    );
    assert!(
        complete,
        "short queries cannot amortize a long-query rolling scan"
    );
    assert_eq!(endings(&reference), endings(&actual));
    Ok(())
}

#[test]
fn repeated_long_endings_stop_before_unrelated_competing_views() -> Result<()> {
    let texts = (0..4)
        .map(|i| format!("Repeated complete source line {i:04} Ω"))
        .collect::<Vec<_>>();
    let mut old = Vec::new();
    let mut new = Vec::new();
    for text in &texts {
        for _ in 0..2 {
            old.push(view(text, true));
            new.push(view(text, true));
        }
    }
    for _ in 0..100 {
        old.push(view(&"unrelated ".repeat(100), false));
        new.push(view(&"unrelated ".repeat(100), false));
    }
    let pairs = (0..4)
        .map(|i| ((2 * i, 2 * i), Vec::new()))
        .collect::<PairMap>();
    let mut reference = pairs.clone();
    let mut old_remaining = 100_000;
    assert!(add_source_end_anchors_scanning(
        [&old, &new],
        &mut reference,
        &mut old_remaining
    )?);
    assert!(endings(&reference).is_empty());
    let mut actual = pairs;
    let mut remaining = 100_000;
    let complete = add_source_end_anchors([&old, &new], &mut actual, &mut remaining, 16)?;
    eprintln!(
        "repeated long endings: old_work={} candidate_work={} complete={}",
        100_000 - old_remaining,
        100_000 - remaining,
        complete
    );
    assert!(
        complete,
        "all long queries already had two exact occurrences near the start"
    );
    assert_eq!(endings(&actual), endings(&reference));
    Ok(())
}

#[test]
fn saturated_long_search_keeps_late_unique_queries_and_first_occurrences() -> Result<()> {
    let text = [
        "shared-prefix-alpha",
        "shared-prefix-beta",
        "shared-prefix-gamma",
        "shared-prefix-delta",
    ];
    let mut old = Vec::new();
    let mut new = Vec::new();
    for s in &text[..3] {
        for _ in 0..2 {
            old.push(view(s, true));
            new.push(view(s, true));
        }
    }
    old.push(view(text[3], true));
    new.push(view(text[3], true));
    old.push(view("unrelated competing content", false));
    new.push(view("unrelated competing content", false));
    new.push(view(text[3], false));
    let owned = text
        .iter()
        .map(|s| s.chars().map(ComparableToken::Scalar).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let needles = owned
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.as_slice()))
        .collect::<Vec<_>>();
    let result = anchors::search_explicit_anchors_with_batch_minimum(
        [&old, &new],
        &needles,
        4,
        &mut 100_000,
        EXPLICIT_ANCHOR_RETAINED_LIMIT,
        4,
    )?
    .expect("complete capped search");
    for (i, a, b) in result {
        for (views, actual) in [(&old, a), (&new, b)] {
            let SearchResult::Complete(expected) = search_views(views, &owned[i], &mut 100_000)?
            else {
                panic!("oracle search complete")
            };
            assert_eq!(actual.count, expected.count);
            assert_eq!(
                actual.first.map(|p| (p.view_index, p.start, p.end)),
                expected.first.map(|p| (p.view_index, p.start, p.end))
            );
        }
    }
    Ok(())
}

#[test]
fn saturated_long_search_budget_cut_never_claims_one_hit_is_complete() -> Result<()> {
    let texts = [
        "alpha repeat",
        "beta repeat",
        "gamma repeat",
        "delta repeat",
    ];
    let views = texts
        .iter()
        .flat_map(|s| [view(s, true), view(s, false)])
        .collect::<Vec<_>>();
    let owned = texts
        .iter()
        .map(|s| s.chars().map(ComparableToken::Scalar).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let needles = owned
        .iter()
        .enumerate()
        .map(|(i, s)| (i, s.as_slice()))
        .collect::<Vec<_>>();
    let mut remaining = 100_000;
    let full = anchors::search_explicit_anchors_with_batch_minimum(
        [&views, &views],
        &needles,
        4,
        &mut remaining,
        EXPLICIT_ANCHOR_RETAINED_LIMIT,
        4,
    )?
    .expect("complete reference");
    let used = 100_000 - remaining;
    for budget in 0..=used {
        let mut remaining = budget;
        if let Some(actual) = anchors::search_explicit_anchors_with_batch_minimum(
            [&views, &views],
            &needles,
            4,
            &mut remaining,
            EXPLICIT_ANCHOR_RETAINED_LIMIT,
            4,
        )? {
            assert_eq!(actual.len(), full.len());
            for ((i, a, b), (j, c, d)) in actual.into_iter().zip(&full) {
                assert_eq!(i, *j);
                for (x, y) in [(a, c), (b, d)] {
                    assert_eq!(x.count, y.count);
                    assert_eq!(
                        x.first.map(|p| (p.view_index, p.start, p.end)),
                        y.first.map(|p| (p.view_index, p.start, p.end))
                    );
                }
            }
        }
    }
    Ok(())
}
