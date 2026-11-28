use std::{collections::BTreeSet, io::Write as _, path::Path, sync::Arc};

use pdfdelta_core::document::image_diff::{ImageDiff, ImageInventory, compare_images};
use pdfdelta_core::{
    document::{
        BackendIdentity, Channel, ChannelCoverage, ComparisonContract, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison,
        EvidenceFailure, EvidenceIssue, EvidenceLimits, EvidenceStore, HierarchyLimits,
        InterpretationStatus, KeyDomain, MatchingChannels, MatchingLimits, NodeContent, NodeId,
        TableRefinements, compare_document_views, document_coverage, refine_table_views,
    },
    model::PageId,
    options::PipelineOptions,
    pdf::ParseLimits,
};
use pdfdelta_diff::Error as DiffError;
use serde::Serialize;

use crate::{
    args::{ColorChoice, ComparisonInput, ComparisonOptions, resolve_color},
    fs::{
        MAX_PASSWORD_FILE_BYTES, input_read_diff_error, read_limited_typed,
        read_password_file_typed, write_output_atomically, write_text_report_atomically,
    },
};

pub struct EvidenceOptions<'a> {
    pub channels: BTreeSet<Channel>,
    /// Flow text comparison that supplies the text channel.
    pub flow_text: Option<&'a crate::flow_cmd::FlowComparison>,
    /// Explicit case budget for an agent review bundle.
    pub agent_case_budget: usize,
}

#[derive(Serialize)]
pub(super) struct EvidenceSummary<'a> {
    revision: &'a str,
    pages: usize,
    native_glyphs: usize,
    rendered_regions: usize,
    structured_elements: usize,
    form_fields: Vec<&'a pdfdelta_core::document::StructuredEvidence>,
    rendered_sources: Vec<RenderedSource<'a>>,
    inventories: Vec<InventorySummary>,
    /// Observed paint is a potential text-discovery obligation, not a claim
    /// that every marked page contains additional characters.
    non_text_paint_pages: Vec<pdfdelta_core::model::PageId>,
    backends: &'a [BackendIdentity],
    issues: &'a [EvidenceIssue],
}

#[derive(Serialize)]
struct InventorySummary {
    page: Option<pdfdelta_core::model::PageId>,
    channel: Channel,
    backend: usize,
    discovered_sources: usize,
    complete: bool,
}

#[derive(Serialize)]
struct RenderedSource<'a> {
    id: u64,
    page: PageId,
    polygon: &'a [pdfdelta_core::model::Vec2],
    backend: usize,
    width: u32,
    height: u32,
}

impl<'a> EvidenceSummary<'a> {
    pub(super) fn new(store: &'a EvidenceStore) -> Self {
        Self {
            revision: &store.revision,
            pages: store.pages.len(),
            native_glyphs: store.native.items().len(),
            rendered_regions: store.rendered.len(),
            structured_elements: store.structured.len(),
            form_fields: store
                .structured
                .iter()
                .filter(|element| {
                    matches!(
                        element.value,
                        pdfdelta_core::document::StructuredValue::FormField { .. }
                    )
                })
                .collect(),
            rendered_sources: store
                .rendered
                .iter()
                .map(|region| RenderedSource {
                    id: region.id,
                    page: region.page,
                    polygon: &region.polygon,
                    backend: region.backend,
                    width: region.raster.width,
                    height: region.raster.height,
                })
                .collect(),
            inventories: store
                .inventories
                .iter()
                .map(|inventory| InventorySummary {
                    page: inventory.page,
                    channel: inventory.channel,
                    backend: inventory.backend,
                    discovered_sources: inventory.sources.len(),
                    complete: inventory.complete,
                })
                .collect(),
            non_text_paint_pages: store.native.last_non_text_paint().keys().copied().collect(),
            backends: &store.backends,
            issues: &store.issues,
        }
    }
}

