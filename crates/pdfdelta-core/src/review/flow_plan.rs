//! Projection of a verified flow token-stream comparison into review cases.
//!
//! Flow establishes a token-level edit script, not a glyph-mask alignment
//! result. Its cases therefore partition the flow report's own change array:
//! every non-layout change and every formatting entry belongs to exactly one
//! case, and the case records the report indexes it covers.
//!
//! Result classes map the script's evidence honestly:
//!
//! * a delete, insert, or high-confidence replace is [`EngineClass::Strict`];
//! * a replace whose span is coarse is [`EngineClass::NonOwningRange`], because
//!   the correspondence covers a range whose internal edit was not localized;
//! * a move, or any change whose assessment reports ambiguity, is
//!   [`EngineClass::Inferred`], because the chosen pairing is one of several;
//! * evidence that extraction never acquired is [`EngineClass::Unavailable`].
//!
//! Candidate enumeration is always [`Completeness::Unknown`]: flow produces
//! one verified script and does not enumerate competing hypotheses. The solver
//! observation records only that the script search finished and verified; it
//! is never presented as a closed alternative universe.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use super::{
    BundleIdentity, CaseCompleteness, CaseFinding, CaseRegion, Completeness, EngineClass,
    EngineOutcome, EvidenceRef, GapScope, Hypothesis, PlannerLimits, ReasonRecord, RetrievalAction,
    ReviewAssumption, ReviewCase, ReviewPlan, ReviewQuestion, ReviewReason, ReviewText,
    ScalarInterval, Side, SideLocation, TextOrder, UnlocalizedGap,
    case::{OmissionReason, OmittedRun},
    planner::{Assembly, Budget, CaseKey, Identifiers, assemble, evidence_ref, retention_boundary},
};
use crate::{
    document::Channel,
    model::{GlyphId, PageId, Rect},
};

/// The edit operation flow selected for one change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowChangeKind {
    Delete,
    Insert,
    Replace,
    Move,
}

impl FlowChangeKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Insert => "insert",
            Self::Replace => "replace",
            Self::Move => "move",
        }
    }
}

/// Flow's own confidence in the chosen script for one change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowConfidence {
    High,
    Medium,
    Low,
}

/// A cheap ambiguity signal flow recorded for one change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FlowAmbiguity {
    CoarseSpan,
    RepeatedRun,
    OrderUncertain,
    ShortMove,
    NumericOnly,
    NumericTable,
}

/// One flow token, projected without copying its text.
#[derive(Clone, Copy, Debug)]
pub struct FlowTokenView<'a> {
    pub text: &'a str,
    pub page: PageId,
    pub bounds: Option<Rect>,
    /// The token encodes an unmapped glyph identity rather than decoded text.
    pub uncertain: bool,
    /// Extracted glyph identifiers, empty when the run did not retain sources.
    pub sources: &'a [GlyphId],
}

/// One non-layout flow change, projected from the verified script.
#[derive(Clone, Debug)]
pub struct FlowChangeView {
    /// Index in the flow report's `changes` array (layout-only excluded).
    pub index: usize,
    pub kind: FlowChangeKind,
    pub old: Range<usize>,
    pub new: Range<usize>,
    pub confidence: FlowConfidence,
    pub ambiguity: Vec<FlowAmbiguity>,
}

/// One formatting-only entry on matched flow tokens.
#[derive(Clone, Debug)]
pub struct FlowFormattingView {
    /// Index in the flow report's `formatting_only_changes` array.
    pub index: usize,
    pub old: Range<usize>,
    pub new: Range<usize>,
    pub reasons: Vec<&'static str>,
    pub old_size: Option<f64>,
    pub new_size: Option<f64>,
}

/// How one flow extraction issue classified evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowIssueKind {
    Unsupported,
    Unresolved,
}

/// An extraction issue flow retained for one side.
#[derive(Clone, Debug)]
pub struct FlowIssueView {
    pub kind: FlowIssueKind,
    pub page: Option<PageId>,
    /// The issue means evidence was not examined, not that examined evidence is
    /// untrustworthy. Paint-gate and image-only-page issues are unexamined.
    pub unexamined: bool,
    pub message: String,
}

/// Per-page reading-order evidence retained by flow.
#[derive(Clone, Copy, Debug)]
pub struct FlowPageOrderView {
    pub page: PageId,
    /// Layout and paint order disagreed, so the order is an interpretation.
    pub uncertain: bool,
    pub agreement: f64,
    pub coherence: f64,
}

/// One side of a flow comparison, ready for projection.
#[derive(Clone, Copy, Debug)]
pub struct FlowSideView<'a> {
    pub tokens: &'a [FlowTokenView<'a>],
    pub issues: &'a [FlowIssueView],
    pub order: &'a [FlowPageOrderView],
    /// Every comparison-visible glyph is referenced by a token or fallback.
    pub conservation_incomplete: bool,
}

impl FlowSideView<'_> {
    /// Extraction and glyph conservation closed with no retained issue.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.issues.is_empty() && !self.conservation_incomplete
    }

    fn broken(&self) -> bool {
        self.issues.iter().any(|issue| !issue.unexamined) || self.conservation_incomplete
    }

    fn unexamined(&self) -> bool {
        self.issues.iter().any(|issue| issue.unexamined)
    }
}

/// A verified flow comparison, projected without the comparison types.
#[derive(Clone, Copy, Debug)]
pub struct FlowTextReview<'a> {
    pub old: FlowSideView<'a>,
    pub new: FlowSideView<'a>,
    pub changes: &'a [FlowChangeView],
    pub formatting: &'a [FlowFormattingView],
    /// The script replayed and covered every token exactly once.
    pub diff_verified: bool,
}

/// A flow-only comparison ready to become a native-text bundle.
pub struct FlowNativeReview<'a> {
    pub identity: BundleIdentity,
    pub outcome: EngineOutcome,
    pub flow: FlowTextReview<'a>,
}

