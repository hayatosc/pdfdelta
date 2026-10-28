use super::super::{TokenPostings, View, ViewKind, translated_occurrences};
use super::{Search, indexed};
use crate::{
    Result,
    diff::GroupText,
    layout::BlockId,
    model::Vec2,
    normalize::{ComparableToken, PositionSignature},
};

fn view(text: &str, positions: &[Option<f64>], pages: &[Option<u32>], candidate: bool) -> View {
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
        block_candidates: vec![candidate],
        token_positions: positions
            .iter()
            .map(|x| {
                x.map(|x| {
                    PositionSignature::new(Vec2 { x, y: 0.0 }, Vec2 { x: 1.0, y: 0.0 })
                        .expect("position")
                })
            })
            .collect(),
        token_pages: pages.to_vec(),
        deny_positions: Vec::new(),
        deny_pages: Vec::new(),
    }
}

fn line(text: &str, offset: f64) -> View {
    let positions = (0..text.chars().count())
        .map(|i| Some(i as f64 + offset))
        .collect::<Vec<_>>();
    view(text, &positions, &vec![Some(0); positions.len()], true)
}

#[test]
fn all_pages_keep_unknown_after_an_earlier_page_mismatch() -> Result<()> {
    let needle = line("AB", 0.0);
    let views = vec![view("AB", &[Some(2.0), None], &[Some(9), Some(0)], false)];
    let postings = TokenPostings::build(&views, &mut 1_000_000).expect("index");
    let expected = translated_occurrences(
        &views,
        &needle,
        &(0..2),
        usize::MAX,
        Vec2 { x: 2.0, y: 0.0 },
        &mut 1_000_000,
    )?
    .expect("scan");
    assert!(expected.unknown);
    assert_eq!(
        indexed(
            &views,
            &postings,
            &needle,
            &(0..2),
            usize::MAX,
            Vec2 { x: 2.0, y: 0.0 },
            &mut 1_000_000
        )?,
        Some(expected)
    );
    Ok(())
}

#[test]
fn ordered_matches_self_exclusion_substrings_and_budget_cuts() -> Result<()> {
    let needle = line("ABA", 0.0);
    let views = vec![line("ABA", 0.0), line("xABAy", -1.0), line("ABA", 0.0)];
    let postings = TokenPostings::build(&views, &mut 1_000_000).expect("index");
    let translation = Vec2 { x: 0.0, y: 0.0 };
    let expected =
        translated_occurrences(&views, &needle, &(0..3), 0, translation, &mut 1_000_000)?
            .expect("scan");
    assert_eq!(expected.same, 2);
    assert_eq!(expected.matched, Some((1, 1..4, false)));
    let mut ample = 1_000_000;
    assert_eq!(
        indexed(
            &views,
            &postings,
            &needle,
            &(0..3),
            0,
            translation,
            &mut ample
        )?,
        Some(expected)
    );
    let used = 1_000_000 - ample;
    for budget in 0..used {
        assert!(
            indexed(&views, &postings, &needle, &(0..3), 0, translation, &mut {
                budget
            })?
            .is_none()
        );
    }
    assert!(
        indexed(&views, &postings, &needle, &(0..3), 0, translation, &mut {
            used
        })?
        .is_some()
    );
    Ok(())
}

#[test]
fn empty_and_signed_zero_keep_literal_translation_rules() -> Result<()> {
    let needle = line("A", 0.0);
    let views = vec![line("A", 0.0)];
    let postings = TokenPostings::build(&views, &mut 1_000).expect("index");
    for x in [0.0, -0.0] {
        let key = Vec2 { x, y: 0.0 };
        assert_eq!(
            indexed(
                &views,
                &postings,
                &needle,
                &(0..1),
                usize::MAX,
                key,
                &mut 1_000
            )?,
            translated_occurrences(&views, &needle, &(0..1), usize::MAX, key, &mut 1_000)?
        );
    }
    let empty = indexed(
        &views,
        &postings,
        &needle,
        &(0..0),
        usize::MAX,
        Vec2 { x: 0.0, y: 0.0 },
        &mut 0,
    )?
    .expect("empty");
    assert_eq!(empty.same, 0);
    assert!(!empty.unknown);
    assert!(empty.matched.is_none());
    Ok(())
}

#[test]
fn setup_is_paid_and_repeated_sparse_searches_amortize() -> Result<()> {
    let needle = line("ABCD", 0.0);
    let mut views = (0..64)
        .map(|_| line(&"x".repeat(200), 0.0))
        .collect::<Vec<_>>();
    views.push(line("ABCD", 2.0));
    let key = Vec2 { x: 2.0, y: 0.0 };
    let mut old = 10_000_000;
    let mut new = old;
    let mut search = Search::new(&views);
    for _ in 0..24 {
        assert_eq!(
            search.find(&needle, &(0..4), usize::MAX, key, &mut new)?,
            translated_occurrences(&views, &needle, &(0..4), usize::MAX, key, &mut old)?
        );
    }
    assert!(search.postings.is_some());
    assert!(
        (10_000_000 - new) * 2 < 10_000_000 - old,
        "old={} new={}",
        10_000_000 - old,
        10_000_000 - new
    );
    println!(
        "translated sparse old={} indexed_with_setup={}",
        10_000_000 - old,
        10_000_000 - new
    );
    Ok(())
}