#[derive(Serialize)]
struct DocumentReport<'a> {
    schema_version: u32,
    /// Includes extraction and child rendering; excludes report serialization and I/O.
    comparison_wall_time_ms: u64,
    contract: ComparisonContract,
    comparison_complete: bool,
    typed_changes: usize,
    inferred_changes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_diff: Option<ImageReport>,
    /// Non-owning range content changes, separate from strict typed changes.
    scope_content_changes: usize,
    inferred_scope_changes: usize,
    coverage: Vec<ChannelCoverage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_basis: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    difference_status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_text: Option<serde_json::Value>,
    /// Shared-evidence text positions that flow ownership replaced. Absent when
    /// the shared search recorded none; absence is not a completed text search.
    #[serde(skip_serializing_if = "Option::is_none")]
    flow_owned_text: Option<pdfdelta_core::document::FlowOwnedTextEvidence>,
    /// Per-channel detected/broken/unexamined verdicts used to select the flow
    /// exit. Present only for the flow engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    channel_verdicts: Option<Vec<crate::flow_exit::ChannelVerdict>>,
    old: EvidenceSummary<'a>,
    new: EvidenceSummary<'a>,
    table_refinements: TableRefinements,
    comparison: DocumentViewComparison,
}

#[derive(Serialize)]
pub(super) struct ImageReport {
    pub old: ImageInventory,
    pub new: ImageInventory,
    pub comparison: ImageDiff,
}

pub fn compare(
    old_input: ComparisonInput<'_>,
    new_input: ComparisonInput<'_>,
    pipeline: PipelineOptions,
    output: ComparisonOptions<'_>,
    cache_dir: Option<&Path>,
    options: &EvidenceOptions<'_>,
) -> Result<
    (
        u8,
        bool,
        crate::render::RenderCpuReport,
        crate::image_hashes::ImageCpuReport,
    ),
    DiffError,
