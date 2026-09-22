//! Expected review packets for hand-built comparisons.
//!
//! Each fixture pins one property the packet contract promises: obligations are
//! explained rather than dropped, competing correspondences are offered as
//! hypotheses instead of a single answer, material with no text is answerable
//! only from an image, and a budget produces an explicit omission rather than a
//! silently shorter plan.

use std::collections::BTreeSet;

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison,
        EvidenceFailure, EvidenceIssue, EvidenceStore, HierarchyLimits, MatchingChannels, NodeId,
        PageEvidence, Raster, RenderedEvidence, SourceRef, StructuredEvidence, StructuredValue,
        compare_document_views, document_source_accounting,
    },
    model::{Document, PageId, Vec2},
    pipeline::PipelineOptions,
    review::{
        BundleIdentity, Completeness, EngineClass, EngineOutcome, EngineStatus, POLICY_VERSION,
        PipelineContract, PlannerLimits, REVIEW_SCHEMA, RequiredEvidence, ReviewPlan,
        ReviewQuestion, ReviewReason, SharedEvidenceReview, Side, plan_shared_evidence,
    },
};

struct Fixture {
    store: EvidenceStore,
    graph: DocumentGraph,
}

impl Fixture {
    fn view(&self) -> DocumentView<'_> {
        DocumentView {
            evidence: &self.store,
            graph: &self.graph,
        }
    }
}

struct Build<'a> {
    paragraphs: &'a [&'a str],
    inventory_complete: bool,
    /// Pages that carry a rendered raster but no extracted text.
    image_only_pages: usize,
    issues: Vec<EvidenceIssue>,
}

impl Default for Build<'_> {
    fn default() -> Self {
        Self {
            paragraphs: &[],
            inventory_complete: true,
            image_only_pages: 0,
            issues: Vec::new(),
        }
    }
}

fn build(options: Build<'_>) -> Fixture {
    let structured: Vec<_> = options
        .paragraphs
        .iter()
        .enumerate()
        .map(|(index, text)| StructuredEvidence {
            id: index as u64,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::StructureElement {
                content: None,
                declared_text: None,
                identifier: None,
                glyphs: Vec::new(),
                role: "paragraph".into(),
                text: Some((*text).into()),
                parent: None,
                order: Some(index as u32),
            },
        })
        .collect();
    let rendered: Vec<_> = (0..options.image_only_pages)
        .map(|page| RenderedEvidence {
            id: page as u64,
            page: PageId(page as u32),
            polygon: vec![
                Vec2 { x: 0.0, y: 0.0 },
                Vec2 { x: 10.0, y: 0.0 },
                Vec2 { x: 10.0, y: 10.0 },
                Vec2 { x: 0.0, y: 10.0 },
            ],
            backend: 1,
            raster: Raster {
                width: 1,
                height: 1,
                rgb: vec![255, 255, 255],
            },
            composited_page: true,
        })
        .collect();
    // Every page an issue or a raster refers to must exist in the store.
    let page_count = options
        .issues
        .iter()
        .filter_map(|issue| issue.page)
        .map(|page| page.0 as usize + 1)
        .chain(std::iter::once(options.image_only_pages))
        .max()
        .unwrap_or(0);
    let store = EvidenceStore {
        revision: "planner-fixture".into(),
        native: Document::new(Vec::new()),
        pages: (0..page_count)
            .map(|page| PageEvidence {
                page: PageId(page as u32),
                bounds: None,
            })
            .collect(),
        backends: vec![
            BackendIdentity {
                kind: BackendKind::NativeParser,
                name: "fixture".into(),
                version: "1".into(),
                profile: "source-structure".into(),
                model: None,
            },
            BackendIdentity {
                kind: BackendKind::Renderer,
                name: "fixture-renderer".into(),
                version: "1".into(),
                profile: "page-rgb-white-72dpi".into(),
                model: None,
            },
        ],
        rendered,
        inventories: vec![ChannelInventory {
            page: None,
            channel: Channel::Text,
            backend: 0,
            sources: structured
                .iter()
                .map(|element| SourceRef::Structured {
                    element: element.id,
                })
                .collect(),
            complete: options.inventory_complete,
        }],
        key_inventories: Vec::new(),
        native_structures: Vec::new(),
        issues: options.issues,
        structured,
    };
    let limits = DocumentComparisonLimits::default();
    let graph = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )
    .expect("derive ordered text views");
    Fixture { store, graph }
}

