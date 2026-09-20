//! Expected review packets for the native-glyph text contract.
//!
//! This adapter examines extracted glyphs only. Its obligations are the
//! comparison's own exclusive partition of comparable tokens, so the fixtures
//! check that every unresolved interval of that partition is accounted for by a
//! case, and that the packet never widens its claim beyond extracted text.

use std::collections::BTreeSet;

use pdfdelta_core::{
    diff::ResolutionState,
    document::Channel,
    layout::BlockId,
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
    pipeline::{ComparisonOutcome, PipelineOptions, compare_extraction_outcomes},
    review::{
        BundleIdentity, EngineOutcome, EngineStatus, NativeTextReview, POLICY_VERSION,
        PipelineContract, PlannerLimits, REVIEW_SCHEMA, RequiredEvidence, ReviewPlan,
        ReviewQuestion, Side, cases_covering, plan_native_text,
    },
    source::{ExtractionIssue, ExtractionIssueKind, ExtractionOutcome, ExtractionScope},
};

fn document(lines: &[(&str, u32, f64)]) -> Document<Glyph> {
    let mut glyphs = Vec::new();
    let mut next = 1_u64;
    for (index, (text, page, y)) in lines.iter().enumerate() {
        let mut offset = 0.0;
        for character in text.chars() {
            if character == ' ' {
                offset += 5.0;
                continue;
            }
            let id = GlyphId(next);
            glyphs.push(Glyph {
                id,
                text: DecodedText::Mapped(character.to_string()),
                raw_code: character.to_string().into_bytes(),
                page: PageId(*page),
                bbox: Rect {
                    min: Vec2 { x: offset, y: *y },
                    max: Vec2 {
                        x: offset + 5.0,
                        y: y + 10.0,
                    },
                },
                baseline: Vec2 { x: offset, y: *y },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: u32::try_from(next).expect("fixture glyph fits in u32"),
                render_mode: TextRenderMode::Fill,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: ObjectRef {
                        object_number: u32::try_from(index + 1).expect("fixture line fits in u32"),
                        generation: 0,
                    },
                    operator_index: u32::try_from(next).expect("fixture glyph fits in u32"),
                },
            });
            next += 1;
            offset += 6.0;
        }
    }
    Document::new(glyphs)
}

fn identity() -> BundleIdentity {
    BundleIdentity {
        policy_version: POLICY_VERSION,
        schema: REVIEW_SCHEMA.into(),
        old_sha256: "ab".repeat(32),
        new_sha256: "cd".repeat(32),
        old_bytes: 1,
        new_bytes: 1,
        options: "native-text-only".into(),
        old_revision: "native-fixture".into(),
        new_revision: "native-fixture".into(),
        backends: vec!["fixture/1/native".into()],
        pipeline: PipelineContract::NativeText,
        selected_channels: BTreeSet::from([Channel::Text]),
    }
}

fn plan(outcome: &ComparisonOutcome) -> ReviewPlan {
    plan_native_text(
        &NativeTextReview {
            identity: identity(),
            outcome: EngineOutcome {
                status: EngineStatus::Incomplete,
                comparison_complete: false,
                typed_changes: outcome.comparison.changes.len(),
                inferred_changes: 0,
                scope_content_changes: 0,
                inferred_scope_changes: 0,
            },
            comparison: &outcome.comparison,
            old_blocks: &outcome.old_blocks,
            new_blocks: &outcome.new_blocks,
            old_glyphs: &outcome.old_glyph_evidence,
            new_glyphs: &outcome.new_glyph_evidence,
            extraction: &outcome.extraction,
        },
        PlannerLimits::default(),
    )
}

/// Every unresolved interval of the comparison's own partition is accounted for.
fn assert_partition_explained(outcome: &ComparisonOutcome, plan: &ReviewPlan) {
    let Some(assessment) = outcome.comparison.assessment.as_ref() else {
        return;
    };
    for (side, ranges) in [
        (Side::Old, &assessment.old_resolution),
        (Side::New, &assessment.new_resolution),
    ] {
        for range in ranges {
            if range.state != ResolutionState::Unresolved {
                continue;
            }
            for position in range.comparable_range.start..range.comparable_range.end {
                assert!(
                    !cases_covering(plan, side, BlockId(range.block.0), position).is_empty(),
                    "{side:?} block {} token {position} is an unexplained obligation",
                    range.block.0
                );
            }
        }
    }
}

#[test]
fn unrelated_revisions_leave_explained_correspondence_questions() {
    let old = document(&[
        ("The reporting deadline is 10 days.", 0, 100.0),
        ("An unrelated closing note.", 0, 80.0),
    ]);
    let new = document(&[
        ("Entirely different opening matter.", 0, 100.0),
        ("Another unrelated sentence here.", 0, 80.0),
    ]);
    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )
    .expect("compare native glyph documents");
    let plan = plan(&outcome);

    assert_partition_explained(&outcome, &plan);
    for case in &plan.cases {
        assert_eq!(
            case.channels,
            BTreeSet::from([Channel::Text]),
            "the native contract never claims another channel"
        );
        assert_eq!(case.pipeline, PipelineContract::NativeText);
    }
}

#[test]
fn a_retained_extraction_issue_is_an_acquisition_case_that_no_retrieval_can_answer() {
    let old = ExtractionOutcome::new(
        document(&[("Retained opening text.", 0, 100.0)]),
        vec![
            ExtractionIssue::new(
                ExtractionIssueKind::Unsupported,
                ExtractionScope::Page(PageId(1)),
                "page evidence is unavailable",
            )
            .expect("fixture issue"),
        ],
    )
    .expect("fixture outcome");
    let new = ExtractionOutcome::complete(document(&[("Retained opening text.", 0, 100.0)]));
    let outcome = compare_extraction_outcomes(old, new, PipelineOptions::default())
        .expect("compare with a retained issue");
    let plan = plan(&outcome);

    let case = plan
        .cases
        .iter()
        .find(|case| case.question == ReviewQuestion::AcquisitionGap)
        .expect("the extraction issue becomes a case");
    let location = case.old.as_ref().expect("the failing side is located");
    assert_eq!(location.page_index, Some(PageId(1)));
    assert_eq!(location.page_number, Some(2));
    assert_eq!(
        case.required_evidence,
        vec![RequiredEvidence::Unavailable],
        "this contract acquires no rasters, so nothing further can be retrieved"
    );
    assert!(
        plan.manifest
            .capabilities
            .iter()
            .all(
                |capability| capability.detail != pdfdelta_core::review::Detail::Visual
                    || !capability.available
            ),
        "a text-only contract does not advertise visual retrieval"
    );
}

#[test]
fn native_planning_is_reproducible() {
    let old = document(&[("A stable opening paragraph.", 0, 100.0)]);
    let new = document(&[("A stable opening paragraph, revised.", 0, 100.0)]);
    let outcome = compare_extraction_outcomes(
        ExtractionOutcome::complete(old),
        ExtractionOutcome::complete(new),
        PipelineOptions::default(),
    )
    .expect("compare native glyph documents");

    assert_eq!(plan(&outcome), plan(&outcome));
    assert_partition_explained(&outcome, &plan(&outcome));
}