> {
    let started = std::time::Instant::now();
    let retain_inputs = output.review_dir.is_some() || output.agent_review_dir.is_some();
    let (old, old_bytes, old_images, old_image_cpu, old_render_cpu) = collect(
        old_input,
        cache_dir,
        options,
        retain_inputs,
        output.limit_scale,
    )?;
    let (new, new_bytes, new_images, new_image_cpu, new_render_cpu) = collect(
        new_input,
        cache_dir,
        options,
        retain_inputs,
        output.limit_scale,
    )?;
    let image_diff = match (old_images, new_images) {
        (Some(old), Some(new)) => {
            let comparison = compare_images(&old, &new).map_err(DiffError::from)?;
            Some(ImageReport {
                old,
                new,
                comparison,
            })
        }
        _ => None,
    };
    let mut matching_channels = MatchingChannels::from(&options.channels);
    if options.flow_text.is_some() {
        matching_channels.text = false;
    }
    // The flow engine owns text whenever it is selected. When text is not a
    // selected channel, text views remain available as relationship
    // correspondence context, but their local comparisons must not become exit
    // evidence: they are superseded as they are when flow owns the text
    // channel. Without this, a forms- or relations-only run could exit 1 on a
    // text difference the caller did not select.
    let text_superseded = options.flow_text.is_some() || !options.channels.contains(&Channel::Text);
    let limits = DocumentComparisonLimits {
        matching: MatchingLimits {
            channels: matching_channels,
            ..MatchingLimits::default()
        },
        // Local text comparisons do not become exit evidence when flow owns
        // the text channel or text is unselected.
        supersede_text: text_superseded,
        // Page rasters remain review context, not a second diff of native text.
        visual: pdfdelta_core::document::VisualCandidateLimits {
            include_composited_pages: false,
            ..Default::default()
        },
        ..DocumentComparisonLimits::default()
    };
    let mut old_graph = DocumentGraph::from_evidence(&old, pipeline, limits.evidence, limits.graph)
        .map_err(DiffError::from)?;
    let mut new_graph = DocumentGraph::from_evidence(&new, pipeline, limits.evidence, limits.graph)
        .map_err(DiffError::from)?;
    let table_refinements = if limits.matching.channels.text {
        refine_table_views(&mut old_graph, &mut new_graph, &old, &new, pipeline, limits)
            .map_err(DiffError::from)?
    } else {
        TableRefinements {
            old: Vec::new(),
            new: Vec::new(),
            exhaustive: true,
        }
    };
    let mut comparison = compare_document_views(
        DocumentView {
            evidence: &old,
            graph: &old_graph,
        },
        DocumentView {
            evidence: &new,
            graph: &new_graph,
        },
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        HierarchyLimits::default(),
    )
    .map_err(DiffError::from)?;
    if !table_refinements.exhaustive {
        comparison.retain_relation_unresolved(
            pdfdelta_core::document::UnresolvedObligation::new(
                pdfdelta_core::document::UnresolvedReason::CounterpartRefinementIncomplete,
            ),
            "counterpart table refinement search is incomplete",
        );
    }
    let selected_nodes = pdfdelta_core::document::selected_nodes(
        &old_graph,
        MatchingChannels {
            relations: false,
            presentation: false,
            ..MatchingChannels::from(&options.channels)
        },
    );
    for scope in &mut comparison.scopes {
        scope
            .result
            .comparisons
            .retain(|pair| pair.old.iter().all(|node| selected_nodes.contains(node)));
    }
    let mut coverage_channels = options.channels.clone();
    if options.flow_text.is_some() {
        coverage_channels.remove(&Channel::Text);
    }
    let mut coverage = document_coverage(
        DocumentView {
            evidence: &old,
            graph: &old_graph,
        },
        DocumentView {
            evidence: &new,
            graph: &new_graph,
        },
        &comparison,
        &coverage_channels,
    );
    if let Some(flow) = options.flow_text {
        let text_complete = flow.text_complete();
        let old_tokens = flow.old_side.stream.tokens.len();
        let new_tokens = flow.new_side.stream.tokens.len();
        // Coverage counts discovered sources that were compared. Flow examined
        // every token it extracted; extraction completeness is recorded
        // separately and still limits the channel's overall completeness.
        coverage.push(ChannelCoverage {
            channel: Channel::Text,
            old_inventory_complete: text_complete,
            new_inventory_complete: text_complete,
            old_discovered_sources: old_tokens,
            new_discovered_sources: new_tokens,
            old_compared_sources: old_tokens,
            new_compared_sources: new_tokens,
            old_presence_sources: 0,
            new_presence_sources: 0,
            old_uncompared_sources: 0,
            new_uncompared_sources: 0,
            complete: text_complete,
        });
        coverage.sort_by_key(|entry| entry.channel);
    }
    let flow_changes = options
        .flow_text
        .map_or(0, crate::flow_cmd::FlowComparison::non_layout_changes);
    let changes = flow_changes
        + comparison
            .comparisons()
            .filter(|pair| {
                pair.operation.is_some()
                    && pair.interpretation == InterpretationStatus::ConditionalOnCorrespondence
            })
            .count()
        + comparison
            .relations()
            .filter(|relation| {
                relation.changed()
                    && relation.interpretation == InterpretationStatus::ConditionalOnCorrespondence
            })
            .count()
        + comparison.keyed_element_operations().count();
    let inferred_changes = image_diff
        .as_ref()
        .map_or(0, |images| images.comparison.changes.len())
        + comparison
            .comparisons()
            .filter(|pair| {
                pair.operation.is_some() && pair.interpretation == InterpretationStatus::Inferred
            })
            .count()
        + comparison
            .relations()
            .filter(|relation| {
                relation.changed() && relation.interpretation == InterpretationStatus::Inferred
            })
            .count();
    let scope_changes = |interpretation| {
        comparison
            .scopes
            .iter()
            .flat_map(|scope| &scope.result.text_scope_reviews)
            .filter(|review| review.comparison.interpretation == interpretation)
            .count()
    };
    let scope_content_changes = scope_changes(InterpretationStatus::ConditionalOnCorrespondence);
    let inferred_scope_changes = scope_changes(InterpretationStatus::Inferred);
    let detected = changes > 0;
    let detected_channels =
        comparison_detected_channels(&comparison, &old_graph, &new_graph, options.flow_text);
    let document_verdict = Some(crate::flow_exit::document_verdicts(
        crate::flow_exit::DocumentEvidence {
            comparison: &comparison,
            coverage: &coverage,
            channels: &options.channels,
            flow: options.flow_text,
            image_incomplete: image_diff
                .as_ref()
                .is_some_and(|images| !images.comparison.complete),
            old_issues: &old.issues,
            new_issues: &new.issues,
            detected_channels: &detected_channels,
        },
    ));
    // A text issue from native acquisition that flow does not supersede still
    // leaves the text channel unexamined or broken. Reflect it in coverage so
    // readers cannot see a complete text channel next to an unexamined verdict.
    if let Some(text) = document_verdict.as_ref().and_then(|verdict| {
        verdict
            .verdicts
            .iter()
            .find(|entry| entry.channel == crate::flow_exit::ChannelClass::Text)
    }) {
        let clean = !text.broken && !text.unexamined;
        for entry in coverage
            .iter_mut()
            .filter(|entry| entry.channel == Channel::Text)
        {
            entry.complete &= clean;
            entry.old_inventory_complete &= clean;
            entry.new_inventory_complete &= clean;
        }
    }
    let structural_complete =
        coverage.iter().all(|channel| channel.complete) && comparison.search_resolved();
    let complete = structural_complete
        && document_verdict
            .as_ref()
            .is_none_or(|verdict| !verdict.broken && !verdict.unexamined);
    let flow_exit = document_verdict
        .as_ref()
        .map(|verdict| crate::flow_exit::select_channels(&verdict.verdicts, structural_complete));
    let exit_basis = flow_exit.map(|exit| exit.basis);
    let difference_status = flow_exit.map(|_| {
        if detected {
            "detected"
        } else if complete {
            "no_content_change"
        } else {
            "indeterminate"
        }
    });
    let flow_text = options.flow_text.map(|flow| {
        let native = crate::flow_native::native_report(flow);
        let report = &native["report"];
        serde_json::json!({
            "complete": flow.text_complete(),
            "summary": report["summary"],
            "changes": report["changes"],
            "reading_order_changes": report["reading_order_changes"],
            "formatting_only_changes": report["formatting_only_changes"],
            // Text evidence reasons are part of the comparison record, not
            // only of the HTML presentation. Unexamined paint and image-only
            // pages therefore stay readable from the JSON alone.
            "issues": crate::flow_review::issues_json(flow),
            "order": crate::flow_review::order_json(flow),
            "flow_summary": report["flow_summary"],
            "exit_basis": exit_basis,
        })
    });
    let report = DocumentReport {
        schema_version: 3,
        engine: Some("flow"),
        exit_basis,
        difference_status,
        flow_text,
        flow_owned_text: options
            .flow_text
            .filter(|_| !comparison.flow_owned_text.is_empty())
            .map(|_| comparison.flow_owned_text.clone()),
        channel_verdicts: document_verdict
            .as_ref()
            .map(|verdict| verdict.verdicts.clone()),
        comparison_wall_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        contract: ComparisonContract {
            version: 1,
            channels: options.channels.clone(),
        },
        comparison_complete: complete,
        typed_changes: changes,
        inferred_changes,
        image_diff,
        scope_content_changes,
        inferred_scope_changes,
        coverage,
        old: EvidenceSummary::new(&old),
        new: EvidenceSummary::new(&new),
        table_refinements,
        comparison,
    };
    if let Some(path) = output.json_path {
        write_output_atomically(path, "document JSON report", |writer| {
            serde_json::to_writer_pretty(writer, &report)
                .map_err(|error| format!("cannot write document report: {error}"))
        })
        .map_err(DiffError::backend)?;
    }
    if let Some(directory) = output.agent_review_dir {
        let (Some(old_bytes), Some(new_bytes)) = (&old_bytes, &new_bytes) else {
            return Err(DiffError::backend("review source bytes were not retained"));
        };
        // The plan is projected from this run's own evidence, so its references
        // point at the material this comparison actually examined.
        let flow_owned = options.flow_text.map(crate::flow_review::project);
        let flow_review = flow_owned
            .as_ref()
            .zip(options.flow_text)
            .map(|(owned, comparison)| owned.review(comparison));
        let mut spool = crate::agent_review::PlanSpool::new()?;
        let mut plan = pdfdelta_core::review::plan_shared_evidence_streaming(
            &pdfdelta_core::review::SharedEvidenceReview {
                identity: bundle_identity(
                    old_bytes,
                    new_bytes,
                    &old,
                    &new,
                    &options.channels,
                    output.limit_scale,
                ),
                outcome: engine_outcome(&report),
                old: DocumentView {
                    evidence: &old,
                    graph: &old_graph,
                },
                new: DocumentView {
                    evidence: &new,
                    graph: &new_graph,
                },
                comparison: &report.comparison,
                channels: &options.channels,
                flow: flow_review.as_ref(),
            },
            pdfdelta_core::review::PlannerLimits {
                max_cases: options.agent_case_budget,
                ..pdfdelta_core::review::PlannerLimits::default()
            },
            &mut spool.raw,
            &mut spool.final_store,
        )
        .map_err(|error| DiffError::backend(error.to_string()))?;
        crate::agent_review::multiply_plan(&mut plan, &mut spool.final_store)?;
        crate::agent_review::write_streaming(
            directory,
            &plan,
            &mut spool.final_store,
            crate::agent_review::SourceDocument {
                name: old_input.path,
                bytes: old_bytes,
            },
            crate::agent_review::SourceDocument {
                name: new_input.path,
                bytes: new_bytes,
            },
            &page_rasters(&old, &new),
        )?;
    }
    if let Some(directory) = output.review_dir {
        let (Some(old_bytes), Some(new_bytes)) = (old_bytes, new_bytes) else {
            return Err(DiffError::backend("review source bytes were not retained"));
        };
        crate::review::write(
            directory,
            &report,
            &report.comparison,
            report.image_diff.as_ref(),
            complete,
            crate::review::FlowReview {
                flow: options.flow_text,
                verdicts: document_verdict
                    .as_ref()
                    .map(|verdict| verdict.verdicts.as_slice()),
            },
            (
                crate::review::Input {
                    view: DocumentView {
                        evidence: &old,
                        graph: &old_graph,
                    },
                    name: old_input.path,
                    bytes: &old_bytes,
                },
                crate::review::Input {
                    view: DocumentView {
                        evidence: &new,
                        graph: &new_graph,
                    },
                    name: new_input.path,
                    bytes: &new_bytes,
                },
            ),
        )?;
    }
    let mut text = format!(
        "Document comparison: {}\nTyped changes: {changes}\nInferred changes: {inferred_changes}\nScope content changes (B; non-owning): {scope_content_changes}\nInferred scope changes (C; non-owning): {inferred_scope_changes}\n",
        if complete { "complete" } else { "incomplete" }
    );
    for coverage in &report.coverage {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "{:?}: old {}/{}, new {}/{} source references compared; {}",
            coverage.channel,
            coverage.old_compared_sources,
            coverage.old_discovered_sources,
            coverage.new_compared_sources,
            coverage.new_discovered_sources,
            if coverage.complete {
                "complete"
            } else {
                "unresolved"
            }
        );
        if coverage.old_presence_sources > 0 || coverage.new_presence_sources > 0 {
            let _ = writeln!(
                text,
                "  Native field membership accounts for {} old and {} new additional references.",
                coverage.old_presence_sources, coverage.new_presence_sources
            );
        }
    }
    crate::evidence_text::append_details(
        &mut text,
        &report.comparison,
        &old_graph,
        &new_graph,
        &old.issues,
        &new.issues,
    );
    if let Some(flow) = options.flow_text {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "Text channel (flow): {}; {} detected changes; {} formatting-only changes",
            if flow.text_complete() {
                "complete"
            } else {
                "incomplete"
            },
            flow.non_layout_changes(),
            flow.formatting.len()
        );
        if exit_basis == Some("detected_with_unexamined_evidence") {
            let note = match flow_exit.and_then(|exit| exit.justifying) {
                Some(crate::flow_exit::ChannelClass::Text) | None => {
                    crate::flow_cmd::DETECTED_WITH_UNEXAMINED_NOTE.to_owned()
                }
                Some(channel) => format!(
                    "difference detected by {}, but some evidence was not examined (non-text paint or visual evidence)",
                    channel.label()
                ),
            };
            let _ = writeln!(text, "{note}");
        }
    }
    if let Some(images) = &report.image_diff {
        use std::fmt::Write as _;
        let _ = writeln!(
            text,
            "Image diff (pixel hashes): {} unchanged, {} changes; {}",
            images.comparison.unchanged,
            images.comparison.changes.len(),
            if images.comparison.complete {
                "image inventory compared"
            } else {
                "unresolved images or acquisition"
            }
        );
        for change in images.comparison.changes.iter().take(200) {
            let location = |inventory: &ImageInventory, index: Option<usize>| {
                index.map_or_else(
                    || "absent".into(),
                    |i| {
                        format!(
                            "page {} image {}",
                            inventory.images[i].page.0 + 1,
                            inventory.images[i].occurrence + 1
                        )
                    },
                )
            };
            let _ = writeln!(
                text,
                "  Image {:?}: {} -> {}",
                change.kind,
                location(&images.old, change.old),
                location(&images.new, change.new)
            );
        }
        if images.comparison.changes.len() > 200 {
            let _ = writeln!(
                text,
                "  Image change list truncated; full results are in JSON."
            );
        }
        let _ = writeln!(
            text,
            "  Unresolved image occurrences: old {}, new {}. Placement, clipping and vector graphics are outside pixel-hash comparison.",
            images.comparison.unresolved_old.len(),
            images.comparison.unresolved_new.len()
        );
        for (side, inventory) in [("old", &images.old), ("new", &images.new)] {
            for reason in inventory.issues.iter().take(20) {
                let _ = writeln!(text, "  {side}: {reason}");
            }
        }
    }
    // Piped output defaults to `auto`, so it stays plain unless the caller
    // explicitly requested colour or stdout is a terminal. A file has no
    // terminal to inspect, so only `always` styles it, matching the
    // unified-diff report's destination rule.
    let color = if output.output_path.is_some() {
        output.color == ColorChoice::Always
    } else {
        resolve_color(output.color)
    };
    let text = if color {
        crate::flow_cmd::paint_report(&text)
    } else {
        text
    };
    if let Some(path) = output.output_path {
        write_text_report_atomically(path, &text).map_err(DiffError::backend)?;
    } else if !output.quiet {
        std::io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(|error| DiffError::backend(error.to_string()))?;
    }
    let code = flow_exit.map_or_else(
        || if complete { u8::from(changes > 0) } else { 3 },
        |exit| exit.code,
    );
    Ok((
        code,
        !complete,
        crate::render::RenderCpuReport {
            old: old_render_cpu,
            new: new_render_cpu,
        },
        crate::image_hashes::ImageCpuReport {
            old: old_image_cpu,
            new: new_image_cpu,
        },
    ))
}

