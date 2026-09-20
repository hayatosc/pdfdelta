use std::{collections::BTreeSet, io::Write as _, path::Path, sync::Arc};

use pdfdelta_core::document::image_diff::{ImageDiff, ImageInventory, compare_images};
use pdfdelta_core::{
    document::{
        BackendIdentity, Channel, ChannelCoverage, ComparisonContract, CorrespondenceScope,
        DocumentComparisonLimits, DocumentGraph, DocumentView, DocumentViewComparison,
        EvidenceFailure, EvidenceIssue, EvidenceLimits, EvidenceStore, HierarchyLimits,
        InterpretationStatus, MatchingChannels, MatchingLimits, NodeId, TableRefinements,
        compare_document_views, document_coverage, refine_table_views,
    },
    model::PageId,
    pdf::ParseLimits,
    pipeline::PipelineOptions,
};
use serde::Serialize;

use crate::{
    args::{ComparisonInput, ComparisonOptions},
    fs::{
        read_limited_typed, read_password_file, write_output_atomically,
        write_text_report_atomically,
    },
    trace::ExecutionTrace,
};

pub struct EvidenceOptions {
    pub channels: BTreeSet<Channel>,
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
    options: &EvidenceOptions,
    trace: &mut ExecutionTrace,
) -> Result<(u8, bool), String> {
    let started = std::time::Instant::now();
    let (old, old_bytes, old_images) =
        collect(old_input, cache_dir, options, output.review_dir.is_some())?;
    let (new, new_bytes, new_images) =
        collect(new_input, cache_dir, options, output.review_dir.is_some())?;
    let image_diff = match (old_images, new_images) {
        (Some(old), Some(new)) => {
            let comparison = compare_images(&old, &new).map_err(|error| error.to_string())?;
            Some(ImageReport {
                old,
                new,
                comparison,
            })
        }
        _ => None,
    };
    let limits = DocumentComparisonLimits {
        matching: MatchingLimits {
            channels: MatchingChannels::from(&options.channels),
            ..MatchingLimits::default()
        },
        // Page rasters remain review context, not a second diff of native text.
        visual: pdfdelta_core::document::VisualCandidateLimits {
            include_composited_pages: false,
            ..Default::default()
        },
        ..DocumentComparisonLimits::default()
    };
    let mut old_graph = DocumentGraph::from_evidence(&old, pipeline, limits.evidence, limits.graph)
        .map_err(|error| error.to_string())?;
    let mut new_graph = DocumentGraph::from_evidence(&new, pipeline, limits.evidence, limits.graph)
        .map_err(|error| error.to_string())?;
    let table_refinements = if limits.matching.channels.text {
        refine_table_views(&mut old_graph, &mut new_graph, &old, &new, pipeline, limits)
            .map_err(|error| error.to_string())?
    } else {
        TableRefinements {
            old: Vec::new(),
            new: Vec::new(),
            exhaustive: true,
        }
    };
    trace.complete(
        "document_graph",
        None,
        [
            ("old_nodes", old_graph.nodes.len()),
            ("new_nodes", new_graph.nodes.len()),
        ],
    );
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
    .map_err(|error| error.to_string())?;
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
    let coverage = document_coverage(
        DocumentView {
            evidence: &old,
            graph: &old_graph,
        },
        DocumentView {
            evidence: &new,
            graph: &new_graph,
        },
        &comparison,
        &options.channels,
    );
    let complete = coverage.iter().all(|channel| channel.complete) && comparison.search_resolved();
    let changes = comparison
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
    let report = DocumentReport {
        schema_version: 2,
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
        })?;
    }
    if let Some(directory) = output.review_dir {
        let (Some(old_bytes), Some(new_bytes)) = (old_bytes, new_bytes) else {
            return Err("review source bytes were not retained".into());
        };
        crate::review::write(
            directory,
            &report,
            &report.comparison,
            report.image_diff.as_ref(),
            complete,
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
    if let Some(path) = output.output_path {
        write_text_report_atomically(path, &text)?;
    } else if !output.quiet {
        std::io::stdout()
            .lock()
            .write_all(text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    trace.complete(
        "document_comparison",
        None,
        [
            ("typed_changes", changes),
            ("inferred_changes", inferred_changes),
            ("selected_channels", report.coverage.len()),
        ],
    );
    Ok((if !complete { 3 } else { u8::from(changes > 0) }, !complete))
}

type CollectedEvidence = (EvidenceStore, Option<Arc<[u8]>>, Option<ImageInventory>);

fn collect(
    input: ComparisonInput<'_>,
    cache_dir: Option<&Path>,
    options: &EvidenceOptions,
    retain_input: bool,
) -> Result<CollectedEvidence, String> {
    let parse_limits = ParseLimits::default();
    let limits = EvidenceLimits::default();
    let bytes = read_limited_typed(input.path, parse_limits.max_input_bytes)
        .map_err(|error| error.to_string())?;
    let password = input.password_file.map(read_password_file).transpose()?;
    let (mut store, page_refs) = crate::native_worker::collect(
        &bytes,
        password.as_deref(),
        input.font_identities,
        cache_dir,
        &options.channels,
    )?;
    let mut images = options
        .channels
        .contains(&Channel::Visual)
        .then(|| crate::image_hashes::collect(&bytes, &page_refs, password.is_some()));
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
    if options.channels.contains(&Channel::Text)
        || options.channels.contains(&Channel::Visual)
        || options.channels.contains(&Channel::Presentation)
        || store.structured.iter().any(|field| matches!(&field.value, pdfdelta_core::document::StructuredValue::FormField { widgets, .. } if !widgets.is_empty()))
    {
        crate::render::collect(&mut store, &bytes, &page_refs, password.is_some());
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
    store.validate(limits).map_err(|error| error.to_string())?;
    Ok((store, retain_input.then_some(bytes), images))
}