fn channels() -> BTreeSet<Channel> {
    BTreeSet::from([Channel::Text])
}

fn compare(
    old: &Fixture,
    new: &Fixture,
    limits: DocumentComparisonLimits,
) -> DocumentViewComparison {
    compare_document_views(
        old.view(),
        new.view(),
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare fixture views")
}

fn comparison_limits() -> DocumentComparisonLimits {
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&channels());
    limits
}

fn identity() -> BundleIdentity {
    BundleIdentity {
        policy_version: POLICY_VERSION,
        schema: REVIEW_SCHEMA.into(),
        old_sha256: "ab".repeat(32),
        new_sha256: "cd".repeat(32),
        old_bytes: 1,
        new_bytes: 1,
        options: "channels=text".into(),
        old_revision: "planner-fixture".into(),
        new_revision: "planner-fixture".into(),
        backends: vec!["fixture/1/source-structure".into()],
        pipeline: PipelineContract::SharedEvidence,
        selected_channels: channels(),
    }
}

fn outcome() -> EngineOutcome {
    EngineOutcome {
        status: EngineStatus::Incomplete,
        comparison_complete: false,
        typed_changes: 0,
        inferred_changes: 0,
        scope_content_changes: 0,
        inferred_scope_changes: 0,
    }
}

fn plan(old: &Fixture, new: &Fixture, comparison: &DocumentViewComparison) -> ReviewPlan {
    plan_with(old, new, comparison, PlannerLimits::default())
}

fn plan_with(
    old: &Fixture,
    new: &Fixture,
    comparison: &DocumentViewComparison,
    limits: PlannerLimits,
) -> ReviewPlan {
    plan_shared_evidence(
        &SharedEvidenceReview {
            identity: identity(),
            outcome: outcome(),
            old: old.view(),
            new: new.view(),
            comparison,
            channels: &channels(),
        },
        limits,
    )
}

/// Every discovered reference the comparison did not reach is named by a case
/// or by a gap. This is the packet's central promise, and it is checked against
/// the same accounting the engine's coverage counts come from.
fn assert_obligations_explained(
    old: &Fixture,
    new: &Fixture,
    comparison: &DocumentViewComparison,
    plan: &ReviewPlan,
) {
    let accounting = document_source_accounting(old.view(), new.view(), comparison, &channels());
    for channel in &accounting {
        for (side, side_accounting) in [(Side::Old, &channel.old), (Side::New, &channel.new)] {
            for source in side_accounting.uncompared() {
                let in_case = plan.cases.iter().any(|case| {
                    case.evidence
                        .iter()
                        .any(|reference| reference.side == side && reference.source == source)
                });
                let in_gap = plan.manifest.unlocalized_gaps.iter().any(|gap| {
                    gap.sources
                        .iter()
                        .any(|reference| reference.side == side && reference.source == source)
                });
                assert!(
                    in_case || in_gap,
                    "{side:?} {source:?} is an unexplained obligation in channel {:?}",
                    channel.channel
                );
            }
        }
    }
}