/// Channels that reported a typed difference under their own channel.
///
/// A contribution can belong only to one channel: text changes come from flow
/// (or, if present, a text local comparison), value operations from forms,
/// pixel operations from visual, relationship changes from relations, and keyed
/// element operations by their raw key domain. Page image hashes are inferred
/// changes, not typed detections, so they do not mark the visual channel as
/// detected.
fn comparison_detected_channels(
    comparison: &DocumentViewComparison,
    old_graph: &DocumentGraph,
    new_graph: &DocumentGraph,
    flow: Option<&crate::flow_cmd::FlowComparison>,
) -> BTreeSet<Channel> {
    let mut detected = BTreeSet::new();
    if flow.is_some_and(|flow| flow.non_layout_changes() > 0) {
        detected.insert(Channel::Text);
    }
    let content_channel = |graph: &DocumentGraph, id: NodeId| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == id)
            .and_then(|node| match &node.content {
                NodeContent::Text { .. } => Some(Channel::Text),
                NodeContent::Value { .. } => Some(Channel::Forms),
                NodeContent::Visual { .. } => Some(Channel::Visual),
                NodeContent::Container | NodeContent::Unknown => None,
            })
    };
    for pair in comparison.comparisons() {
        if pair.operation.is_none()
            || pair.interpretation != InterpretationStatus::ConditionalOnCorrespondence
        {
            continue;
        }
        if let Some(channel) = pair
            .old
            .iter()
            .find_map(|node| content_channel(old_graph, *node))
            .or_else(|| {
                pair.new
                    .iter()
                    .find_map(|node| content_channel(new_graph, *node))
            })
        {
            detected.insert(channel);
        }
    }
    for relation in comparison.relations() {
        if relation.changed()
            && relation.interpretation == InterpretationStatus::ConditionalOnCorrespondence
        {
            detected.insert(Channel::Relations);
        }
    }
    if let Some(keys) = &comparison.key_presence {
        for operation in comparison.keyed_element_operations() {
            if let Some(claim) = keys.claims.get(operation.claim) {
                match claim.domain {
                    KeyDomain::PdfFieldName => {
                        detected.insert(Channel::Forms);
                    }
                    KeyDomain::PdfStructureId => {
                        detected.insert(Channel::Relations);
                    }
                }
            }
        }
    }
    detected
}

