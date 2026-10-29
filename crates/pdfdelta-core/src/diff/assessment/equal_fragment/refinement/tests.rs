use super::*;
use crate::{
    alignment::BlockSeparator,
    diff::SidePlan,
    layout::BlockRole,
    model::GlyphId,
    normalize::{MappedText, SourceMapEntry, TextSource, TextSourceAtom},
};
use serde::Deserialize;

fn block(id: u64, text: &str, page: u32) -> BlockText {
    let mapped = MappedText {
        text: text.into(),
        unmapped: Vec::new(),
        source_map: text
            .chars()
            .enumerate()
            .map(|(i, _)| SourceMapEntry {
                output_range: ScalarRange {
                    start: i,
                    end: i + 1,
                },
                source: TextSource {
                    atoms: vec![TextSourceAtom::Glyph(GlyphId(id * 1000 + i as u64 + 1))].into(),
                },
            })
            .collect(),
    };
    BlockText {
        block: BlockId(id),
        role: BlockRole::Body,
        raw: mapped.clone(),
        matching_tokens: mapped.comparable_tokens().expect("fixture tokens"),
        canonical: mapped,
        matching: text.into(),
        numeric_mask_applied: false,
        normalization_events: Vec::new(),
        issues: Vec::new(),
        pages: vec![page],
        font_size_signatures: None,
        position_signatures: None,
        line_breaks: Some(Vec::new()),
        page_breaks: Some(Vec::new()),
    }
}

#[derive(Deserialize)]
struct Captured {
    old: Vec<String>,
    new: Vec<String>,
    separator: String,
    expected: Vec<[usize; 8]>,
}

fn fixture(
    old: &[String],
    new: &[String],
    separator: BlockSeparator,
    mutation: u8,
    budget: usize,
) -> Result<(Vec<[usize; 8]>, usize)> {
    let mut old_blocks: Vec<_> = old
        .iter()
        .enumerate()
        .map(|(i, t)| block(i as u64 + 1, t, 7))
        .collect();
    let new_blocks: Vec<_> = new
        .iter()
        .enumerate()
        .map(|(i, t)| block(i as u64 + 101, t, 5))
        .collect();
    match mutation {
        1 => {
            let source = old_blocks[0].canonical.source_map[0].source.clone();
            old_blocks[1].raw.source_map[0].source = source.clone();
            old_blocks[1].canonical.source_map[0].source = source;
        }
        2 => old_blocks[0].raw.text.replace_range(..1, "Z"),
        3 => old_blocks[0].canonical.source_map[0]
            .source
            .atoms
            .push(TextSourceAtom::Glyph(GlyphId(999_999))),
        4 => old_blocks[1].pages = vec![8],
        5 => {
            let gid = GlyphId(999_999);
            old_blocks[0].canonical.source_map[0].source.atoms =
                vec![TextSourceAtom::SyntheticSpace {
                    preceding: gid,
                    following: gid,
                }]
                .into();
        }
        _ => {}
    }
    let a = SidePlan::inspect("refinement old", &old_blocks)?.materialize()?;
    let b = SidePlan::inspect("refinement new", &new_blocks)?.materialize()?;
    let sides = [&a, &b];
    let key = DomainKey {
        local: None,
        old: 0..old.len(),
        new: 0..new.len(),
        old_separator: separator,
        new_separator: separator,
    };
    let mut setup = 32_000_000;
    let spans = [
        light_group_metadata(&a, key.old.clone(), separator, &mut setup, 64 * 1024 * 1024)
            .expect("old scope")
            .span,
        light_group_metadata(&b, key.new.clone(), separator, &mut setup, 64 * 1024 * 1024)
            .expect("new scope")
            .span,
    ];
    let mut proof = DomainProof {
        scope: ProofScope::ExactKey,
        relation: 0,
        unique: true,
        search: SearchCompleteness::Complete,
        edits: Vec::new(),
        lengths: spans.each_ref().map(|s| s.comparable_range.end),
        strict_unique: true,
        stable_events: None,
    };
    let mut parent = RelationAssessment {
        old_span: Some(spans[0].clone()),
        new_span: Some(spans[1].clone()),
        parent: None,
        outcome: RelationOutcome::Established,
        search: SearchCompleteness::Complete,
        reasons: Vec::new(),
        assumptions: vec![ComparisonAssumption::LocalEvidenceBoundaries],
    };
    match mutation {
        6 => proof.strict_unique = false,
        7 => proof.unique = false,
        8 => proof.scope = ProofScope::BroadRootRefusal,
        9 => proof.search = SearchCompleteness::Incomplete,
        10 => parent.parent = Some(0),
        11 => parent.outcome = RelationOutcome::Tentative,
        12 => parent
            .assumptions
            .push(ComparisonAssumption::AlternativeLineBreakNormalization),
        13 => {
            parent
                .old_span
                .as_mut()
                .expect("old")
                .comparable_range
                .start = 1;
        }
        14 => proof.lengths[0] += 1,
        15 => parent.old_span.as_mut().expect("old").blocks.reverse(),
        16 => parent.old_span.as_mut().expect("old").blocks[1] = BlockId(1),
        _ => {}
    }
    let mut remaining = budget;
    let mut cache = EqualFragmentCache::new(sides);
    let cert = cache.prove_refined_equal_parent(
        &key,
        &proof,
        &parent,
        64 * 1024 * 1024,
        &mut remaining,
    )?;
    assert!(remaining <= budget);
    let Some(cert) = cert else {
        return Ok((Vec::new(), budget - remaining));
    };
    assert!(cert.runs.iter().all(|r| r.parent_end > r.parent_start));
    let mut check_work = 1_000_000;
    for run in &cert.runs {
        assert!(cert.accepts(sides, 0, [&run.spans[0], &run.spans[1]], &mut check_work));
        assert!(!cert.accepts(sides, 1, [&run.spans[0], &run.spans[1]], &mut check_work));
        assert!(!cert.accepts([&b, &a], 0, [&run.spans[0], &run.spans[1]], &mut check_work));
        let mut changed = run.spans[0].clone();
        changed.canonical_range.end += 1;
        assert!(!cert.accepts(sides, 0, [&changed, &run.spans[1]], &mut check_work));
    }
    Ok((
        cert.runs
            .iter()
            .map(|r| {
                [
                    r.spans[0].blocks[0].0 as usize - 1,
                    r.spans[0].comparable_range.start,
                    r.spans[0].comparable_range.end,
                    r.spans[1].blocks[0].0 as usize - 101,
                    r.spans[1].comparable_range.start,
                    r.spans[1].comparable_range.end,
                    r.parent_start,
                    r.parent_end,
                ]
            })
            .collect(),
        budget - remaining,
    ))
}