#[test]
fn dense_queries_refuse_index_and_single_query_keeps_scanning() -> Result<()> {
    for text in ["x".repeat(200), "A".repeat(200)] {
        let views = (0..64).map(|_| line(&text, 0.0)).collect::<Vec<_>>();
        let needle = line("AAAA", 0.0);
        let key = Vec2 { x: 0.0, y: 0.0 };
        let mut search = Search::new(&views);
        let mut old = 10_000_000;
        let mut new = old;
        assert_eq!(
            search.find(&needle, &(0..4), usize::MAX, key, &mut new)?,
            translated_occurrences(&views, &needle, &(0..4), usize::MAX, key, &mut old)?
        );
        assert!(search.postings.is_none());
        assert_eq!(old - new, 4);
        if text.starts_with('A') {
            for _ in 1..24 {
                assert_eq!(
                    search.find(&needle, &(0..4), usize::MAX, key, &mut new)?,
                    translated_occurrences(&views, &needle, &(0..4), usize::MAX, key, &mut old)?
                );
            }
            assert!(search.postings.is_none());
            assert!(search.disabled);
            assert!(old - new < 1000);
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct OracleView {
    text: String,
    positions: Vec<Option<f64>>,
    pages: Vec<Option<u32>>,
    candidate: bool,
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
    views: Vec<OracleView>,
    self_view: Option<usize>,
    translation_x: f64,
    expected: OracleExpected,
}

#[test]
fn fixed_independent_python_metadata_oracle() -> Result<()> {
    let cases: Vec<OracleCase> =
        serde_json::from_str(include_str!("oracle.json")).expect("fixed oracle");
    assert_eq!(cases.len(), 600);
    for (number, case) in cases.iter().enumerate() {
        let n = &case.needle;
        let needle = view(&n.text, &n.positions, &n.pages, n.candidate);
        let views = case
            .views
            .iter()
            .map(|v| view(&v.text, &v.positions, &v.pages, v.candidate))
            .collect::<Vec<_>>();
        let postings = TokenPostings::build(&views, &mut 1_000_000).expect("bounded fixture index");
        let range = 0..n.text.chars().count();
        let key = Vec2 {
            x: case.translation_x,
            y: 0.0,
        };
        let expected = super::super::PositionedOccurrences {
            same: case.expected.same,
            unknown: case.expected.unknown,
            matched: case
                .expected
                .matched
                .map(|(v, start, end, whole)| (v, start..end, whole)),
        };
        let actual = indexed(
            &views,
            &postings,
            &needle,
            &range,
            case.self_view.unwrap_or(usize::MAX),
            key,
            &mut 1_000_000,
        )?;
        assert_eq!(actual, Some(expected), "independent case {number}");
        assert_eq!(
            actual,
            translated_occurrences(
                &views,
                &needle,
                &range,
                case.self_view.unwrap_or(usize::MAX),
                key,
                &mut 1_000_000
            )?,
            "scanning case {number}"
        );
    }
    Ok(())
}

#[test]
fn different_direction_and_nonfinite_delta_never_match() -> Result<()> {
    let needle = line("AB", 0.0);
    let mut wrong = line("AB", 2.0);
    wrong.token_positions[0] = Some(
        PositionSignature::new(Vec2 { x: 2.0, y: 0.0 }, Vec2 { x: -1.0, y: 0.0 })
            .expect("direction"),
    );
    let views = vec![wrong];
    let postings = TokenPostings::build(&views, &mut 1_000).expect("index");
    let actual = indexed(
        &views,
        &postings,
        &needle,
        &(0..2),
        usize::MAX,
        Vec2 { x: 2.0, y: 0.0 },
        &mut 1_000,
    )?
    .expect("complete");
    assert_eq!(actual.same, 0);
    assert!(!actual.unknown);
    let needle = view(
        "AB",
        &[Some(-1e308), Some(-1e308)],
        &[Some(0), Some(0)],
        true,
    );
    let views = vec![view(
        "AB",
        &[Some(1e308), Some(1e308)],
        &[Some(0), Some(0)],
        true,
    )];
    let postings = TokenPostings::build(&views, &mut 1_000).expect("index");
    let actual = indexed(
        &views,
        &postings,
        &needle,
        &(0..2),
        usize::MAX,
        Vec2 { x: 0.0, y: 0.0 },
        &mut 1_000,
    )?
    .expect("complete");
    assert_eq!(actual.same, 0);
    assert!(!actual.unknown);
    Ok(())
}