/// The composited page rasters this run retained, ready to publish.
///
/// These are the samples the comparison examined, in the profile the renderer
/// declared; nothing is re-rendered for the bundle.
fn page_rasters<'a>(
    old: &'a EvidenceStore,
    new: &'a EvidenceStore,
) -> Vec<crate::agent_review::PageRaster<'a>> {
    use pdfdelta_core::review::Side;

    let mut rasters = Vec::new();
    for (side, store) in [(Side::Old, old), (Side::New, new)] {
        for region in store
            .rendered
            .iter()
            .filter(|region| region.composited_page)
        {
            let backend = store.backends.get(region.backend).map_or_else(
                || "unknown".to_owned(),
                |backend| format!("{}/{}/{}", backend.name, backend.version, backend.profile),
            );
            rasters.push(crate::agent_review::PageRaster {
                side,
                page: region.page,
                page_bounds: store
                    .pages
                    .iter()
                    .find(|page| page.page == region.page)
                    .and_then(|page| page.bounds)
                    .map(|bounds| [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]),
                width: region.raster.width,
                height: region.raster.height,
                rgb: &region.raster.rgb,
                backend,
            });
        }
    }
    rasters
}

/// The engine's own verdict, copied for the bundle without recomputation.
fn engine_outcome(report: &DocumentReport<'_>) -> pdfdelta_core::review::EngineOutcome {
    use pdfdelta_core::review::{EngineOutcome, EngineStatus};

    EngineOutcome {
        status: if report.comparison_complete {
            if report.typed_changes > 0 {
                EngineStatus::CompleteChanged
            } else {
                EngineStatus::CompleteUnchanged
            }
        } else {
            EngineStatus::Incomplete
        },
        comparison_complete: report.comparison_complete,
        typed_changes: report.typed_changes,
        inferred_changes: report.inferred_changes,
        scope_content_changes: report.scope_content_changes,
        inferred_scope_changes: report.inferred_scope_changes,
    }
}

