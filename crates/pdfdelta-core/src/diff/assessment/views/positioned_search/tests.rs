use super::super::{
    PositionedOccurrences, TokenPostings, View, ViewKind, positioned_occurrences,
    positioned_occurrences_indexed,
};
use super::Search;
use crate::{
    Result,
    diff::GroupText,
    layout::BlockId,
    model::Vec2,
    normalize::{ComparableToken, PositionSignature},
};

fn signature(value: [f64; 2]) -> PositionSignature {
    PositionSignature::new(
        Vec2 {
            x: value[0],
            y: 0.0,
        },
        Vec2 {
            x: value[1],
            y: 0.0,
        },
    )
    .expect("finite fixture position")
}

fn line(text: &str, offset: f64) -> View {
    let tokens = text
        .chars()
        .map(ComparableToken::Scalar)
        .collect::<Vec<_>>();
    let len = tokens.len();
    View {
        kind: ViewKind::Untrusted(0),
        source_order: 0,
        block_indices: vec![0],
        group: GroupText::try_new(vec![BlockId(1)], None, tokens, None, None, None, None)
            .expect("group"),
        source_bounded: true,
        order_certified: false,
        horizontal_text: true,
        position_signatures: Vec::new(),
        page: None,
        block_ranges: std::iter::once(0..len).collect(),
        block_candidates: vec![true],
        token_positions: (0..len)
            .map(|i| Some(signature([i as f64 + offset, 1.0])))
            .collect(),
        token_pages: vec![Some(0); len],
        deny_positions: vec![None; len],
        deny_pages: vec![None; len],
    }
}

#[test]
fn sparse_repeated_queries_pay_setup_and_cut_work_by_half() -> Result<()> {
    let needle = line("ABCD", 0.0);
    let mut views = (0..64)
        .map(|_| line(&"x".repeat(200), 0.0))
        .collect::<Vec<_>>();
    views.push(line("ABCD", 0.0));
    let mut search = Search::new(&views);
    let mut old = 10_000_000;
    let mut new = old;
    for _ in 0..24 {
        assert_eq!(
            search.find(&needle, &(0..4), usize::MAX, &mut new)?,
            positioned_occurrences(&views, &needle, &(0..4), usize::MAX, &mut old)?
        );
    }
    assert!(search.postings.is_some());
    assert!((10_000_000 - new) * 2 < 10_000_000 - old);
    println!(
        "positioned sparse old={} indexed_with_setup={}",
        10_000_000 - old,
        10_000_000 - new
    );
    Ok(())
}

#[test]
fn dense_single_and_short_queries_keep_scanning() -> Result<()> {
    let views = (0..8)
        .map(|_| line(&"A".repeat(60), 0.0))
        .collect::<Vec<_>>();
    for text in ["A", "AAAA"] {
        let needle = line(text, 0.0);
        let range = 0..text.len();
        let mut search = Search::new(&views);
        let mut old = 1_000_000;
        let mut new = old;
        for i in 0..12 {
            assert_eq!(
                search.find(&needle, &range, usize::MAX, &mut new)?,
                positioned_occurrences(&views, &needle, &range, usize::MAX, &mut old)?
            );
            assert!(search.postings.is_none());
            if i == 0 {
                assert_eq!(old - new, if text.len() < 4 { 0 } else { 4 });
            }
        }
        assert!(old - new < 1000);
    }
    Ok(())
}

#[test]
fn budget_stop_is_not_a_negative_answer_or_completed_receipt() -> Result<()> {
    let needle = line("ABCD", 0.0);
    let views = (0..8).map(|_| line("ABCD", 0.0)).collect::<Vec<_>>();
    let mut search = Search::new(&views);
    let mut remaining = 5;
    assert!(
        search
            .find(&needle, &(0..4), usize::MAX, &mut remaining)?
            .is_none()
    );
    assert_eq!(remaining, 0);
    assert_eq!(search.calls, 0);
    assert!(search.last.is_none());
    let expected = positioned_occurrences(&views, &needle, &(0..4), usize::MAX, &mut 1_000_000)?;
    assert_eq!(
        search.find(&needle, &(0..4), usize::MAX, &mut 1_000_000)?,
        expected
    );
    let postings = TokenPostings::build(&views, &mut 1_000_000).expect("index");
    let mut ample = 1_000_000;
    assert_eq!(
        positioned_occurrences_indexed(
            &views,
            &postings,
            &needle,
            &(0..4),
            usize::MAX,
            &mut ample
        )?,
        expected
    );
    let used = 1_000_000 - ample;
    for budget in 0..used {
        assert!(
            positioned_occurrences_indexed(
                &views,
                &postings,
                &needle,
                &(0..4),
                usize::MAX,
                &mut { budget }
            )?
            .is_none()
        );
    }
    assert_eq!(
        positioned_occurrences_indexed(&views, &postings, &needle, &(0..4), usize::MAX, &mut {
            used
        })?,
        expected
    );
    Ok(())
}