/// Plans and assembles a flow-only bundle.
///
/// The manifest records the `NativeText` pipeline because no non-text evidence
/// was selected; the cases still carry flow's own change indexes.
#[must_use]
pub fn plan_flow_native(input: &FlowNativeReview<'_>, limits: PlannerLimits) -> ReviewPlan {
    let mut budget = Budget::new(limits);
    let mut identifiers = Identifiers::new();
    let mut cases = Vec::new();
    let mut gaps = Vec::new();
    plan_flow_into(FlowPlanContext {
        flow: &input.flow,
        identity: &input.identity,
        channels: &BTreeSet::from([Channel::Text]),
        visual_available: false,
        limits,
        budget: &mut budget,
        identifiers: &mut identifiers,
        cases: &mut cases,
        gaps: &mut gaps,
    });
    budget.finalize();
    assemble(
        Assembly {
            contexts: Vec::new(),
            identity: input.identity.clone(),
            outcome: input.outcome.clone(),
            channels: &BTreeSet::from([Channel::Text]),
            inventory_gaps: Vec::new(),
            unlocalized_gaps: gaps,
            cases,
            visual_available: false,
        },
        &budget,
    )
}

/// Adds flow cases and gaps to a plan that already projected shared evidence.
///
/// The function never returns early on a source budget: coverage grouping
/// happens before any optional source is visited, and a case whose references
/// were truncated still accounts for its changes.
pub(super) struct FlowPlanContext<'a, 'b> {
    pub flow: &'b FlowTextReview<'a>,
    pub identity: &'b BundleIdentity,
    pub channels: &'b BTreeSet<Channel>,
    pub visual_available: bool,
    pub limits: PlannerLimits,
    pub budget: &'b mut Budget,
    pub identifiers: &'b mut Identifiers,
    pub cases: &'b mut Vec<ReviewCase>,
    pub gaps: &'b mut Vec<UnlocalizedGap>,
}

pub(super) fn plan_flow_into(mut context: FlowPlanContext<'_, '_>) {
    // Evidence that was not examined or cannot be trusted is planned first:
    // its cases are few compared with the change clusters, and every such
    // reason must remain nameable even when the change coverage has to merge.
    flow_evidence_cases(&mut context);
    let groups = flow_groups(context.flow, context.limits, context.cases.len());
    for group in groups {
        if let Some(case) = build_flow_case(&mut context, &group) {
            // Groups were bounded by the remaining case budget before any
            // case was built, so acceptance cannot fail for the first flow
            // case. The call still charges the shared export budget.
            if context.budget.accept_case() {
                context.cases.push(case);
            }
        }
    }
}

/// One planned group of report entries.
struct Group {
    class: EngineClass,
    anchor_page: PageId,
    content: Vec<usize>,
    formatting: Vec<usize>,
    old: Option<Range<usize>>,
    new: Option<Range<usize>>,
    order_uncertain: bool,
    ambiguities: BTreeSet<FlowAmbiguity>,
    kind_counts: BTreeMap<&'static str, usize>,
    formatting_reasons: BTreeSet<&'static str>,
}

impl Group {
    fn new(class: EngineClass, anchor_page: PageId) -> Self {
        Self {
            class,
            anchor_page,
            content: Vec::new(),
            formatting: Vec::new(),
            old: None,
            new: None,
            order_uncertain: false,
            ambiguities: BTreeSet::new(),
            kind_counts: BTreeMap::new(),
            formatting_reasons: BTreeSet::new(),
        }
    }

    fn absorb(&mut self, other: Group) {
        self.content.extend(other.content);
        self.formatting.extend(other.formatting);
        extend_range(&mut self.old, other.old);
        extend_range(&mut self.new, other.new);
        self.order_uncertain |= other.order_uncertain;
        self.ambiguities.extend(other.ambiguities);
        for (kind, count) in other.kind_counts {
            *self.kind_counts.entry(kind).or_default() += count;
        }
        self.formatting_reasons.extend(other.formatting_reasons);
        // Merging can only weaken a class: a group that contains inferred
        // material must not remain strict.
        self.class = weaker_class(self.class, other.class);
    }
}