/// Binds a bundle to its inputs, its evidence selection, and this run's
/// evidence snapshot. Wall-clock time and durations are deliberately excluded.
fn bundle_identity(
    old_bytes: &[u8],
    new_bytes: &[u8],
    old: &EvidenceStore,
    new: &EvidenceStore,
    channels: &BTreeSet<Channel>,
    limit_scale: f64,
) -> pdfdelta_core::review::BundleIdentity {
    use sha2::{Digest, Sha256};

    let digest = |bytes: &[u8]| crate::fs::lowercase_hex(&Sha256::digest(bytes));
    let backend = |identity: &BackendIdentity| {
        format!(
            "{}/{}/{}",
            identity.name, identity.version, identity.profile
        )
    };
    pdfdelta_core::review::BundleIdentity {
        policy_version: pdfdelta_core::review::POLICY_VERSION,
        schema: pdfdelta_core::review::REVIEW_SCHEMA.into(),
        old_sha256: digest(old_bytes),
        new_sha256: digest(new_bytes),
        old_bytes: old_bytes.len(),
        new_bytes: new_bytes.len(),
        options: format!(
            "channels={};limit_scale={limit_scale}",
            channels
                .iter()
                .map(|channel| format!("{channel:?}"))
                .collect::<Vec<_>>()
                .join(",")
        ),
        old_revision: old.revision.clone(),
        new_revision: new.revision.clone(),
        backends: old
            .backends
            .iter()
            .chain(&new.backends)
            .map(backend)
            .collect(),
        pipeline: pdfdelta_core::review::PipelineContract::SharedEvidence,
        selected_channels: channels.clone(),
    }
}