#[test]
fn c34_refinement_matches_independent_matching_and_cut_oracle() -> Result<()> {
    let captured: Vec<Captured> =
        serde_json::from_str(include_str!("oracle.json")).expect("Python oracle");
    assert_eq!(captured.len(), 2133);
    for (index, case) in captured.iter().enumerate() {
        let separator = if case.separator == "space" {
            BlockSeparator::Space
        } else {
            BlockSeparator::Concatenate
        };
        let (actual, _) = fixture(&case.old, &case.new, separator, 0, 1_000_000)?;
        assert_eq!(
            actual, case.expected,
            "case {index}: {:?} / {:?}",
            case.old, case.new
        );
    }
    Ok(())
}

#[test]
fn c34_refinement_holds_source_and_parent_counterexamples() -> Result<()> {
    let old = vec!["AB CD".to_owned(), "EF".to_owned()];
    let new = vec!["AB".to_owned(), "CD".to_owned(), "EF".to_owned()];
    assert_eq!(
        fixture(&old, &new, BlockSeparator::Space, 0, 1_000_000)?
            .0
            .len(),
        3
    );
    for mutation in 1..=16 {
        assert!(
            fixture(&old, &new, BlockSeparator::Space, mutation, 1_000_000)?
                .0
                .is_empty(),
            "mutation {mutation}"
        );
    }
    Ok(())
}

#[test]
fn c34_refinement_never_certifies_a_partial_parent_at_budget_stop() -> Result<()> {
    let old = vec!["AB CD".to_owned(), "EF".to_owned()];
    let new = vec!["AB".to_owned(), "CD".to_owned(), "EF".to_owned()];
    let full = fixture(&old, &new, BlockSeparator::Space, 0, 1_000_000)?;
    assert!(full.1 < 100_000);
    for budget in 0..full.1 {
        assert!(
            fixture(&old, &new, BlockSeparator::Space, 0, budget)?
                .0
                .is_empty(),
            "budget {budget}"
        );
    }
    assert_eq!(
        fixture(&old, &new, BlockSeparator::Space, 0, full.1)?.0,
        full.0
    );
    Ok(())
}