fn weaker_class(left: EngineClass, right: EngineClass) -> EngineClass {
    let rank = |class| match class {
        EngineClass::Strict => 0,
        EngineClass::NonOwningRange => 1,
        EngineClass::Inferred => 2,
        EngineClass::Unavailable => 3,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

fn extend_range(target: &mut Option<Range<usize>>, next: Option<Range<usize>>) {
    let Some(next) = next else { return };
    *target = Some(match target.take() {
        Some(current) => current.start.min(next.start)..current.end.max(next.end),
        None => next,
    });
}

fn class_of(change: &FlowChangeView) -> EngineClass {
    if change.kind == FlowChangeKind::Move {
        return EngineClass::Inferred;
    }
    if change.ambiguity.contains(&FlowAmbiguity::CoarseSpan) {
        return EngineClass::NonOwningRange;
    }
    if change.confidence == FlowConfidence::High {
        EngineClass::Strict
    } else {
        EngineClass::Inferred
    }
}

fn anchor(change: &FlowChangeView, flow: &FlowTextReview<'_>) -> (Side, PageId) {
    if !change.old.is_empty() {
        let page = flow
            .old
            .tokens
            .get(change.old.start)
            .map_or(PageId(0), |token| token.page);
        (Side::Old, page)
    } else if let Some(token) = flow.new.tokens.get(change.new.start) {
        (Side::New, token.page)
    } else {
        (Side::New, PageId(0))
    }
}

fn group_key(class: EngineClass, side: Side, page: PageId, formatting: bool) -> GroupKey {
    GroupKey {
        class,
        side,
        page,
        formatting,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    class: EngineClass,
    side: Side,
    page: PageId,
    formatting: bool,
}

/// Maximum token gap that keeps two events in one decision cluster.
///
/// The gap is a structural boundary for a page-sized decision, not a tuned
/// content threshold: 32 tokens is about one text line at ordinary sizes.
const CLUSTER_GAP_TOKENS: usize = 32;
/// Maximum report entries in one cluster before a new case starts.
const CLUSTER_MAX_EVENTS: usize = 64;

struct Event {
    key: GroupKey,
    start: usize,
    end: usize,
    old: Option<Range<usize>>,
    new: Option<Range<usize>>,
    content: Option<usize>,
    formatting: Option<usize>,
    order_uncertain: bool,
    ambiguities: Vec<FlowAmbiguity>,
    kind: Option<&'static str>,
    formatting_reasons: Vec<&'static str>,
}

impl Event {
    fn absorb(self, group: &mut Group) {
        if let Some(index) = self.content {
            group.content.push(index);
        }
        if let Some(index) = self.formatting {
            group.formatting.push(index);
        }
        extend_range(&mut group.old, self.old);
        extend_range(&mut group.new, self.new);
        group.order_uncertain |= self.order_uncertain;
        group.ambiguities.extend(self.ambiguities);
        if let Some(kind) = self.kind {
            *group.kind_counts.entry(kind).or_default() += 1;
        }
        group.formatting_reasons.extend(self.formatting_reasons);
    }
}

/// Groups report entries into page-sized decision clusters.
///
/// Events are first clustered by class, anchor side, page, kind, and token
/// proximity. If that produces more groups than the remaining case budget,
/// groups are merged by page, then by class, then globally. Merging weakens a
/// result class rather than dropping an event.
fn flow_groups(
    flow: &FlowTextReview<'_>,
    limits: PlannerLimits,
    existing_cases: usize,
) -> Vec<Group> {
    let mut events = Vec::new();
    for change in flow.changes {
        let class = class_of(change);
        let (side, page) = anchor(change, flow);
        let (start, end) = if side == Side::Old {
            (change.old.start, change.old.end)
        } else {
            (change.new.start, change.new.end)
        };
        events.push(Event {
            key: group_key(class, side, page, false),
            start,
            end,
            old: (!change.old.is_empty()).then(|| change.old.clone()),
            new: (!change.new.is_empty()).then(|| change.new.clone()),
            content: Some(change.index),
            formatting: None,
            order_uncertain: change.ambiguity.contains(&FlowAmbiguity::OrderUncertain),
            ambiguities: change.ambiguity.clone(),
            kind: Some(change.kind.label()),
            formatting_reasons: Vec::new(),
        });
    }
    for entry in flow.formatting {
        let page = flow
            .old
            .tokens
            .get(entry.old.start)
            .map_or(PageId(0), |token| token.page);
        events.push(Event {
            key: group_key(EngineClass::Strict, Side::Old, page, true),
            start: entry.old.start,
            end: entry.old.end,
            old: (!entry.old.is_empty()).then(|| entry.old.clone()),
            new: (!entry.new.is_empty()).then(|| entry.new.clone()),
            content: None,
            formatting: Some(entry.index),
            order_uncertain: false,
            ambiguities: Vec::new(),
            kind: None,
            formatting_reasons: entry.reasons.clone(),
        });
    }
    events.sort_by_key(|event| (event.key, event.start, event.end));
    let mut groups: Vec<Group> = Vec::new();
    let mut current: Option<(GroupKey, Group, usize, usize)> = None;
    for event in events {
        let continues = current.as_ref().is_some_and(|(key, _, last_end, count)| {
            *key == event.key
                && event.start.saturating_sub(*last_end) <= CLUSTER_GAP_TOKENS
                && *count < CLUSTER_MAX_EVENTS
        });
        if continues {
            let (_, group, last_end, count) = current.as_mut().expect("continues implies current");
            *last_end = (*last_end).max(event.end);
            *count += 1;
            event.absorb(group);
        } else {
            if let Some((_, group, _, _)) = current.take() {
                groups.push(group);
            }
            let mut group = Group::new(event.key.class, event.key.page);
            let key = event.key;
            let end = event.end;
            event.absorb(&mut group);
            current = Some((key, group, end, 1));
        }
    }
    if let Some((_, group, _, _)) = current {
        groups.push(group);
    }
    let remaining = limits.max_cases.saturating_sub(existing_cases);
    if groups.len() <= remaining {
        return groups;
    }
    // Merge by page/class/kind first, which is the smallest honest widening.
    let mut by_page: BTreeMap<GroupKey, Group> = BTreeMap::new();
    for group in groups {
        // The cluster key is not retained on the group; recover the page and
        // kind from the first event's ranges by merging all groups that share
        // a class and are either content or formatting.
        let key = GroupKey {
            class: group.class,
            side: Side::Old,
            page: group.anchor_page,
            formatting: !group.formatting.is_empty() && group.content.is_empty(),
        };
        match by_page.entry(key) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().absorb(group);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(group);
            }
        }
    }
    groups = by_page.into_values().collect();
    if groups.len() <= remaining {
        return groups;
    }
    // Then merge across pages within a class and kind.
    let mut by_class: BTreeMap<(EngineClass, bool), Group> = BTreeMap::new();
    for group in groups {
        let formatting = !group.formatting.is_empty() && group.content.is_empty();
        match by_class.entry((group.class, formatting)) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                entry.get_mut().absorb(group);
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(group);
            }
        }
    }
    groups = by_class.into_values().collect();
    while groups.len() > remaining && groups.len() > 1 {
        let second = groups.remove(1);
        groups[0].absorb(second);
    }
    groups
}