#[test]
fn unknown_page_merge_keeps_first_match_and_later_veto() -> Result<()> {
    let needle = line("ABA", 0.0);
    let mut unknown = line("ABA", 0.0);
    unknown.token_positions[2] = None;
    let mut elsewhere = line("ABA", 0.0);
    elsewhere.token_pages[0] = Some(9);
    elsewhere.token_positions[1] = None;
    let views = vec![
        line("ABA", 0.0),
        line("xABAy", -1.0),
        unknown,
        elsewhere,
        line("ABA", 0.0),
    ];
    let postings = TokenPostings::build(&views, &mut 1_000_000).expect("index");
    let actual =
        positioned_occurrences_indexed(&views, &postings, &needle, &(0..3), 0, &mut 1_000_000)?
            .expect("complete");
    assert_eq!(actual.same, 2);
    assert!(actual.unknown);
    assert_eq!(actual.matched, Some((1, 1..4, false)));
    assert_eq!(
        Some(actual),
        positioned_occurrences(&views, &needle, &(0..3), 0, &mut 1_000_000)?
    );
    Ok(())
}

#[test]
fn augmentation_precedes_index_and_populations_stay_separate() -> Result<()> {
    let needle = line("ABCD", 0.0);
    let mut views = vec![line("ABCD", 0.0)];
    views[0].token_positions[0] = None;
    views[0].block_candidates[0] = false;
    let before = Search::new(&views)
        .find(&needle, &(0..4), usize::MAX, &mut 1_000_000)?
        .expect("complete");
    assert!(before.unknown);
    assert_eq!(before.same, 0);
    views[0].token_positions[0] = Some(signature([0.0, 1.0]));
    views[0].block_candidates[0] = true;
    let postings = TokenPostings::build(&views, &mut 1_000_000).expect("post-augmentation index");
    let after = positioned_occurrences_indexed(
        &views,
        &postings,
        &needle,
        &(0..4),
        usize::MAX,
        &mut 1_000_000,
    )?
    .expect("complete");
    assert!(!after.unknown);
    assert_eq!(after.matched, Some((0, 0..4, true)));
    Ok(())
}

#[test]
fn empty_population_and_empty_needle_do_not_nominated_matches() -> Result<()> {
    let needle = line("ABCD", 0.0);
    for views in [Vec::new(), vec![line("ABCD", 0.0)]] {
        let mut search = Search::new(&views);
        assert_eq!(
            search.find(&needle, &(0..0), usize::MAX, &mut 0)?,
            Some(PositionedOccurrences {
                same: 0,
                unknown: false,
                matched: None
            })
        );
    }
    assert_eq!(
        Search::new(&[]).find(&needle, &(0..4), usize::MAX, &mut 0)?,
        Some(PositionedOccurrences {
            same: 0,
            unknown: false,
            matched: None
        })
    );
    Ok(())
}

#[derive(serde::Deserialize)]
struct OracleView {
    text: String,
    positions: Vec<Option<[f64; 2]>>,
    pages: Vec<Option<u32>>,
    deny_positions: Vec<Option<[f64; 2]>>,
    deny_pages: Vec<Option<u32>>,
    candidate: bool,
}
impl OracleView {
    fn materialize(&self) -> View {
        let mut v = line(&self.text, 0.0);
        v.token_positions = self.positions.iter().map(|p| p.map(signature)).collect();
        v.token_pages = self.pages.clone();
        v.deny_positions = self
            .deny_positions
            .iter()
            .map(|p| p.map(signature))
            .collect();
        v.deny_pages = self.deny_pages.clone();
        v.block_candidates[0] = self.candidate;
        v
    }
}
#[derive(serde::Deserialize)]
struct OracleExpected {
    same: usize,
    unknown: bool,
    matched: Option<(usize, usize, usize, bool)>,
}
#[derive(serde::Deserialize)]
struct OracleCase {
    needle: OracleView,
    range: [usize; 2],
    views: Vec<OracleView>,
    self_view: Option<usize>,
    expected: OracleExpected,
}

#[test]
fn independent_python_oracle_includes_unknown_deny_and_unicode() -> Result<()> {
    let cases: Vec<OracleCase> =
        serde_json::from_str(include_str!("oracle.json")).expect("independent oracle");
    assert_eq!(cases.len(), 1024);
    let mut indexed_cases = 0;
    for (number, c) in cases.iter().enumerate() {
        let needle = c.needle.materialize();
        let views = c
            .views
            .iter()
            .map(OracleView::materialize)
            .collect::<Vec<_>>();
        let range = c.range[0]..c.range[1];
        let self_view = c.self_view.unwrap_or(usize::MAX);
        let expected = Some(PositionedOccurrences {
            same: c.expected.same,
            unknown: c.expected.unknown,
            matched: c.expected.matched.map(|(v, s, e, w)| (v, s..e, w)),
        });
        let postings = TokenPostings::build(&views, &mut 10_000_000).expect("bounded index");
        assert_eq!(
            positioned_occurrences_indexed(
                &views,
                &postings,
                &needle,
                &range,
                self_view,
                &mut 10_000_000
            )?,
            expected,
            "indexed oracle case {number}"
        );
        if !range.is_empty() {
            assert_eq!(
                positioned_occurrences(&views, &needle, &range, self_view, &mut 10_000_000)?,
                expected,
                "legacy oracle case {number}"
            );
        }
        let mut search = Search::new(&views);
        let mut remaining = 10_000_000;
        for _ in 0..16 {
            assert_eq!(
                search.find(&needle, &range, self_view, &mut remaining)?,
                expected,
                "adaptive oracle case {number}"
            );
        }
        indexed_cases += usize::from(search.postings.is_some());
    }
    assert!(indexed_cases > 0);
    println!(
        "Python oracle 1024 passed; adaptive indexes admitted for {indexed_cases} populations"
    );
    Ok(())
}