#[test]
fn a_deleted_paragraph_becomes_a_correspondence_question_with_its_text() {
    let old = build(Build {
        paragraphs: &[
            "A stable opening paragraph.",
            "A paragraph that only the old revision contains.",
            "A stable closing paragraph.",
        ],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["A stable opening paragraph.", "A stable closing paragraph."],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    assert_obligations_explained(&old, &new, &comparison, &plan);
    let case = plan
        .cases
        .iter()
        .find(|case| {
            case.old_text
                .as_ref()
                .is_some_and(|text| text.text.contains("only the old revision"))
        })
        .expect("the unmatched paragraph is asked about");
    assert_eq!(case.question, ReviewQuestion::ResolveCorrespondence);
    assert!(
        case.required_evidence.is_empty(),
        "retained text is enough to answer this question"
    );
    // The engine established no counterpart here, so the packet offers none and
    // does not present the absence as a decided deletion.
    assert_eq!(case.engine_class, EngineClass::Unavailable);
    assert_eq!(case.alternatives_total, None);
}

#[test]
fn competing_correspondences_are_offered_as_hypotheses_not_as_one_answer() {
    // Two identical candidates on the new side make the correspondence for the
    // repeated old paragraph genuinely ambiguous.
    let old = build(Build {
        paragraphs: &["Shared heading text", "Shared heading text", "A tail."],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["Shared heading text", "A tail."],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    assert_obligations_explained(&old, &new, &comparison, &plan);
    for case in &plan.cases {
        if case.alternatives_total.is_none() {
            continue;
        }
        assert_eq!(
            case.alternatives_total,
            Some(case.alternatives_returned),
            "a closed enumeration reports exactly what it returned"
        );
    }
    assert!(
        plan.cases
            .iter()
            .any(|case| case.question == ReviewQuestion::ResolveCorrespondence),
        "an ambiguous counterpart is a correspondence question"
    );
}

#[test]
fn a_truncated_candidate_search_leaves_the_alternative_count_unknown() {
    let old = build(Build {
        paragraphs: &["First.", "Second.", "Third."],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["First.", "Second changed.", "Third."],
        ..Build::default()
    });
    let mut limits = comparison_limits();
    limits.matching.max_proposals = 1;
    let comparison = compare(&old, &new, limits);
    let plan = plan(&old, &new, &comparison);

    assert_obligations_explained(&old, &new, &comparison, &plan);
    let case = plan
        .cases
        .iter()
        .find(|case| {
            case.reasons
                .iter()
                .any(|reason| reason.reason == ReviewReason::CandidateEnumerationIncomplete)
        })
        .expect("a truncated enumeration is reported as a reason");
    assert_eq!(
        case.completeness.candidate_enumeration,
        Completeness::Incomplete
    );
    assert_eq!(
        case.alternatives_total, None,
        "an unfinished enumeration has no known total"
    );
}

#[test]
fn a_page_without_acquired_text_can_only_be_answered_from_an_image() {
    let old = build(Build {
        paragraphs: &[],
        image_only_pages: 1,
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &[],
        image_only_pages: 1,
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    let visual: Vec<_> = plan
        .cases
        .iter()
        .filter(|case| case.question == ReviewQuestion::InterpretVisualRegion)
        .collect();
    assert_eq!(visual.len(), 2, "one case per side's unread page");
    for case in visual {
        assert_eq!(case.required_evidence, vec![RequiredEvidence::Visual]);
        assert!(case.old_text.is_none() && case.new_text.is_none());
        assert!(
            case.reasons
                .iter()
                .any(|reason| reason.reason == ReviewReason::VisualOnlyRegion)
        );
    }
    assert!(
        plan.manifest
            .capabilities
            .iter()
            .any(|capability| capability.available
                && capability.detail == pdfdelta_core::review::Detail::Visual)
    );
}

#[test]
fn an_unimplemented_channel_is_one_gap_rather_than_a_case_per_page() {
    let issues: Vec<_> = (0..4)
        .map(|page| EvidenceIssue {
            page: Some(PageId(page)),
            channel: Channel::Text,
            sources: Vec::new(),
            boundary: None,
            kind: EvidenceFailure::Unsupported,
            reason: "this interpretation is not implemented".into(),
        })
        .collect();
    let old = build(Build {
        paragraphs: &["A paragraph."],
        issues: issues.clone(),
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["A paragraph."],
        issues,
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    assert_eq!(
        plan.manifest.unlocalized_gaps.len(),
        2,
        "one gap per side, not one per page: {:?}",
        plan.manifest.unlocalized_gaps
    );
    for gap in &plan.manifest.unlocalized_gaps {
        assert_eq!(gap.evidence_complete, Completeness::Incomplete);
        assert_eq!(gap.channel, Some(Channel::Text));
    }
    assert!(
        !plan
            .cases
            .iter()
            .any(|case| case.question == ReviewQuestion::AcquisitionGap),
        "a channel-wide gap is not restated as a located case"
    );
}

#[test]
fn a_localized_acquisition_failure_is_a_case_at_its_page() {
    let issues = vec![EvidenceIssue {
        page: Some(PageId(2)),
        channel: Channel::Text,
        sources: Vec::new(),
        boundary: None,
        kind: EvidenceFailure::ResourceLimit,
        reason: "retained raster budget exhausted".into(),
    }];
    let old = build(Build {
        paragraphs: &["A paragraph."],
        issues,
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["A paragraph."],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    let case = plan
        .cases
        .iter()
        .find(|case| case.question == ReviewQuestion::AcquisitionGap)
        .expect("a localized failure is a case");
    assert_eq!(case.completeness.evidence, Completeness::Incomplete);
    let location = case.old.as_ref().expect("the failing side is located");
    assert_eq!(location.page_number, Some(3));
    assert_eq!(location.page_index, Some(PageId(2)));
    assert_eq!(
        case.required_evidence,
        vec![RequiredEvidence::Unavailable],
        "no retrieval can supply evidence that was never acquired"
    );
}

#[test]
fn a_case_budget_reports_an_omission_instead_of_a_shorter_plan() {
    // Three independent acquisition failures are three independent questions,
    // so a case budget must drop some of them visibly.
    let issues: Vec<_> = (0..3)
        .map(|page| EvidenceIssue {
            page: Some(PageId(page)),
            channel: Channel::Text,
            sources: Vec::new(),
            boundary: None,
            kind: EvidenceFailure::BackendFailure,
            reason: format!("page {page} acquisition failed"),
        })
        .collect();
    let old = build(Build {
        paragraphs: &["A paragraph."],
        issues,
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["A paragraph."],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let generous = plan(&old, &new, &comparison);
    assert!(generous.manifest.export.export_complete);

    assert!(
        generous.cases.len() > 1,
        "the fixture must produce more than one case to bound"
    );
    let allowed = generous.cases.len() - 1;
    let bounded = plan_with(
        &old,
        &new,
        &comparison,
        PlannerLimits {
            max_cases: allowed,
            ..PlannerLimits::default()
        },
    );
    assert_eq!(bounded.cases.len(), allowed);
    assert!(
        !bounded.manifest.export.export_complete,
        "a stopped export says so"
    );
    assert!(!bounded.manifest.export.omissions.is_empty());
    assert!(
        !bounded.manifest.engine.comparison_complete,
        "the engine's own completeness is copied, not recomputed"
    );
    assert_eq!(
        bounded.manifest.engine, generous.manifest.engine,
        "an export budget never changes the engine result"
    );
}

#[test]
fn planning_is_reproducible_and_leaves_the_comparison_untouched() {
    let old = build(Build {
        paragraphs: &["First.", "Second only in old.", "Third."],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["First.", "Third."],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let before = comparison.clone();

    let first = plan(&old, &new, &comparison);
    let second = plan(&old, &new, &comparison);

    assert_eq!(
        comparison, before,
        "planning does not mutate the comparison"
    );
    assert_eq!(first, second, "the same inputs produce the same plan");
    assert_eq!(first.manifest.bundle_id, second.manifest.bundle_id);
    let identifiers: BTreeSet<_> = first.cases.iter().map(|case| &case.case_id).collect();
    assert_eq!(
        identifiers.len(),
        first.cases.len(),
        "case identifiers are unique inside a bundle"
    );
}

#[test]
fn an_open_inventory_is_reported_even_when_every_reference_was_compared() {
    let old = build(Build {
        paragraphs: &["A stable paragraph."],
        inventory_complete: false,
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["A stable paragraph."],
        inventory_complete: false,
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    let gaps = &plan.manifest.inventory_gaps;
    assert_eq!(gaps.len(), 2, "both sides report their open inventory");
    for gap in gaps {
        assert!(!gap.inventory_complete);
        assert_eq!(gap.uncompared_sources, 0);
    }
}

#[test]
fn ambiguous_scope_reports_its_competing_proposals() {
    // A tight conflict budget leaves the solver unable to separate optima, so
    // the scope keeps a competing-correspondence obligation.
    let old = build(Build {
        paragraphs: &["Repeated line", "Repeated line", "Distinct tail."],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["Repeated line", "Distinct tail."],
        ..Build::default()
    });
    let mut limits = comparison_limits();
    limits.matching.max_states_per_component = 1;
    limits.matching.max_assignment_work_per_component = 1;
    let comparison = compare(&old, &new, limits);
    let plan = plan(&old, &new, &comparison);
    assert_obligations_explained(&old, &new, &comparison, &plan);

    let correspondence: Vec<_> = plan
        .cases
        .iter()
        .filter(|case| case.question == ReviewQuestion::ResolveCorrespondence)
        .collect();
    assert!(!correspondence.is_empty());
    let competing = correspondence
        .iter()
        .find(|case| case.hypotheses.len() > 1)
        .expect("the competing proposals are offered as hypotheses");
    assert_eq!(competing.alternatives_returned, competing.hypotheses.len());
    assert!(
        competing
            .hypotheses
            .iter()
            .all(|hypothesis| !hypothesis.conflicts_with.is_empty()),
        "competing hypotheses declare that they exclude each other"
    );
    assert!(
        competing
            .hypotheses
            .iter()
            .any(|hypothesis| !hypothesis.mandatory_in_examined_optima),
        "a competing hypothesis is not presented as established"
    );
    for case in &correspondence {
        for hypothesis in &case.hypotheses {
            assert!(
                hypothesis.objective_weight.is_some(),
                "a hypothesis carries the declared search objective"
            );
            assert!(
                !hypothesis.supplier.is_empty(),
                "a hypothesis names the supplier that proposed it"
            );
        }
        if case.completeness.solver_search == Completeness::Incomplete {
            assert_eq!(
                case.alternatives_total, None,
                "an unfinished search cannot report a total"
            );
        }
    }
}

/// A tagged table whose cells carry their own text.
///
/// Row and column membership is structure, not text: two revisions can hold the
/// same cell strings while associating them with different rows.
fn table(rows: &[&[&str]]) -> Fixture {
    let mut structured = Vec::new();
    let mut next = 0_u64;
    let mut element = |role: &str, text: Option<&str>, parent: Option<u64>, order: u32| {
        let id = next;
        next += 1;
        structured.push(StructuredEvidence {
            id,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::StructureElement {
                content: None,
                declared_text: None,
                identifier: None,
                glyphs: Vec::new(),
                role: role.into(),
                text: text.map(Into::into),
                parent,
                order: Some(order),
            },
        });
        id
    };
    let table = element("table", None, None, 0);
    for (row_index, cells) in rows.iter().enumerate() {
        let row = element(
            "tr",
            None,
            Some(table),
            u32::try_from(row_index).expect("row fits in u32"),
        );
        for (cell_index, text) in cells.iter().enumerate() {
            element(
                "td",
                Some(text),
                Some(row),
                u32::try_from(cell_index).expect("cell fits in u32"),
            );
        }
    }
    let store = EvidenceStore {
        revision: "planner-fixture".into(),
        native: Document::new(Vec::new()),
        pages: Vec::new(),
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "fixture".into(),
            version: "1".into(),
            profile: "source-structure".into(),
            model: None,
        }],
        rendered: Vec::new(),
        inventories: vec![ChannelInventory {
            page: None,
            channel: Channel::Text,
            backend: 0,
            sources: structured
                .iter()
                .map(|element| SourceRef::Structured {
                    element: element.id,
                })
                .collect(),
            complete: true,
        }],
        key_inventories: Vec::new(),
        native_structures: Vec::new(),
        issues: Vec::new(),
        structured,
    };
    let limits = DocumentComparisonLimits::default();
    let graph = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )
    .expect("derive table views");
    Fixture { store, graph }
}

#[test]
fn equal_cell_text_with_a_changed_row_association_is_never_dropped() {
    // Both revisions contain exactly the same cell strings. Only which row each
    // value belongs to differs, so text equality alone must not end the matter.
    let old = table(&[
        &["Region", "Amount"],
        &["North", "10 days"],
        &["South", "20 days"],
    ]);
    let new = table(&[
        &["Region", "Amount"],
        &["North", "20 days"],
        &["South", "10 days"],
    ]);
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    assert_obligations_explained(&old, &new, &comparison, &plan);

    // The two moved values must be visible as a settled change or as a case to
    // review; disappearing because the multiset of strings is unchanged is the
    // failure this fixture exists to catch.
    let values = ["10 days", "20 days"];
    for value in values {
        let element = old
            .store
            .structured
            .iter()
            .find(|element| match &element.value {
                StructuredValue::StructureElement { text, .. } => text.as_deref() == Some(value),
                _ => false,
            })
            .expect("the fixture contains the value");
        let source = SourceRef::Structured {
            element: element.id,
        };
        let settled = comparison.comparisons().any(|pair| {
            pair.operation.is_some()
                && pair.old.iter().any(|node| {
                    old.graph.nodes.iter().any(|graph_node| {
                        graph_node.id == *node && graph_node.sources.contains(&source)
                    })
                })
        });
        let reviewed = plan.cases.iter().any(|case| {
            case.evidence
                .iter()
                .any(|reference| reference.side == Side::Old && reference.source == source)
        });
        assert!(
            settled || reviewed,
            "{value:?} is neither a reported change nor a case to review"
        );
    }
}

#[test]
fn a_larger_budget_may_reveal_competitors_without_having_settled_the_earlier_answer() {
    let old = build(Build {
        paragraphs: &["Repeated line", "Repeated line", "Distinct tail."],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &["Repeated line", "Distinct tail."],
        ..Build::default()
    });

    let mut narrow = comparison_limits();
    narrow.matching.max_proposals = 1;
    let narrow_plan = {
        let comparison = compare(&old, &new, narrow);
        plan(&old, &new, &comparison)
    };
    let wide_plan = {
        let comparison = compare(&old, &new, comparison_limits());
        plan(&old, &new, &comparison)
    };

    // Whatever the wider search found, the narrower one must never have
    // presented its own view as a closed enumeration.
    for case in &narrow_plan.cases {
        if case.completeness.candidate_enumeration == Completeness::Complete {
            continue;
        }
        assert_eq!(
            case.alternatives_total, None,
            "an unfinished enumeration must not report a total"
        );
    }
    let narrow_incomplete = narrow_plan
        .cases
        .iter()
        .any(|case| case.completeness.candidate_enumeration == Completeness::Incomplete);
    assert!(
        narrow_incomplete,
        "the narrow budget must actually report its truncation"
    );
    assert!(
        !wide_plan.cases.is_empty(),
        "the wider search still has material to review"
    );
}

#[test]
fn a_table_case_carries_its_row_and_repeated_occurrences_as_context() {
    let old = table(&[
        &["Region", "Amount"],
        &["North", "10 days"],
        &["South", "10 days"],
    ]);
    let new = table(&[
        &["Region", "Amount"],
        &["North", "20 days"],
        &["South", "10 days"],
    ]);
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);

    assert!(
        !plan.contexts.is_empty(),
        "a structured case gathers surrounding evidence"
    );
    for context in &plan.contexts {
        assert!(
            plan.case(&context.case_id).is_some(),
            "context is only kept for a case the plan retained"
        );
    }
    let kinds: BTreeSet<_> = plan
        .contexts
        .iter()
        .flat_map(|context| context.items.iter().map(|item| item.kind))
        .collect();
    assert!(
        kinds.contains(&pdfdelta_core::review::ContextKind::EnclosingHeading)
            || kinds.contains(&pdfdelta_core::review::ContextKind::TableRow),
        "the enclosing table structure is offered as context: {kinds:?}"
    );
    // The duplicated cell text exists twice in the old revision. When both
    // occurrences belong to one case, the case's own text must still show both,
    // so equal text is never collapsed into a single element.
    let repeated = plan
        .cases
        .iter()
        .filter_map(|case| case.old_text.as_ref())
        .any(|text| text.text.matches("10 days").count() > 1);
    let separated = kinds.contains(&pdfdelta_core::review::ContextKind::OtherOccurrence);
    assert!(
        repeated || separated,
        "a repeated value is either quoted twice or offered as another occurrence: {kinds:?}"
    );
}

/// A stored form field whose value the engine could not reconcile with its
/// declared widget appearance.
fn form_fixture() -> Fixture {
    let structured = vec![StructuredEvidence {
        id: 0,
        page: Some(PageId(0)),
        bounds: None,
        object: None,
        backend: 0,
        value: StructuredValue::FormField {
            name: "deadline".into(),
            field_type: Some(b"Btn".to_vec()),
            value: pdfdelta_core::document::FieldValue::Name(b"Yes".to_vec()),
            widgets: Vec::new(),
            button_states: Vec::new(),
        },
    }];
    let store = EvidenceStore {
        revision: "planner-fixture".into(),
        native: Document::new(Vec::new()),
        pages: vec![PageEvidence {
            page: PageId(0),
            bounds: None,
        }],
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "fixture".into(),
            version: "1".into(),
            profile: "source-structure".into(),
            model: None,
        }],
        rendered: Vec::new(),
        inventories: vec![ChannelInventory {
            page: None,
            channel: Channel::Forms,
            backend: 0,
            sources: vec![SourceRef::Structured { element: 0 }],
            complete: true,
        }],
        key_inventories: Vec::new(),
        native_structures: Vec::new(),
        issues: vec![EvidenceIssue {
            page: Some(PageId(0)),
            channel: Channel::Forms,
            sources: vec![SourceRef::Structured { element: 0 }],
            boundary: None,
            kind: EvidenceFailure::Unresolved,
            reason: "saved button value and declared widget appearance states disagree; neither value was substituted".into(),
        }],
        structured,
    };
    let limits = DocumentComparisonLimits::default();
    let graph = DocumentGraph::from_evidence(
        &store,
        PipelineOptions::default(),
        limits.evidence,
        limits.graph,
    )
    .expect("derive form views");
    Fixture { store, graph }
}

#[test]
fn a_stored_value_the_engine_could_not_reconcile_is_asked_about_not_filed_as_a_gap() {
    let old = form_fixture();
    let new = form_fixture();
    let channels = BTreeSet::from([Channel::Forms]);
    let mut limits = DocumentComparisonLimits::default();
    limits.matching.channels = MatchingChannels::from(&channels);
    let comparison = compare_document_views(
        old.view(),
        new.view(),
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .expect("compare form fixtures");
    let plan = plan_shared_evidence(
        &SharedEvidenceReview {
            identity: identity(),
            outcome: outcome(),
            old: old.view(),
            new: new.view(),
            comparison: &comparison,
            channels: &channels,
        },
        PlannerLimits::default(),
    );

    let case = plan
        .cases
        .iter()
        .find(|case| case.question == ReviewQuestion::CheckValueAppearance)
        .expect("the unreconciled field becomes a question about its appearance");
    assert!(
        case.reasons
            .iter()
            .any(|reason| reason.reason == ReviewReason::ValueAppearanceUnverified)
    );
    // The engine's own wording is retained beside the classification.
    assert!(
        case.reasons.iter().any(|reason| reason
            .message
            .as_deref()
            .is_some_and(|message| message.contains("appearance states disagree"))),
        "{:?}",
        case.reasons
    );
    assert!(
        !plan
            .cases
            .iter()
            .any(|case| case.question == ReviewQuestion::AcquisitionGap),
        "a value that was acquired is not an acquisition failure"
    );
}

#[test]
fn a_case_says_what_the_engine_established_and_settled_ones_are_listed_first() {
    // One paragraph differs; the rest of the material is never reached, so the
    // two kinds of case must be distinguishable without opening them.
    let old = build(Build {
        paragraphs: &[
            "A stable opening paragraph.",
            "The reporting deadline is 10 days.",
            "A stable closing paragraph.",
        ],
        ..Build::default()
    });
    let new = build(Build {
        paragraphs: &[
            "A stable opening paragraph.",
            "The reporting deadline is 20 days.",
            "A stable closing paragraph.",
        ],
        ..Build::default()
    });
    let comparison = compare(&old, &new, comparison_limits());
    let plan = plan(&old, &new, &comparison);
    assert_obligations_explained(&old, &new, &comparison, &plan);

    let findings: Vec<_> = plan.cases.iter().map(|case| case.finding).collect();
    assert!(
        !findings.is_empty(),
        "the fixture leaves something to review"
    );
    // Whatever the engine settled comes first, so stopping early stops on the
    // material the engine could reach.
    let ranks: Vec<u8> = findings.iter().map(|finding| finding.rank()).collect();
    assert!(
        ranks.windows(2).all(|pair| pair[0] <= pair[1]),
        "cases are ordered by what the engine established: {findings:?}"
    );
    for case in &plan.cases {
        // A case that never reached a comparison must not claim a finding.
        if case
            .reasons
            .iter()
            .any(|reason| reason.reason == ReviewReason::DiscoveredButUnexamined)
        {
            assert_eq!(
                case.finding,
                pdfdelta_core::review::CaseFinding::NotEstablished,
                "unexamined material establishes nothing"
            );
        }
    }
}