/// Builds one case for a group of flow report entries.
fn build_flow_case(context: &mut FlowPlanContext<'_, '_>, group: &Group) -> Option<ReviewCase> {
    let flow = context.flow;
    let marking = !group.content.is_empty();
    let question = ReviewQuestion::CompareContent;
    let class = group.class;
    let finding = CaseFinding::DifferenceEstablished;
    let mut key = CaseKey::new(question);
    key.debug(&class);
    key.debug(&finding);
    key.debug(&group.content);
    key.debug(&group.formatting);
    key.debug(&group.old);
    key.debug(&group.new);
    let digest = key.finish();
    let case_id = context.identifiers.case(&digest);
    let quote = || RetrievalAction::Show {
        case: case_id.clone(),
        detail: super::Detail::Quote,
        cursor: None,
    };
    let old_text = group.old.as_ref().and_then(|range| {
        flow_text(
            Side::Old,
            flow.old.tokens,
            range,
            &case_id,
            context.budget,
            context.limits,
        )
    });
    let new_text = group.new.as_ref().and_then(|range| {
        flow_text(
            Side::New,
            flow.new.tokens,
            range,
            &case_id,
            context.budget,
            context.limits,
        )
    });
    let readable = old_text.as_ref().is_some_and(|text| !text.text.is_empty())
        || new_text.as_ref().is_some_and(|text| !text.text.is_empty());
    let mut reasons = Vec::new();
    if marking {
        let mut detail = group
            .kind_counts
            .iter()
            .map(|(kind, count)| format!("{kind}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        detail.push_str(&format!(" (flow changes {:?})", group.content));
        reasons.push(
            ReasonRecord::new(ReviewReason::FlowTextChange)
                .with_message(detail)
                .with_channel(Channel::Text),
        );
    }
    if !group.formatting.is_empty() {
        let detail = group
            .formatting_reasons
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        reasons.push(
            ReasonRecord::new(ReviewReason::FlowFormattingChange)
                .with_message(detail)
                .with_channel(Channel::Text),
        );
    }
    match class {
        EngineClass::NonOwningRange => {
            reasons.push(ReasonRecord::new(ReviewReason::NonOwningRangeOnly));
        }
        EngineClass::Inferred => {
            reasons.push(ReasonRecord::new(ReviewReason::InferredCorrespondence));
        }
        EngineClass::Strict | EngineClass::Unavailable => {}
    }
    for ambiguity in &group.ambiguities {
        match ambiguity {
            FlowAmbiguity::OrderUncertain => {
                reasons.push(ReasonRecord::new(ReviewReason::UnknownReadingOrder));
            }
            FlowAmbiguity::CoarseSpan
            | FlowAmbiguity::RepeatedRun
            | FlowAmbiguity::ShortMove
            | FlowAmbiguity::NumericOnly
            | FlowAmbiguity::NumericTable => {
                reasons.push(ReasonRecord::new(ReviewReason::AmbiguousEditLocation));
            }
        }
    }
    let evidence: Vec<EvidenceRef> = old_text
        .as_ref()
        .into_iter()
        .flat_map(|text| text.sources.iter().copied())
        .chain(
            new_text
                .as_ref()
                .into_iter()
                .flat_map(|text| text.sources.iter().copied()),
        )
        .collect();
    let regions = group
        .old
        .as_ref()
        .map(|range| flow_regions(Side::Old, flow.old.tokens, range))
        .unwrap_or_default()
        .into_iter()
        .chain(
            group
                .new
                .as_ref()
                .map(|range| flow_regions(Side::New, flow.new.tokens, range))
                .unwrap_or_default(),
        )
        .collect::<Vec<_>>();
    let old = group
        .old
        .as_ref()
        .and_then(|range| flow.tokens_page(Side::Old, range.start))
        .map(|page| SideLocation::unknown(Side::Old).with_page(Some(page)));
    let new = group
        .new
        .as_ref()
        .and_then(|range| flow.tokens_page(Side::New, range.start))
        .map(|page| SideLocation::unknown(Side::New).with_page(Some(page)));
    let required_evidence = if readable {
        Vec::new()
    } else if context.visual_available {
        vec![super::RequiredEvidence::Visual]
    } else {
        vec![super::RequiredEvidence::Unavailable]
    };
    let mut available_actions = vec![RetrievalAction::Show {
        case: case_id.clone(),
        detail: super::Detail::Text,
        cursor: None,
    }];
    if readable {
        available_actions.push(quote());
    }
    Some(ReviewCase {
        case_id,
        content_digest: digest,
        question,
        pipeline: context.identity.pipeline,
        engine_class: class,
        finding,
        channels: context.channels.clone(),
        completeness: CaseCompleteness {
            evidence: Completeness::from_flag(flow.old.complete() && flow.new.complete()),
            // Flow returns one verified script; it enumerates no alternative
            // correspondence universe.
            candidate_enumeration: Completeness::Unknown,
            solver_search: Completeness::from_flag(flow.diff_verified),
            response: Completeness::Complete,
        },
        reasons,
        assumptions: vec![
            ReviewAssumption::InputReadingOrder,
            ReviewAssumption::AcceptedCorrespondence,
        ],
        old,
        new,
        old_text,
        new_text,
        hypotheses: Vec::<Hypothesis>::new(),
        alternatives_total: None,
        alternatives_returned: 0,
        omitted: Vec::new(),
        next_cursor: None,
        required_evidence,
        related_cases: Vec::new(),
        conflicts_with: Vec::new(),
        available_actions,
        evidence,
        covered: Vec::new(),
        regions,
        flow_evidence: {
            let mut items = group
                .content
                .iter()
                .map(|index| super::FlowEvidence {
                    kind: super::FlowEvidenceKind::Content,
                    index: *index,
                })
                .collect::<Vec<_>>();
            items.extend(group.formatting.iter().map(|index| super::FlowEvidence {
                kind: super::FlowEvidenceKind::Formatting,
                index: *index,
            }));
            items
        },
    })
}

/// Quotes one token range, with the flow token text as the retained evidence.
fn flow_text(
    side: Side,
    tokens: &[FlowTokenView<'_>],
    range: &Range<usize>,
    case: &super::CaseId,
    budget: &mut Budget,
    limits: PlannerLimits,
) -> Option<ReviewText> {
    let selected = tokens.get(range.clone())?;
    if selected.is_empty() {
        return None;
    }
    let mut text = String::new();
    let mut unmapped = Vec::new();
    let mut sources = BTreeSet::new();
    let mut sources_omitted = false;
    // Track the scalar position incrementally. Re-counting the accumulated
    // string per token is quadratic on a page-sized range.
    let mut scalar_position = 0usize;
    for (position, token) in selected.iter().enumerate() {
        if position > 0 {
            text.push(' ');
            scalar_position += 1;
        }
        if token.uncertain {
            unmapped.push(super::UnmappedMark {
                position: scalar_position,
                raw_codes: Vec::new(),
                sources: token
                    .sources
                    .iter()
                    .take(limits.max_sources_per_case)
                    .map(|glyph| {
                        evidence_ref(side, crate::document::SourceRef::Native { glyph: *glyph })
                    })
                    .collect(),
            });
        }
        text.push_str(token.text);
        scalar_position += token.text.chars().count();
        // A page-sized range can reference far more glyphs than a case may
        // list. Stop growing the set at the limit instead of materializing
        // every source and truncating afterwards.
        for glyph in token.sources {
            let reference =
                evidence_ref(side, crate::document::SourceRef::Native { glyph: *glyph });
            if sources.len() < limits.max_sources_per_case {
                sources.insert(reference);
            } else {
                sources_omitted = true;
            }
        }
    }
    let scalars: Vec<char> = text.chars().collect();
    let scalar_count = scalars.len();
    let retained = retention_boundary(&scalars, limits.max_text_scalars).unwrap_or(0);
    let mut omitted = Vec::new();
    if retained < scalar_count {
        omitted.push(OmittedRun {
            range: ScalarInterval::new(retained, scalar_count),
            scalars: scalar_count - retained,
            reason: OmissionReason::ExportBudget,
            expand_with: Some(RetrievalAction::Show {
                case: case.clone(),
                detail: super::Detail::Text,
                cursor: None,
            }),
        });
    }
    if sources_omitted {
        // The exact omitted count is unknown because the set stopped
        // inserting at the limit; declaring it unknown is more honest than
        // counting duplicates.
        budget.stop(
            super::OmissionScope::Case { case: case.clone() },
            super::OmissionKind::GapSourcesOmitted,
            None,
        );
    }
    let retained_sources = sources
        .into_iter()
        .filter(|reference| reference.side == side)
        .collect::<Vec<_>>();
    Some(ReviewText {
        side,
        text: scalars[..retained].iter().collect(),
        scalar_count,
        order: TextOrder::InferredReadingOrder,
        unmapped: unmapped
            .into_iter()
            .filter(|mark| mark.position <= retained)
            .collect(),
        omitted,
        token_range: Some(super::TokenInterval {
            start: range.start,
            end: range.end,
        }),
        canonical_range: None,
        sources: retained_sources,
    })
}

/// Page regions covered by a token range, with the union of token geometry.
fn flow_regions(side: Side, tokens: &[FlowTokenView<'_>], range: &Range<usize>) -> Vec<CaseRegion> {
    let mut pages: BTreeMap<PageId, Option<[f64; 4]>> = BTreeMap::new();
    for token in tokens.get(range.clone()).unwrap_or_default() {
        let entry = pages.entry(token.page).or_insert(None);
        if let Some(bounds) = token.bounds {
            let next = [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y];
            *entry = Some(match *entry {
                Some(current) => [
                    current[0].min(next[0]),
                    current[1].min(next[1]),
                    current[2].max(next[2]),
                    current[3].max(next[3]),
                ],
                None => next,
            });
        }
    }
    pages
        .into_iter()
        .map(|(page, bounds)| CaseRegion {
            side,
            page_number: page.0.saturating_add(1),
            page_index: page,
            bounds,
        })
        .collect()
}

trait TokenPage {
    fn tokens_page(&self, side: Side, index: usize) -> Option<PageId>;
}

impl TokenPage for FlowTextReview<'_> {
    fn tokens_page(&self, side: Side, index: usize) -> Option<PageId> {
        match side {
            Side::Old => self.old.tokens.get(index).map(|token| token.page),
            Side::New => self.new.tokens.get(index).map(|token| token.page),
        }
    }
}

/// Cases and gaps for material flow did not examine or could not trust.
fn flow_evidence_cases(context: &mut FlowPlanContext<'_, '_>) {
    let flow = context.flow;
    for (side, view) in [(Side::Old, &flow.old), (Side::New, &flow.new)] {
        for issue in view.issues {
            let reason = match issue.kind {
                FlowIssueKind::Unsupported => ReviewReason::UnsupportedChannel,
                FlowIssueKind::Unresolved => {
                    if issue.unexamined {
                        ReviewReason::DiscoveredButUnexamined
                    } else {
                        ReviewReason::ExtractionGap
                    }
                }
            };
            let Some(page) = issue.page else {
                // A document-scoped issue names no material to quote. It is a
                // channel gap rather than a case that could only repeat itself.
                let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
                key.field(side.label().as_bytes());
                key.field(issue.message.as_bytes());
                let gap_id = context.identifiers.gap(&key.finish());
                context.gaps.push(UnlocalizedGap {
                    gap_id,
                    scope: GapScope::Channel,
                    channel: Some(Channel::Text),
                    side: Some(side),
                    evidence_complete: Completeness::Incomplete,
                    reasons: vec![
                        ReasonRecord::new(reason)
                            .with_message(issue.message.clone())
                            .with_channel(Channel::Text),
                    ],
                    sources: Vec::new(),
                });
                continue;
            };
            let page_range = page_range(view.tokens, page);
            let old_range = (side == Side::Old).then(|| page_range.clone());
            let new_range = (side == Side::New).then(|| page_range.clone());
            let case = evidence_case(
                context,
                EvidenceCase {
                    side,
                    page,
                    old_range,
                    new_range,
                    tokens: view.tokens,
                    reason,
                    issue,
                },
            );
            if let Some(case) = case
                && context.budget.accept_case()
            {
                context.cases.push(case);
            }
        }
        if view.conservation_incomplete {
            let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
            key.field(b"conservation");
            key.field(side.label().as_bytes());
            let gap_id = context.identifiers.gap(&key.finish());
            context.gaps.push(UnlocalizedGap {
                gap_id,
                scope: GapScope::Document,
                channel: Some(Channel::Text),
                side: Some(side),
                evidence_complete: Completeness::Incomplete,
                reasons: vec![
                    ReasonRecord::new(ReviewReason::ExtractionGap)
                        .with_message("glyph conservation is incomplete")
                        .with_channel(Channel::Text),
                ],
                sources: Vec::new(),
            });
        }
        for order in view.order.iter().filter(|order| order.uncertain) {
            let range = page_range(view.tokens, order.page);
            let mut key = CaseKey::new(ReviewQuestion::CompareContent);
            key.field(b"flow-order");
            key.field(side.label().as_bytes());
            key.debug(&order.page);
            let digest = key.finish();
            let case_id = context.identifiers.case(&digest);
            let text = flow_text(
                side,
                view.tokens,
                &range,
                &case_id,
                context.budget,
                context.limits,
            );
            let old_text = (side == Side::Old).then(|| text.clone()).flatten();
            let new_text = (side == Side::New).then(|| text.clone()).flatten();
            let evidence = text
                .as_ref()
                .map(|text| text.sources.clone())
                .unwrap_or_default();
            let regions = flow_regions(side, view.tokens, &range);
            let location = SideLocation::unknown(side).with_page(Some(order.page));
            let case = ReviewCase {
                case_id: case_id.clone(),
                content_digest: digest,
                question: ReviewQuestion::CompareContent,
                pipeline: context.identity.pipeline,
                engine_class: EngineClass::Unavailable,
                finding: CaseFinding::NotEstablished,
                channels: context.channels.clone(),
                completeness: CaseCompleteness {
                    evidence: Completeness::from_flag(view.complete()),
                    candidate_enumeration: Completeness::Unknown,
                    solver_search: Completeness::Incomplete,
                    response: Completeness::Complete,
                },
                reasons: vec![
                    ReasonRecord::new(ReviewReason::UnknownReadingOrder)
                        .with_message(format!(
                            "layout and paint order disagree; agreement {:.3}, coherence {:.3}",
                            order.agreement, order.coherence
                        ))
                        .with_channel(Channel::Text)
                        .with_page(Some(order.page)),
                ],
                assumptions: vec![ReviewAssumption::InputReadingOrder],
                old: (side == Side::Old).then(|| location.clone()),
                new: (side == Side::New).then_some(location),
                old_text,
                new_text,
                hypotheses: Vec::new(),
                alternatives_total: None,
                alternatives_returned: 0,
                omitted: Vec::new(),
                next_cursor: None,
                required_evidence: vec![super::RequiredEvidence::Unavailable],
                related_cases: Vec::new(),
                conflicts_with: Vec::new(),
                available_actions: vec![RetrievalAction::Show {
                    case: case_id.clone(),
                    detail: super::Detail::Text,
                    cursor: None,
                }],
                evidence,
                covered: Vec::new(),
                regions,
                flow_evidence: Vec::new(),
            };
            if context.budget.accept_case() {
                context.cases.push(case);
            }
        }
        // A broken or unexamined text channel must always leave something a
        // reviewer can name, even when the individual issue had no page.
        if view.broken() || view.unexamined() {
            let covered = context.cases.iter().any(|case| {
                case.reasons
                    .iter()
                    .any(|reason| reason.channel == Some(Channel::Text) && reason.page.is_some())
            }) || context
                .gaps
                .iter()
                .any(|gap| gap.channel == Some(Channel::Text) && gap.side == Some(side));
            if !covered {
                let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
                key.field(b"flow-text-channel");
                key.field(side.label().as_bytes());
                let gap_id = context.identifiers.gap(&key.finish());
                context.gaps.push(UnlocalizedGap {
                    gap_id,
                    scope: GapScope::Channel,
                    channel: Some(Channel::Text),
                    side: Some(side),
                    evidence_complete: Completeness::Incomplete,
                    reasons: vec![
                        ReasonRecord::new(if view.broken() {
                            ReviewReason::ExtractionGap
                        } else {
                            ReviewReason::DiscoveredButUnexamined
                        })
                        .with_message("flow text evidence is not complete")
                        .with_channel(Channel::Text),
                    ],
                    sources: Vec::new(),
                });
            }
        }
    }
}

struct EvidenceCase<'a> {
    side: Side,
    page: PageId,
    old_range: Option<Range<usize>>,
    new_range: Option<Range<usize>>,
    tokens: &'a [FlowTokenView<'a>],
    reason: ReviewReason,
    issue: &'a FlowIssueView,
}

fn evidence_case(
    context: &mut FlowPlanContext<'_, '_>,
    case: EvidenceCase<'_>,
) -> Option<ReviewCase> {
    let EvidenceCase {
        side,
        page,
        old_range,
        new_range,
        tokens,
        reason,
        issue,
    } = case;
    let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
    key.field(side.label().as_bytes());
    key.debug(&page);
    key.field(issue.message.as_bytes());
    let digest = key.finish();
    let case_id = context.identifiers.case(&digest);
    let text = old_range.as_ref().or(new_range.as_ref()).and_then(|range| {
        flow_text(
            side,
            tokens,
            range,
            &case_id,
            context.budget,
            context.limits,
        )
    });
    let (old_text, new_text) = match side {
        Side::Old => (text.clone(), None),
        Side::New => (None, text.clone()),
    };
    let evidence = text
        .as_ref()
        .map(|text| text.sources.clone())
        .unwrap_or_default();
    let mut regions = Vec::new();
    if let Some(range) = old_range.as_ref().or(new_range.as_ref()) {
        regions = flow_regions(side, tokens, range);
    }
    if regions.is_empty() {
        regions.push(CaseRegion {
            side,
            page_number: page.0.saturating_add(1),
            page_index: page,
            bounds: None,
        });
    }
    let location = SideLocation::unknown(side).with_page(Some(page));
    let required_evidence = if text.as_ref().is_some_and(|text| !text.text.is_empty()) {
        Vec::new()
    } else if context.visual_available {
        vec![super::RequiredEvidence::Visual]
    } else {
        vec![super::RequiredEvidence::Unavailable]
    };
    Some(ReviewCase {
        case_id: case_id.clone(),
        content_digest: digest,
        question: ReviewQuestion::AcquisitionGap,
        pipeline: context.identity.pipeline,
        engine_class: EngineClass::Unavailable,
        finding: if issue.unexamined {
            CaseFinding::NotExamined
        } else {
            CaseFinding::NotEstablished
        },
        channels: context.channels.clone(),
        completeness: CaseCompleteness {
            evidence: Completeness::Incomplete,
            candidate_enumeration: Completeness::Unknown,
            solver_search: Completeness::Unknown,
            response: Completeness::Complete,
        },
        reasons: vec![
            ReasonRecord::new(reason)
                .with_message(issue.message.clone())
                .with_channel(Channel::Text)
                .with_page(Some(page)),
        ],
        assumptions: vec![ReviewAssumption::InputReadingOrder],
        old: (side == Side::Old).then(|| location.clone()),
        new: (side == Side::New).then_some(location),
        old_text,
        new_text,
        hypotheses: Vec::new(),
        alternatives_total: None,
        alternatives_returned: 0,
        omitted: Vec::new(),
        next_cursor: None,
        required_evidence,
        related_cases: Vec::new(),
        conflicts_with: Vec::new(),
        available_actions: vec![RetrievalAction::Show {
            case: case_id,
            detail: super::Detail::Text,
            cursor: None,
        }],
        evidence,
        covered: Vec::new(),
        regions,
        flow_evidence: Vec::new(),
    })
}

/// Token indexes on one page, as a contiguous range when the page is contiguous.
fn page_range(tokens: &[FlowTokenView<'_>], page: PageId) -> Range<usize> {
    let mut start = None;
    let mut end = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if token.page == page {
            start.get_or_insert(index);
            end = index + 1;
        }
    }
    start.map_or(0..0, |start| start..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{
        BundleIdentity, EngineStatus, FlowEvidenceKind, PipelineContract, ReviewReason,
    };

    fn identity() -> BundleIdentity {
        BundleIdentity {
            policy_version: 1,
            schema: "agent-review/v1".into(),
            old_sha256: "old".into(),
            new_sha256: "new".into(),
            old_bytes: 3,
            new_bytes: 3,
            options: "native_text_only".into(),
            old_revision: "flow-test".into(),
            new_revision: "flow-test".into(),
            backends: Vec::new(),
            pipeline: PipelineContract::NativeText,
            selected_channels: BTreeSet::from([Channel::Text]),
        }
    }

    fn outcome() -> EngineOutcome {
        EngineOutcome {
            status: EngineStatus::CompleteChanged,
            comparison_complete: true,
            typed_changes: 3,
            inferred_changes: 0,
            scope_content_changes: 0,
            inferred_scope_changes: 0,
        }
    }

    fn token(text: &str, page: u32) -> FlowTokenView<'_> {
        FlowTokenView {
            text,
            page: PageId(page),
            bounds: None,
            uncertain: false,
            sources: &[],
        }
    }

    fn change(
        index: usize,
        kind: FlowChangeKind,
        old: Range<usize>,
        new: Range<usize>,
        confidence: FlowConfidence,
        ambiguity: Vec<FlowAmbiguity>,
    ) -> FlowChangeView {
        FlowChangeView {
            index,
            kind,
            old,
            new,
            confidence,
            ambiguity,
        }
    }

    fn plan(flow: &FlowTextReview<'_>, limits: PlannerLimits) -> ReviewPlan {
        plan_flow_native(
            &FlowNativeReview {
                identity: identity(),
                outcome: outcome(),
                flow: *flow,
            },
            limits,
        )
    }

    #[test]
    fn every_change_and_formatting_entry_is_covered_exactly_once() {
        let old = [token("a", 0), token("b", 0), token("c", 1)];
        let new = [token("a", 0), token("x", 0), token("c", 1)];
        let changes = vec![
            change(
                0,
                FlowChangeKind::Delete,
                1..2,
                1..1,
                FlowConfidence::High,
                Vec::new(),
            ),
            change(
                1,
                FlowChangeKind::Move,
                2..3,
                2..3,
                FlowConfidence::High,
                Vec::new(),
            ),
        ];
        let formatting = vec![FlowFormattingView {
            index: 0,
            old: 0..1,
            new: 0..1,
            reasons: vec!["font_size"],
            old_size: Some(10.0),
            new_size: Some(12.0),
        }];
        let flow = FlowTextReview {
            old: FlowSideView {
                tokens: &old,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            new: FlowSideView {
                tokens: &new,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            changes: &changes,
            formatting: &formatting,
            diff_verified: true,
        };
        let plan = plan(&flow, PlannerLimits::default());
        let mut content = BTreeSet::new();
        let mut formatting_seen = BTreeSet::new();
        for case in &plan.cases {
            for evidence in &case.flow_evidence {
                match evidence.kind {
                    FlowEvidenceKind::Content => {
                        assert!(content.insert(evidence.index), "duplicate content coverage");
                    }
                    FlowEvidenceKind::Formatting => {
                        assert!(
                            formatting_seen.insert(evidence.index),
                            "duplicate formatting coverage"
                        );
                    }
                }
            }
        }
        assert_eq!(content, BTreeSet::from([0, 1]));
        assert_eq!(formatting_seen, BTreeSet::from([0]));
        assert_eq!(
            plan.cases
                .iter()
                .filter(|case| !case.flow_evidence.is_empty())
                .count(),
            3,
            "one strict page case, one inferred move case, one formatting case"
        );
    }

    #[test]
    fn result_classes_map_confidence_and_ambiguity_honestly() {
        let tokens = [token("a", 0), token("b", 0)];
        let changes = vec![
            change(
                0,
                FlowChangeKind::Replace,
                0..1,
                0..1,
                FlowConfidence::High,
                Vec::new(),
            ),
            change(
                1,
                FlowChangeKind::Replace,
                1..2,
                1..2,
                FlowConfidence::Low,
                vec![FlowAmbiguity::CoarseSpan],
            ),
            change(
                2,
                FlowChangeKind::Move,
                0..1,
                1..2,
                FlowConfidence::High,
                Vec::new(),
            ),
        ];
        let formatting = Vec::new();
        let flow = FlowTextReview {
            old: FlowSideView {
                tokens: &tokens,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            new: FlowSideView {
                tokens: &tokens,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            changes: &changes,
            formatting: &formatting,
            diff_verified: true,
        };
        let plan = plan(&flow, PlannerLimits::default());
        let class_for = |index: usize| {
            plan.cases
                .iter()
                .find(|case| {
                    case.flow_evidence
                        .iter()
                        .any(|evidence| evidence.index == index)
                })
                .map(|case| case.engine_class)
                .expect("case for change")
        };
        assert_eq!(class_for(0), EngineClass::Strict);
        assert_eq!(class_for(1), EngineClass::NonOwningRange);
        assert_eq!(class_for(2), EngineClass::Inferred);
        for case in &plan.cases {
            assert_eq!(
                case.completeness.candidate_enumeration,
                Completeness::Unknown
            );
        }
    }

    #[test]
    fn unexamined_broken_and_uncertain_evidence_leaves_cases_or_gaps() {
        let old = [token("page", 1)];
        let new = [token("page", 1)];
        let old_issues = [
            FlowIssueView {
                kind: FlowIssueKind::Unresolved,
                page: Some(PageId(1)),
                unexamined: true,
                message:
                    "page has a potentially visible image and no comparison-visible native glyphs"
                        .into(),
            },
            FlowIssueView {
                kind: FlowIssueKind::Unresolved,
                page: None,
                unexamined: false,
                message: "document extraction stopped".into(),
            },
        ];
        let order = [FlowPageOrderView {
            page: PageId(1),
            uncertain: true,
            agreement: 0.2,
            coherence: 0.3,
        }];
        let flow = FlowTextReview {
            old: FlowSideView {
                tokens: &old,
                issues: &old_issues,
                order: &order,
                conservation_incomplete: true,
            },
            new: FlowSideView {
                tokens: &new,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            changes: &[],
            formatting: &[],
            diff_verified: true,
        };
        let plan = plan(&flow, PlannerLimits::default());
        assert!(plan.cases.iter().any(|case| {
            case.reasons
                .iter()
                .any(|reason| reason.reason == ReviewReason::DiscoveredButUnexamined)
        }));
        assert!(plan.cases.iter().any(|case| {
            case.reasons
                .iter()
                .any(|reason| reason.reason == ReviewReason::UnknownReadingOrder)
        }));
        assert!(plan.manifest.unlocalized_gaps.iter().any(|gap| {
            gap.channel == Some(Channel::Text)
                && gap
                    .reasons
                    .iter()
                    .any(|reason| reason.reason == ReviewReason::ExtractionGap)
        }));
        // The unexamined page issue is a located case, so no duplicate channel
        // gap is invented for it.
        assert!(!plan.manifest.unlocalized_gaps.iter().any(|gap| {
            gap.reasons
                .iter()
                .any(|reason| reason.reason == ReviewReason::DiscoveredButUnexamined)
        }));
        assert!(plan.cases.iter().all(|case| !case.reasons.is_empty()));
    }

    #[test]
    fn the_case_budget_merges_groups_instead_of_dropping_coverage() {
        let old = [token("a", 0), token("b", 1), token("c", 2)];
        let new = [token("x", 0), token("y", 1), token("z", 2)];
        let changes = vec![
            change(
                0,
                FlowChangeKind::Replace,
                0..1,
                0..1,
                FlowConfidence::High,
                Vec::new(),
            ),
            change(
                1,
                FlowChangeKind::Move,
                1..2,
                1..2,
                FlowConfidence::High,
                Vec::new(),
            ),
            change(
                2,
                FlowChangeKind::Replace,
                2..3,
                2..3,
                FlowConfidence::Low,
                vec![FlowAmbiguity::CoarseSpan],
            ),
        ];
        let flow = FlowTextReview {
            old: FlowSideView {
                tokens: &old,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            new: FlowSideView {
                tokens: &new,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            changes: &changes,
            formatting: &[],
            diff_verified: true,
        };
        let limits = PlannerLimits {
            max_cases: 2,
            ..PlannerLimits::default()
        };
        let plan = plan(&flow, limits);
        assert!(plan.cases.len() <= 2);
        let covered: BTreeSet<_> = plan
            .cases
            .iter()
            .flat_map(|case| case.flow_evidence.iter())
            .filter(|evidence| evidence.kind == FlowEvidenceKind::Content)
            .map(|evidence| evidence.index)
            .collect();
        assert_eq!(covered, BTreeSet::from([0, 1, 2]));
        assert!(plan.manifest.export.export_complete);
    }

    #[test]
    fn native_flow_manifest_names_the_flow_contract() {
        let tokens = [token("a", 0)];
        let flow = FlowTextReview {
            old: FlowSideView {
                tokens: &tokens,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            new: FlowSideView {
                tokens: &tokens,
                issues: &[],
                order: &[],
                conservation_incomplete: false,
            },
            changes: &[],
            formatting: &[],
            diff_verified: true,
        };
        let plan = plan(&flow, PlannerLimits::default());
        assert_eq!(plan.manifest.pipeline, PipelineContract::NativeText);
        assert_eq!(
            plan.manifest.selected_channels,
            BTreeSet::from([Channel::Text])
        );
        let visual = plan
            .manifest
            .capabilities
            .iter()
            .find(|capability| capability.detail == super::super::Detail::Visual)
            .expect("visual capability is listed");
        assert!(!visual.available, "flow publishes no page rasters");
    }
}