type CollectedEvidence = (
    EvidenceStore,
    Option<Arc<[u8]>>,
    Option<ImageInventory>,
    crate::image_hashes::ImageCpuSummary,
    crate::render::RenderCpuSummary,
);

fn collect(
    input: ComparisonInput<'_>,
    cache_dir: Option<&Path>,
    options: &EvidenceOptions,
    retain_input: bool,
    limit_scale: f64,
) -> Result<CollectedEvidence, DiffError> {
    let parse_limits = ParseLimits::default();
    let limits = EvidenceLimits::default();
    let bytes = read_limited_typed(input.path, parse_limits.max_input_bytes).map_err(|error| {
        input_read_diff_error(error, "PDF input bytes", parse_limits.max_input_bytes)
    })?;
    let password = input
        .password_file
        .map(read_password_file_typed)
        .transpose()
        .map_err(|error| {
            input_read_diff_error(error, "password file bytes", MAX_PASSWORD_FILE_BYTES)
        })?;
    let (mut store, page_refs) = crate::native_worker::collect(
        &bytes,
        password.as_deref(),
        input.font_identities,
        cache_dir,
        &options.channels,
    )?;
    let mut image_cpu = crate::image_hashes::ImageCpuSummary::default();
    let mut images = options.channels.contains(&Channel::Visual).then(|| {
        let (inventory, cpu) =
            crate::image_hashes::collect(&bytes, &page_refs, password.is_some(), limit_scale);
        image_cpu = cpu;
        inventory
    });
    if let Some(images) = &mut images
        && store
            .issues
            .iter()
            .any(|issue| issue.channel == Channel::Text)
    {
        images.complete = false;
        images.issues.push(
            "native acquisition has unresolved content; image discovery is not certified".into(),
        );
    }
    let mut render_cpu = crate::render::RenderCpuSummary::default();
    if options.channels.contains(&Channel::Text)
        || options.channels.contains(&Channel::Visual)
        || options.channels.contains(&Channel::Presentation)
        || store.structured.iter().any(|field| matches!(&field.value, pdfdelta_core::document::StructuredValue::FormField { widgets, .. } if !widgets.is_empty()))
    {
        render_cpu = crate::render::collect(
            &mut store,
            &bytes,
            &page_refs,
            password.is_some(),
            limit_scale,
        );
    }
    crate::widgets::collect(&mut store);
    for channel in [Channel::Presentation] {
        if options.channels.contains(&channel) {
            for page in &store.pages {
                store.issues.push(EvidenceIssue {
                    boundary: None,
                    page: Some(page.page),
                    channel,
                    sources: Vec::new(),
                    kind: EvidenceFailure::Unsupported,
                    reason: "presentation interpretation is not implemented; retained page pixels do not establish presentation coverage"
                        .into(),
                });
            }
        }
    }
    store.validate(limits).map_err(DiffError::from)?;
    Ok((
        store,
        retain_input.then_some(bytes),
        images,
        image_cpu,
        render_cpu,
    ))
}
