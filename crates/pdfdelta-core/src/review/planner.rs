//! Shared planning machinery: identifiers, aliases, text projection, budgets,
//! and final assembly of a bundle's manifest and cases.
//!
//! Planning is pure. It reads the comparison a run already produced and writes
//! no files, so the same inputs always produce the same plan and the engine's
//! result cannot be altered by exporting it.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::{
    AgentReviewManifest, BundleId, BundleIdentity, CaseCensus, CaseFinding, CaseId, Detail,
    EngineOutcome, EvidenceRef, ExportOmission, ExportStatus, GapId, InventoryGap,
    MAX_IDENTIFIER_BYTES, OmissionKind, OmissionScope, OmittedRun, QuestionCount, RetrievalAction,
    RetrievalCapability, ReviewCase, ReviewQuestion, ReviewText, ScalarInterval, Side, SourceAlias,
    TextOrder, UnlocalizedGap, UnmappedMark, case::OmissionReason, lowercase_hex,
};
use crate::{
    document::{Channel, SourceRef, TextView, ViewBasis},
    normalize::ComparableToken,
};

/// Default case budget for an agent review bundle.
///
/// A planned case costs a packet file and manifest entry. The default bounds a
/// single bundle well below the shared 512 MiB artifact budget on large
/// documents; a caller can raise it explicitly with the CLI option.
pub const DEFAULT_AGENT_CASE_BUDGET: usize = 4_096;

/// Bounds on the work a plan may perform and the size of what it retains.
///
/// Every limit is a stopping point, not a target: when one is reached the plan
/// records an explicit omission on the affected scope and reports
/// `export_complete: false`, which is separate from the comparison's own
/// completeness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlannerLimits {
    pub max_cases: usize,
    pub max_source_visits: usize,
    pub max_candidate_visits: usize,
    pub max_hypotheses_per_case: usize,
    /// Retained scalars per side of a case before the rest becomes an omission.
    pub max_text_scalars: usize,
    /// Retained sources per side of a case.
    pub max_sources_per_case: usize,
    /// Structural context items gathered per side of a case.
    pub max_labels_per_case: usize,
    /// Views one case quotes before the rest become an omission. Their sources
    /// still travel as evidence, so the obligation stays explained.
    pub max_members_per_case: usize,
    /// How far context gathering walks up the containment hierarchy.
    pub max_context_depth: usize,
}

impl Default for PlannerLimits {
    fn default() -> Self {
        Self {
            max_cases: DEFAULT_AGENT_CASE_BUDGET,
            max_source_visits: 1_000_000,
            max_candidate_visits: 100_000,
            max_hypotheses_per_case: 16,
            max_text_scalars: 4_096,
            max_sources_per_case: 4_096,
            max_labels_per_case: 16,
            max_members_per_case: 32,
            max_context_depth: 8,
        }
    }
}

/// A planned bundle: the index and the cases it indexes.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewPlan {
    pub manifest: AgentReviewManifest,
    pub cases: Vec<ReviewCase>,
    /// Surrounding structure per case, kept apart from the cases themselves so
    /// a caller pays for it only when it asks.
    pub contexts: Vec<super::CaseContext>,
}

impl ReviewPlan {
    /// Looks up one case by identifier.
    #[must_use]
    pub fn case(&self, case: &CaseId) -> Option<&ReviewCase> {
        self.cases.iter().find(|entry| &entry.case_id == case)
    }

    /// Looks up one case's gathered context.
    #[must_use]
    pub fn context(&self, case: &CaseId) -> Option<&super::CaseContext> {
        self.contexts.iter().find(|entry| &entry.case_id == case)
    }
}

/// A storage failure while a plan is being assembled.
///
/// Planning is otherwise a pure projection of the comparison. The store only
/// fails when the caller cannot publish the plan's temporary case bodies, so
/// the failure is reported instead of being folded into the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanError {
    message: String,
}

impl PlanError {
    /// Records a store failure with its own context.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// The failure text.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PlanError {}

/// Storage for case bodies while a plan is being assembled.
///
/// Case bodies are the part of a plan that grows with the number of cases.
/// Keeping them behind this interface lets the planner retain only a
/// fixed-size index, while a caller can put the bodies on disk. Handles are
/// opaque and only meaningful to the store that minted them.
pub trait CaseStore {
    /// Stores one case and its optional gathered context.
    ///
    /// The returned handle is used for later reads, evidence merges, and link
    /// attachment. It must remain valid until the store is dropped.
    fn store(
        &mut self,
        case: ReviewCase,
        context: Option<super::CaseContext>,
    ) -> Result<usize, PlanError>;

    /// Reads one stored case and its context.
    fn load(
        &mut self,
        handle: usize,
    ) -> Result<(ReviewCase, Option<super::CaseContext>), PlanError>;

    /// Appends evidence a duplicate case contributed to an existing case.
    ///
    /// References already present are not repeated, and the order of the
    /// existing list is preserved.
    fn append_evidence(&mut self, handle: usize, evidence: &[EvidenceRef])
    -> Result<(), PlanError>;
}

/// An in-memory [`CaseStore`], used by the whole-plan frontends and by tests.
#[derive(Default)]
pub struct MemoryCaseStore {
    records: Vec<(ReviewCase, Option<super::CaseContext>)>,
}

impl MemoryCaseStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of stored cases.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether no case has been stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl CaseStore for MemoryCaseStore {
    fn store(
        &mut self,
        case: ReviewCase,
        context: Option<super::CaseContext>,
    ) -> Result<usize, PlanError> {
        self.records.push((case, context));
        Ok(self.records.len() - 1)
    }

    fn load(
        &mut self,
        handle: usize,
    ) -> Result<(ReviewCase, Option<super::CaseContext>), PlanError> {
        self.records.get(handle).cloned().ok_or_else(|| {
            PlanError::new(format!("review case handle {handle} is outside the store"))
        })
    }

    fn append_evidence(
        &mut self,
        handle: usize,
        evidence: &[EvidenceRef],
    ) -> Result<(), PlanError> {
        let (case, _) = self.records.get_mut(handle).ok_or_else(|| {
            PlanError::new(format!("review case handle {handle} is outside the store"))
        })?;
        append_unique_evidence(&mut case.evidence, evidence);
        Ok(())
    }
}

/// One case in a streaming plan's final listing order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedCase {
    /// Store handle for the case body.
    pub handle: usize,
    pub case_id: CaseId,
    pub question: ReviewQuestion,
    pub finding: CaseFinding,
    pub old_page: Option<u32>,
    pub new_page: Option<u32>,
    /// Pages the case's regions cover, in the case's own region order.
    pub pages: Vec<(Side, crate::model::PageId)>,
    /// Links to cases that quote the same evidence.
    pub links: CaseLinks,
    /// Store handle for gathered context, when the case carries it.
    pub context: Option<usize>,
}

/// Related and conflicting cases for one listing record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaseLinks {
    pub related: Vec<CaseId>,
    pub conflicts: Vec<CaseId>,
}

/// A finished plan whose case bodies remain in a [`CaseStore`].
///
/// The manifest and the fixed-size ordering index are resident. The bodies and
/// their contexts are read back one at a time by the publisher, so memory does
/// not grow with the number of cases.
pub struct StreamingPlan {
    pub manifest: AgentReviewManifest,
    /// Cases in the listing order the manifest's index records use.
    pub order: Vec<OrderedCase>,
    /// Context handles in the order the planner gathered them.
    pub contexts: Vec<usize>,
}

/// Appends evidence references without repeating one already retained.
///
/// The existing order is preserved, so a merged duplicate cannot reorder the
/// evidence a reviewer reads.
fn append_unique_evidence(existing: &mut Vec<EvidenceRef>, additional: &[EvidenceRef]) {
    let mut retained: BTreeSet<EvidenceRef> = existing.iter().copied().collect();
    for reference in additional {
        if retained.insert(*reference) {
            existing.push(*reference);
        }
    }
}

/// Array lengths for each side and source origin, one past the largest id.
///
/// A source id is a dense ordinal assigned by the extractor, so a byte-sized
/// membership bitmap and a four-byte first-case slot are enough for the
/// material a document actually produced. Ids outside these bounds fall back
/// to an ordered map rather than allocating an unbounded array.
#[derive(Clone, Debug, Default)]
pub(super) struct SourceExtents {
    lengths: [[usize; 4]; 2],
}

impl SourceExtents {
    pub(super) fn new(lengths: [[usize; 4]; 2]) -> Self {
        Self { lengths }
    }
}

const fn side_index(side: Side) -> usize {
    match side {
        Side::Old => 0,
        Side::New => 1,
    }
}

const fn origin_index(source: SourceRef) -> usize {
    match source {
        SourceRef::Native { .. } => 0,
        SourceRef::NativeVector { .. } => 1,
        SourceRef::Rendered { .. } => 2,
        SourceRef::Structured { .. } => 3,
    }
}

const fn source_id(source: SourceRef) -> u64 {
    match source {
        SourceRef::Native { glyph } => glyph.0,
        SourceRef::NativeVector { line } => line.0,
        SourceRef::Rendered { region } => region,
        SourceRef::Structured { element } => element,
    }
}

/// Compact membership for source references a case has already quoted.
///
/// The old planner kept every explained reference in a tree, which grew by one
/// node per retained source. A bitmap uses one bit per possible source and the
/// same answer, so the residual search stays cheap while its memory stays
/// proportional to the evidence rather than to the number of cases.
pub(super) struct SourceMarks {
    bits: Vec<Vec<u64>>,
    extra: BTreeSet<(Side, SourceRef)>,
}

impl SourceMarks {
    pub(super) fn new(extents: &SourceExtents) -> Self {
        let mut bits = Vec::with_capacity(8);
        for side in 0..2 {
            for origin in 0..4 {
                let words = extents.lengths[side][origin].div_ceil(64);
                bits.push(vec![0; words]);
            }
        }
        Self {
            bits,
            extra: BTreeSet::new(),
        }
    }

    pub(super) fn insert(&mut self, side: Side, source: SourceRef) {
        let slot = side_index(side) * 4 + origin_index(source);
        let id = source_id(source);
        let Ok(id) = usize::try_from(id) else {
            self.extra.insert((side, source));
            return;
        };
        let Some(words) = self.bits.get_mut(slot) else {
            self.extra.insert((side, source));
            return;
        };
        let (word, bit) = (id / 64, id % 64);
        let Some(word) = words.get_mut(word) else {
            self.extra.insert((side, source));
            return;
        };
        *word |= 1_u64 << bit;
    }

    pub(super) fn contains(&self, side: Side, source: SourceRef) -> bool {
        let slot = side_index(side) * 4 + origin_index(source);
        let Ok(id) = usize::try_from(source_id(source)) else {
            return self.extra.contains(&(side, source));
        };
        let Some(words) = self.bits.get(slot) else {
            return self.extra.contains(&(side, source));
        };
        let (word, bit) = (id / 64, id % 64);
        words
            .get(word)
            .is_some_and(|word| word & (1_u64 << bit) != 0)
            || self.extra.contains(&(side, source))
    }

    pub(super) fn mark(&mut self, references: &[EvidenceRef]) {
        for reference in references {
            self.insert(reference.side, reference.source);
        }
    }
}

/// Accumulates cases in a store while keeping the planner's side effects small.
///
/// A store failure is retained rather than propagated through every planning
/// phase: the remaining projection is not observable once publication failed,
/// so phases stop storing and the first failure is returned at the end.
pub(super) struct CaseSink<'a> {
    store: &'a mut dyn CaseStore,
    error: Option<PlanError>,
    raw: Vec<usize>,
    /// Whether a stored case names a page-scoped text problem. Flow uses this
    /// to decide whether a broken text channel still needs a channel gap.
    text_page_case: bool,
}

impl<'a> CaseSink<'a> {
    pub(super) fn new(store: &'a mut dyn CaseStore) -> Self {
        Self {
            store,
            error: None,
            raw: Vec::new(),
            text_page_case: false,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.raw.len()
    }

    pub(super) fn failed(&self) -> bool {
        self.error.is_some()
    }

    pub(super) fn take_error(&mut self) -> Option<PlanError> {
        self.error.take()
    }

    /// Stores a case that is already finalized by its planner.
    pub(super) fn push_unique(
        &mut self,
        case: ReviewCase,
        context: Option<super::CaseContext>,
    ) -> Option<usize> {
        if self.failed() {
            return None;
        }
        if case
            .reasons
            .iter()
            .any(|reason| reason.channel == Some(Channel::Text) && reason.page.is_some())
        {
            self.text_page_case = true;
        }
        match self.store.store(case, context) {
            Ok(handle) => {
                self.raw.push(handle);
                Some(handle)
            }
            Err(error) => {
                self.error = Some(error);
                None
            }
        }
    }

    pub(super) fn append_evidence(&mut self, handle: usize, evidence: &[EvidenceRef]) {
        if self.failed() {
            return;
        }
        if let Err(error) = self.store.append_evidence(handle, evidence) {
            self.error = Some(error);
        }
    }

    pub(super) fn store(&mut self) -> &mut dyn CaseStore {
        self.store
    }

    pub(super) fn take_handles(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.raw)
    }

    pub(super) const fn has_text_page_case(&self) -> bool {
        self.text_page_case
    }
}

/// Finds shared sources without holding every source-to-case edge in memory.
///
/// Only sources quoted by more than one final case are retained in `shared`.
/// All other sources occupy one four-byte slot, which is the same order as the
/// evidence and does not depend on how many cases were planned.
pub(super) struct LinkTracker {
    first: Vec<Vec<u32>>,
    overflow_first: BTreeMap<(Side, SourceRef), u32>,
    shared: BTreeMap<(Side, SourceRef), Vec<u32>>,
}

impl LinkTracker {
    pub(super) fn new(extents: &SourceExtents) -> Self {
        let mut first = Vec::with_capacity(8);
        for side in 0..2 {
            for origin in 0..4 {
                first.push(vec![0; extents.lengths[side][origin]]);
            }
        }
        Self {
            first,
            overflow_first: BTreeMap::new(),
            shared: BTreeMap::new(),
        }
    }

    fn record(&mut self, generation: u32, case: &ReviewCase) {
        for reference in &case.evidence {
            let slot = side_index(reference.side) * 4 + origin_index(reference.source);
            let dense = usize::try_from(source_id(reference.source))
                .ok()
                .and_then(|id| self.first.get_mut(slot).and_then(|first| first.get_mut(id)));
            if let Some(entry) = dense {
                if *entry == 0 {
                    *entry = generation.saturating_add(1);
                } else {
                    let group = self
                        .shared
                        .entry((reference.side, reference.source))
                        .or_default();
                    if group.is_empty() {
                        group.push(entry.saturating_sub(1));
                    }
                    group.push(generation);
                }
                continue;
            }
            match self
                .overflow_first
                .get_mut(&(reference.side, reference.source))
            {
                Some(first) => {
                    let group = self
                        .shared
                        .entry((reference.side, reference.source))
                        .or_default();
                    if group.is_empty() {
                        group.push(*first);
                    }
                    group.push(generation);
                }
                None => {
                    self.overflow_first
                        .insert((reference.side, reference.source), generation);
                }
            }
        }
    }
}

/// Pages one case's regions cover, in their existing order.
pub(super) fn ordered_pages(case: &ReviewCase) -> Vec<(Side, crate::model::PageId)> {
    case.regions
        .iter()
        .map(|region| (region.side, region.page_index))
        .collect()
}

/// The sort key the listing uses, kept separate from its case body.
fn ordered_key(case: &OrderedCase) -> (u8, &'static str, Option<u32>, Option<u32>, &CaseId) {
    (
        case.finding.rank(),
        question_key(case.question),
        case.old_page,
        case.new_page,
        &case.case_id,
    )
}

const fn question_key(question: ReviewQuestion) -> &'static str {
    match question {
        ReviewQuestion::ResolveCorrespondence => "ResolveCorrespondence",
        ReviewQuestion::CompareContent => "CompareContent",
        ReviewQuestion::CompareRelationship => "CompareRelationship",
        ReviewQuestion::InterpretVisualRegion => "InterpretVisualRegion",
        ReviewQuestion::CheckValueAppearance => "CheckValueAppearance",
        ReviewQuestion::AcquisitionGap => "AcquisitionGap",
    }
}

/// Identifier of one consecutive part of a split case.
///
/// The parent identifier is already validated ASCII, and the fixed suffix keeps
/// the result inside the identifier limit. Part numbering is zero-based and is
/// never reused within one parent.
pub(super) fn split_case_id(parent: &CaseId, index: u32) -> CaseId {
    CaseId::new(format!("{}-p{index:04}", parent.as_str()))
        .expect("a split case identifier is valid")
}

/// The bundle-local alias for one source reference.
///
/// The alias is derived from the reference itself, so it is stable without an
/// allocation order and cannot collide within a side: each origin has its own
/// letter and each identifier is unique inside its origin.
#[must_use]
pub fn source_alias(source: SourceRef) -> SourceAlias {
    EvidenceRef::new(Side::Old, source).alias()
}

/// Pairs a source with the side it belongs to.
#[must_use]
pub const fn evidence_ref(side: Side, source: SourceRef) -> EvidenceRef {
    EvidenceRef::new(side, source)
}

/// Accumulates the canonical key a case identifier is derived from.
///
/// Fields are length-prefixed so that moving text across a field boundary
/// cannot produce the same key.
pub(super) struct CaseKey(Sha256);

impl CaseKey {
    pub(super) fn new(question: ReviewQuestion) -> Self {
        let mut key = Self(Sha256::new());
        key.field(format!("{question:?}").as_bytes());
        key
    }

    pub(super) fn field(&mut self, bytes: &[u8]) {
        self.0
            .update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.0.update(bytes);
    }

    pub(super) fn debug(&mut self, value: &impl std::fmt::Debug) {
        self.field(format!("{value:?}").as_bytes());
    }

    pub(super) fn sources(&mut self, side: Side, sources: &BTreeSet<SourceRef>) {
        self.field(side.label().as_bytes());
        for source in sources {
            self.debug(source);
        }
    }

    pub(super) fn finish(self) -> String {
        lowercase_hex(&self.0.finalize())
    }
}

/// Mints collision-free identifiers from content digests.
///
/// An identifier depends only on its own case, so an unrelated case appearing
/// or disappearing does not renumber the others. Identity is still bundle-local:
/// source identifiers are execution-local, so the same case text under another
/// engine revision is a different case.
pub(super) struct Identifiers {
    used: BTreeSet<String>,
}

impl Identifiers {
    pub(super) fn new() -> Self {
        Self {
            used: BTreeSet::new(),
        }
    }

    /// Derives `prefix + digest`, lengthening the digest until it is unique.
    fn mint(&mut self, prefix: char, digest: &str) -> String {
        let max_digest = MAX_IDENTIFIER_BYTES.saturating_sub(prefix.len_utf8());
        for length in [12, 20, 32, max_digest] {
            let length = length.min(digest.len()).min(max_digest);
            let candidate = format!("{prefix}{}", &digest[..length]);
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
        // Repeating digests are different observations with the same canonical
        // key; disambiguate by ordinal while staying inside the identifier
        // bound. The suffix is reserved before the digest prefix is taken.
        let mut ordinal = 1u32;
        loop {
            let suffix = format!("-{ordinal}");
            let keep = max_digest.saturating_sub(suffix.len()).min(digest.len());
            let candidate = format!("{prefix}{}{suffix}", &digest[..keep]);
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            ordinal = ordinal.saturating_add(1);
        }
    }

    /// # Panics
    /// Never: the minted text is hex with an ASCII prefix.
    pub(super) fn case(&mut self, digest: &str) -> CaseId {
        CaseId::new(self.mint('R', digest)).expect("a minted case identifier is valid")
    }

    /// # Panics
    /// Never: the minted text is hex with an ASCII prefix.
    pub(super) fn gap(&mut self, digest: &str) -> GapId {
        GapId::new(self.mint('G', digest)).expect("a minted gap identifier is valid")
    }
}

/// Tracks planning work against its limits and records what was left out.
pub(super) struct Budget {
    limits: PlannerLimits,
    source_visits: usize,
    candidate_visits: usize,
    cases: usize,
    /// Cases a phase tried to add after the case limit was reached.
    case_shortfall: usize,
    complete: bool,
    omissions: Vec<ExportOmission>,
}

impl Budget {
    pub(super) fn new(limits: PlannerLimits) -> Self {
        Self {
            limits,
            source_visits: 0,
            candidate_visits: 0,
            cases: 0,
            case_shortfall: 0,
            complete: true,
            omissions: Vec::new(),
        }
    }

    /// Charges source inspection; false once the budget is spent.
    pub(super) fn visit_sources(&mut self, count: usize) -> bool {
        self.source_visits = self.source_visits.saturating_add(count);
        if self.source_visits > self.limits.max_source_visits {
            self.stop(OmissionScope::Bundle, OmissionKind::CasesNotExported, None);
            return false;
        }
        true
    }

    /// Charges candidate inspection; false once the budget is spent.
    pub(super) fn visit_candidates(&mut self, count: usize) -> bool {
        self.candidate_visits = self.candidate_visits.saturating_add(count);
        if self.candidate_visits > self.limits.max_candidate_visits {
            self.stop(
                OmissionScope::Bundle,
                OmissionKind::AlternativesTruncated,
                None,
            );
            return false;
        }
        true
    }

    /// Reserves room for one more case; false once the budget is spent.
    pub(super) fn accept_case(&mut self) -> bool {
        if !self.case_budget_available() {
            self.record_case_shortfall(1);
            self.stop(OmissionScope::Bundle, OmissionKind::CasesNotExported, None);
            return false;
        }
        self.cases += 1;
        true
    }

    /// Whether another case may still be accepted.
    pub(super) fn case_budget_available(&self) -> bool {
        self.cases < self.limits.max_cases
    }

    /// Records cases a phase did not attempt because the budget was spent.
    ///
    /// Phases that can count their withheld cases before building them use
    /// this instead of constructing and discarding one case per omission.
    pub(super) fn record_case_shortfall(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        self.case_shortfall = self.case_shortfall.saturating_add(count);
        self.complete = false;
    }

    /// Makes the case-limit omission carry the number of cases it withheld.
    ///
    /// The planner knows this count only after every phase has tried to add
    /// its cases, so the bundle omission is finalized once, immediately before
    /// the manifest is assembled.
    pub(super) fn finalize(&mut self) {
        if self.case_shortfall == 0 {
            return;
        }
        self.complete = false;
        if let Some(existing) = self.omissions.iter_mut().find(|omission| {
            omission.scope == OmissionScope::Bundle
                && omission.kind == OmissionKind::CasesNotExported
        }) {
            existing.omitted = Some(
                existing
                    .omitted
                    .unwrap_or(0)
                    .saturating_add(self.case_shortfall),
            );
        } else {
            self.omissions.push(ExportOmission {
                scope: OmissionScope::Bundle,
                kind: OmissionKind::CasesNotExported,
                omitted: Some(self.case_shortfall),
                expand_with: None,
                reason: None,
            });
        }
    }

    /// Records an omission and marks the export incomplete.
    pub(super) fn stop(
        &mut self,
        scope: OmissionScope,
        kind: OmissionKind,
        omitted: Option<usize>,
    ) {
        self.complete = false;
        let omission = ExportOmission {
            scope,
            kind,
            omitted,
            expand_with: None,
            reason: None,
        };
        if !self.omissions.contains(&omission) {
            self.omissions.push(omission);
        }
    }

    /// Removes one case-scoped omission after its case was split.
    ///
    /// The split parts retain all material the original case held, so the
    /// original truncation no longer describes the export. A withheld part is
    /// reported separately through the case budget.
    pub(super) fn clear_case_omission(&mut self, case: &CaseId, kind: OmissionKind) {
        self.omissions.retain(|omission| {
            !(omission.kind == kind
                && matches!(&omission.scope, OmissionScope::Case { case: scoped } if scoped == case))
        });
        self.complete = self.omissions.is_empty() && self.case_shortfall == 0;
    }

    pub(super) fn status(&self, cases_planned: usize) -> ExportStatus {
        ExportStatus {
            export_complete: self.complete,
            source_visits: self.source_visits,
            source_visit_limit: self.limits.max_source_visits,
            candidate_visits: self.candidate_visits,
            candidate_visit_limit: self.limits.max_candidate_visits,
            cases_planned,
            case_limit: self.limits.max_cases,
            omissions: self.omissions.clone(),
        }
    }
}

/// How the scalars of a view were ordered, from the view's own basis.
#[must_use]
pub(super) fn text_order(basis: ViewBasis) -> TextOrder {
    match basis {
        ViewBasis::SourceStructure => TextOrder::DeclaredStructureOrder,
        ViewBasis::NativeLayout | ViewBasis::ReconstructedStructure => {
            TextOrder::InferredReadingOrder
        }
        // Recognized and modelled readings order their own output; neither the
        // painting order nor a declared structure order is established by them.
        ViewBasis::RenderedRegion | ViewBasis::Recognition { .. } | ViewBasis::Model { .. } => {
            TextOrder::Unknown
        }
    }
}

/// The last position at or before `budget` where retained text may end.
///
/// Sentence and clause boundaries are preferred, then whitespace. When no
/// boundary exists the caller withholds the whole run rather than cutting text
/// at an arbitrary scalar, because a cut inside a number, a negation, or a unit
/// changes what the reviewer reads.
pub(super) fn retention_boundary(scalars: &[char], budget: usize) -> Option<usize> {
    if scalars.len() <= budget {
        return Some(scalars.len());
    }
    let terminators = ['.', '!', '?', '。', '！', '？', '\n', ';', '；'];
    scalars[..budget]
        .iter()
        .rposition(|scalar| terminators.contains(scalar))
        .map(|position| position + 1)
        .or_else(|| {
            scalars[..budget]
                .iter()
                .rposition(|scalar| scalar.is_whitespace())
                .map(|position| position + 1)
        })
}

/// Shortens retained text to a scalar budget, recording what it withheld.
///
/// The cut lands on a sentence, clause, or whitespace boundary, never inside a
/// number, a negation, or a unit. When no boundary exists at or before the
/// budget the whole run is withheld rather than cut at an arbitrary scalar, so
/// a reviewer never reads a value that the document does not contain.
///
/// The withheld run keeps its interval, its length, and the action that
/// retrieves it.
#[must_use]
pub fn retain_text(
    text: &ReviewText,
    max_scalars: usize,
    expand: Option<RetrievalAction>,
) -> ReviewText {
    let scalars: Vec<char> = text.text.chars().collect();
    if scalars.len() <= max_scalars {
        return text.clone();
    }
    let retained = retention_boundary(&scalars, max_scalars).unwrap_or(0);
    let mut shortened = text.clone();
    shortened.text = scalars[..retained].iter().collect();
    shortened.unmapped.retain(|mark| mark.position <= retained);
    shortened.omitted.push(OmittedRun {
        range: ScalarInterval::new(retained, scalars.len()),
        scalars: scalars.len() - retained,
        reason: OmissionReason::ResponseBudget,
        expand_with: expand,
    });
    shortened
}

/// Replaces retained text with its location and its size.
///
/// The scalars stay in the bundle; what a caller receives is the interval, how
/// long it is, and the action that quotes it. The omission is recorded as a
/// detail-level exclusion rather than a budget cut, so a reader can tell a
/// response that ran out of room from one that deliberately declined to quote.
///
/// Text that is already empty is returned unchanged: an omission of nothing
/// would assert material the case does not hold.
#[must_use]
pub fn withhold_text(text: &ReviewText, expand: Option<RetrievalAction>) -> ReviewText {
    let scalars = text.text.chars().count();
    let mut withheld = text.clone();
    if scalars == 0 {
        return withheld;
    }
    withheld.text = String::new();
    withheld.unmapped = Vec::new();
    withheld.sources = Vec::new();
    withheld.omitted.push(OmittedRun {
        range: ScalarInterval::new(0, scalars),
        scalars,
        reason: OmissionReason::DetailLevel,
        expand_with: expand,
    });
    withheld
}

/// Projects one text view into retained review text.
///
/// Unmapped tokens keep their position and raw codes instead of being replaced
/// by a substitute character, and any withheld run keeps its interval, length,
/// and the action that retrieves it.
pub(super) fn review_text(
    side: Side,
    basis: ViewBasis,
    view: &TextView,
    case: Option<&CaseId>,
    limits: PlannerLimits,
) -> ReviewText {
    let mut scalars = Vec::new();
    let mut unmapped = Vec::new();
    let mut sources: BTreeMap<usize, BTreeSet<SourceRef>> = BTreeMap::new();
    for (index, token) in view.tokens.iter().enumerate() {
        let origins = view.origins.get(index).cloned().unwrap_or_default();
        match token {
            ComparableToken::Scalar(scalar) => {
                sources
                    .entry(scalars.len())
                    .or_default()
                    .extend(origins.iter().copied());
                scalars.push(*scalar);
            }
            ComparableToken::Unmapped { .. } => unmapped.push(UnmappedMark {
                position: scalars.len(),
                raw_codes: Vec::new(),
                sources: origins
                    .iter()
                    .map(|source| evidence_ref(side, *source))
                    .collect(),
            }),
        }
    }
    let scalar_count = scalars.len();
    let retained = retention_boundary(&scalars, limits.max_text_scalars).unwrap_or(0);
    let mut omitted = Vec::new();
    if retained < scalar_count {
        omitted.push(OmittedRun {
            range: ScalarInterval::new(retained, scalar_count),
            scalars: scalar_count - retained,
            reason: OmissionReason::ExportBudget,
            expand_with: case.map(|case| RetrievalAction::Show {
                case: case.clone(),
                detail: Detail::Text,
                cursor: None,
            }),
        });
    }
    let mut references = BTreeSet::new();
    for (_, origins) in sources.range(..retained) {
        references.extend(origins.iter().copied());
    }
    ReviewText {
        side,
        text: scalars[..retained].iter().collect(),
        scalar_count,
        order: text_order(basis),
        unmapped: unmapped
            .into_iter()
            .filter(|mark| mark.position <= retained)
            .collect(),
        omitted,
        token_range: None,
        canonical_range: None,
        sources: references
            .into_iter()
            .take(limits.max_sources_per_case)
            .map(|source| evidence_ref(side, source))
            .collect(),
    }
}

/// Chunks of source references that each respect a per-case cap.
fn source_chunks(sources: &[EvidenceRef], cap: usize) -> Vec<Vec<EvidenceRef>> {
    if sources.is_empty() {
        return Vec::new();
    }
    sources.chunks(cap.max(1)).map(<[_]>::to_vec).collect()
}

/// Rewrites a retrieval action that names the original case.
fn rekey_action(action: &RetrievalAction, old: &CaseId, new: &CaseId) -> RetrievalAction {
    match action {
        RetrievalAction::List { cursor } => RetrievalAction::List {
            cursor: cursor.clone(),
        },
        RetrievalAction::Show {
            case,
            detail,
            cursor,
        } if case == old => RetrievalAction::Show {
            case: new.clone(),
            detail: *detail,
            cursor: cursor.clone(),
        },
        RetrievalAction::Render { case } if case == old => {
            RetrievalAction::Render { case: new.clone() }
        }
        other => other.clone(),
    }
}

/// Splits one case at the per-case source cap without dropping material.
///
/// The split is only applied when a side's retained source list or the case's
/// evidence list exceeds [`PlannerLimits::max_sources_per_case`]. Parts are
/// consecutive: each carries the same text and regions, a disjoint source and
/// evidence slice, and a link to the shared parent. Flow report entries stay
/// on the first part so no change is counted twice.
pub(super) fn split_review_case(case: ReviewCase, limits: PlannerLimits) -> Vec<ReviewCase> {
    let cap = limits.max_sources_per_case.max(1);
    let old = case
        .old_text
        .as_ref()
        .map(|text| source_chunks(&text.sources, cap))
        .unwrap_or_default();
    let new = case
        .new_text
        .as_ref()
        .map(|text| source_chunks(&text.sources, cap))
        .unwrap_or_default();
    let count = old.len().max(new.len());
    if count <= 1 {
        return vec![case];
    }
    let evidence_per_part = case.evidence.len().div_ceil(count).max(1);
    let regions_per_part = case.regions.len().div_ceil(count).max(1);
    let parent = case.case_id.clone();
    let mut parts = Vec::with_capacity(count);
    for index in 0..count {
        let mut part = case.clone();
        let case_id = split_case_id(&parent, u32::try_from(index).unwrap_or(u32::MAX));
        part.case_id = case_id.clone();
        part.content_digest = format!("{}:p{index:04}", case.content_digest);
        part.subcase = Some(super::CaseSplit {
            schema: super::CASE_SPLIT_SCHEMA.into(),
            parent: parent.clone(),
            index: u32::try_from(index).unwrap_or(u32::MAX),
            count: u32::try_from(count).unwrap_or(u32::MAX),
        });
        if let Some(text) = &mut part.old_text {
            text.sources = old.get(index).cloned().unwrap_or_default();
        }
        if let Some(text) = &mut part.new_text {
            text.sources = new.get(index).cloned().unwrap_or_default();
        }
        if index > 0 {
            // The first part carries the quoted text. Later parts carry the
            // remaining source references, not another copy of the same text;
            // the shared parent link tells a reviewer how to reassemble them.
            for text in [&mut part.old_text, &mut part.new_text]
                .into_iter()
                .flatten()
            {
                text.text.clear();
                text.scalar_count = 0;
                text.unmapped.clear();
                text.omitted.clear();
            }
        }
        part.evidence = case
            .evidence
            .iter()
            .skip(index * evidence_per_part)
            .take(evidence_per_part)
            .copied()
            .collect();
        part.regions = case
            .regions
            .iter()
            .skip(index * regions_per_part)
            .take(regions_per_part)
            .copied()
            .collect();
        if index > 0 {
            part.flow_evidence.clear();
        }
        part.available_actions = case
            .available_actions
            .iter()
            .map(|action| rekey_action(action, &parent, &case_id))
            .collect();
        parts.push(part);
    }
    parts
}

/// Everything a finished projection hands to assembly.
pub(super) struct Assembly<'a> {
    pub identity: BundleIdentity,
    pub outcome: EngineOutcome,
    pub channels: &'a BTreeSet<Channel>,
    pub inventory_gaps: Vec<InventoryGap>,
    pub unlocalized_gaps: Vec<UnlocalizedGap>,
    /// Whether any locally rendered evidence exists to answer a visual case.
    pub visual_available: bool,
}

/// How many related or conflicting cases one case names.
///
/// The links are navigation aids; a longer list would grow with document size
/// without telling a reviewer anything more.
const LINKS_PER_CASE: usize = 16;

impl LinkTracker {
    /// Attaches case identifiers to the final listing order.
    ///
    /// The pair generation is the same one the in-memory planner used: every
    /// pair of cases sharing a source is a relation, and a pair of
    /// correspondence questions is also a conflict. Only the storage of the
    /// source-to-case edges changed.
    fn attach(&self, order: &mut [OrderedCase], generation_to_position: &[u32]) {
        let mut related: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); order.len()];
        let mut conflicting: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); order.len()];
        for sharing in self.shared.values().filter(|sharing| sharing.len() > 1) {
            let mut positions: Vec<u32> = sharing
                .iter()
                .map(|generation| generation_to_position[*generation as usize])
                .collect();
            positions.sort_unstable();
            for (position, index) in positions.iter().enumerate() {
                for other in positions.iter().skip(position + 1) {
                    related[*index as usize].insert(*other);
                    related[*other as usize].insert(*index);
                    if order[*index as usize].question == ReviewQuestion::ResolveCorrespondence
                        && order[*other as usize].question == ReviewQuestion::ResolveCorrespondence
                    {
                        conflicting[*index as usize].insert(*other);
                        conflicting[*other as usize].insert(*index);
                    }
                }
            }
        }
        let identifiers: Vec<CaseId> = order.iter().map(|case| case.case_id.clone()).collect();
        for (position, case) in order.iter_mut().enumerate() {
            case.links.related = related[position]
                .iter()
                .filter(|other| !conflicting[position].contains(other))
                .take(LINKS_PER_CASE)
                .map(|other| identifiers[*other as usize].clone())
                .collect();
            case.links.conflicts = conflicting[position]
                .iter()
                .take(LINKS_PER_CASE)
                .map(|other| identifiers[*other as usize].clone())
                .collect();
        }
    }
}

/// Splits raw cases at the per-case caps and writes their final parts.
///
/// The raw and final stores may be the same kind of store but are always
/// distinct instances: a raw case is read once, partitioned, and replaced by
/// its parts, so a publisher never sees both representations.
pub(super) struct FinalizedCases {
    pub entries: Vec<(u32, OrderedCase)>,
    pub contexts: Vec<usize>,
    pub tracker: LinkTracker,
}

pub(super) fn finalize_cases(
    raw: &mut dyn CaseStore,
    final_store: &mut dyn CaseStore,
    limits: super::PlannerLimits,
    budget: &mut Budget,
    extents: &SourceExtents,
    handles: Vec<usize>,
) -> Result<FinalizedCases, PlanError> {
    let mut tracker = LinkTracker::new(extents);
    let mut entries: Vec<(u32, OrderedCase)> = Vec::with_capacity(handles.len());
    let mut contexts = Vec::new();
    for handle in handles {
        let (case, context) = raw.load(handle)?;
        let parent = case.case_id.clone();
        let parts = split_review_case(case, limits);
        let split = parts.len() > 1;
        if split {
            budget.clear_case_omission(&parent, super::OmissionKind::GapSourcesOmitted);
        }
        let mut context = context;
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 && !budget.accept_case() {
                continue;
            }
            let case_id = part.case_id.clone();
            let question = part.question;
            let finding = part.finding;
            let old_page = part.old.as_ref().and_then(|side| side.page_number);
            let new_page = part.new.as_ref().and_then(|side| side.page_number);
            let pages = ordered_pages(&part);
            let has_context = !split && index == 0 && context.is_some();
            let stored_context = if has_context { context.take() } else { None };
            let generation = u32::try_from(entries.len())
                .map_err(|_| PlanError::new("review plan exceeds the supported case count"))?;
            tracker.record(generation, &part);
            let final_handle = final_store.store(part, stored_context)?;
            if has_context {
                contexts.push(final_handle);
            }
            entries.push((
                generation,
                OrderedCase {
                    handle: final_handle,
                    case_id,
                    question,
                    finding,
                    old_page,
                    new_page,
                    pages,
                    links: CaseLinks::default(),
                    context: has_context.then_some(final_handle),
                },
            ));
        }
    }
    Ok(FinalizedCases {
        entries,
        contexts,
        tracker,
    })
}

/// Materializes a streaming plan from an in-memory store.
///
/// This is the whole-plan frontend used by callers and tests that do not need
/// the streaming bound. Store failures are impossible for
/// [`MemoryCaseStore`], so the loads are infallible in practice.
pub(super) fn materialize(plan: StreamingPlan, store: &mut dyn CaseStore) -> ReviewPlan {
    let mut cases = Vec::with_capacity(plan.order.len());
    for ordered in &plan.order {
        let (mut case, _) = store
            .load(ordered.handle)
            .expect("an in-memory case handle remains valid");
        case.related_cases.clone_from(&ordered.links.related);
        case.conflicts_with.clone_from(&ordered.links.conflicts);
        cases.push(case);
    }
    let mut contexts = Vec::with_capacity(plan.contexts.len());
    for handle in &plan.contexts {
        let (_, context) = store
            .load(*handle)
            .expect("an in-memory context handle remains valid");
        if let Some(context) = context {
            contexts.push(context);
        }
    }
    ReviewPlan {
        manifest: plan.manifest,
        cases,
        contexts,
    }
}

/// Final assembly: order the cases, census them, and build the index.
///
/// `entries` pairs each case's generation index with its fixed-size metadata.
/// The case bodies remain in the store, so sorting and linking touch only the
/// index rather than every retained packet.
pub(super) fn assemble_streaming(
    assembly: Assembly<'_>,
    budget: &Budget,
    entries: Vec<(u32, OrderedCase)>,
    tracker: &LinkTracker,
    contexts: Vec<usize>,
) -> StreamingPlan {
    let Assembly {
        identity,
        outcome,
        channels,
        inventory_gaps,
        unlocalized_gaps,
        visual_available,
    } = assembly;
    let mut entries = entries;
    // Cases the engine already settled something about come first, so a
    // reviewer that stops early stops on the material the engine could reach
    // rather than at an arbitrary point. Within one finding the order is
    // stable and readable, and it implies no ranking among equals.
    entries.sort_by(|(_, left), (_, right)| ordered_key(left).cmp(&ordered_key(right)));
    let mut generation_to_position = vec![0; entries.len()];
    for (position, (generation, _)) in entries.iter().enumerate() {
        generation_to_position[*generation as usize] = u32::try_from(position).unwrap_or(u32::MAX);
    }
    let mut order: Vec<OrderedCase> = entries.into_iter().map(|(_, case)| case).collect();
    tracker.attach(&mut order, &generation_to_position);
    let mut by_question: BTreeMap<String, (ReviewQuestion, usize)> = BTreeMap::new();
    for case in &order {
        let entry = by_question
            .entry(question_key(case.question).to_owned())
            .or_insert((case.question, 0));
        entry.1 += 1;
    }
    let pipeline = identity.pipeline;
    let bundle_id: BundleId = identity.bundle_id();
    let manifest = AgentReviewManifest {
        schema: super::REVIEW_SCHEMA.into(),
        bundle_id,
        identity,
        pipeline,
        selected_channels: channels.clone(),
        engine: outcome,
        inventory_gaps,
        cases: CaseCensus {
            total: order.len(),
            by_question: by_question
                .into_values()
                .map(|(question, cases)| QuestionCount { question, cases })
                .collect(),
        },
        unlocalized_gaps,
        export: budget.status(order.len()),
        capabilities: vec![
            RetrievalCapability {
                detail: Detail::Index,
                available: true,
            },
            RetrievalCapability {
                detail: Detail::Text,
                available: true,
            },
            RetrievalCapability {
                detail: Detail::Quote,
                available: true,
            },
            RetrievalCapability {
                detail: Detail::Context,
                available: true,
            },
            RetrievalCapability {
                detail: Detail::Alternatives,
                available: true,
            },
            RetrievalCapability {
                detail: Detail::Visual,
                available: visual_available,
            },
        ],
        next: RetrievalAction::List { cursor: None },
    };
    StreamingPlan {
        manifest,
        order,
        contexts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::GlyphId;

    #[test]
    fn aliases_are_disjoint_by_origin() {
        assert_eq!(
            source_alias(SourceRef::Native { glyph: GlyphId(7) }).as_str(),
            "g7"
        );
        assert_eq!(
            source_alias(SourceRef::Structured { element: 7 }).as_str(),
            "s7"
        );
        assert_ne!(
            source_alias(SourceRef::Rendered { region: 7 }),
            source_alias(SourceRef::Structured { element: 7 })
        );
    }

    #[test]
    fn identifiers_do_not_renumber_when_a_neighbour_changes() {
        let mut first = Identifiers::new();
        let a = first.case(&"a".repeat(64));
        let b = first.case(&"b".repeat(64));

        let mut second = Identifiers::new();
        let c = second.case(&"c".repeat(64));
        let b_again = second.case(&"b".repeat(64));

        assert_ne!(a, c);
        assert_eq!(b, b_again, "an identifier depends only on its own case");
    }

    #[test]
    fn colliding_digests_still_receive_distinct_identifiers() {
        let mut identifiers = Identifiers::new();
        let digest = "f".repeat(64);
        let minted = (0..6)
            .map(|_| identifiers.case(&digest))
            .collect::<BTreeSet<_>>();
        assert_eq!(minted.len(), 6);
        assert!(
            minted
                .iter()
                .all(|id| id.as_str().len() <= MAX_IDENTIFIER_BYTES)
        );
    }

    fn split_fixture() -> ReviewCase {
        use crate::review::{
            CaseCompleteness, EngineClass, PipelineContract, ReviewQuestion, ReviewText, TextOrder,
        };

        let sources = vec![
            EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(1) }),
            EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(2) }),
            EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(3) }),
        ];
        ReviewCase {
            case_id: CaseId::new("R17").expect("case id"),
            content_digest: "00".repeat(32),
            subcase: None,
            question: ReviewQuestion::CompareContent,
            pipeline: PipelineContract::SharedEvidence,
            engine_class: EngineClass::Strict,
            finding: CaseFinding::DifferenceEstablished,
            channels: BTreeSet::new(),
            completeness: CaseCompleteness::unknown(),
            reasons: Vec::new(),
            assumptions: Vec::new(),
            old: None,
            new: None,
            old_text: Some(ReviewText {
                side: Side::Old,
                text: "abc".into(),
                scalar_count: 3,
                order: TextOrder::RenderOrder,
                unmapped: Vec::new(),
                omitted: Vec::new(),
                token_range: None,
                canonical_range: None,
                sources,
            }),
            new_text: None,
            hypotheses: Vec::new(),
            alternatives_total: None,
            alternatives_returned: 0,
            omitted: Vec::new(),
            next_cursor: None,
            required_evidence: Vec::new(),
            related_cases: Vec::new(),
            conflicts_with: Vec::new(),
            available_actions: Vec::new(),
            covered: Vec::new(),
            flow_evidence: Vec::new(),
            regions: Vec::new(),
            evidence: vec![
                EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(1) }),
                EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(2) }),
                EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(3) }),
            ],
        }
    }

    #[test]
    fn splitting_charges_parts_and_reports_withheld_cases() {
        let limits = PlannerLimits {
            max_cases: 2,
            max_sources_per_case: 1,
            ..PlannerLimits::default()
        };
        let mut raw = MemoryCaseStore::new();
        let handle = raw
            .store(split_fixture(), None)
            .expect("fixture case stores");
        let mut final_store = MemoryCaseStore::new();
        let mut budget = Budget::new(limits);
        assert!(
            budget.accept_case(),
            "the raw case was accepted while planning"
        );
        let finalized = finalize_cases(
            &mut raw,
            &mut final_store,
            limits,
            &mut budget,
            &SourceExtents::default(),
            vec![handle],
        )
        .expect("fixture finalizes");
        let entries = finalized.entries;
        let contexts = finalized.contexts;
        budget.finalize();
        assert_eq!(
            entries.len(),
            2,
            "the first part is accepted, the second fits"
        );
        assert_eq!(entries[0].1.case_id.as_str(), "R17-p0000");
        assert_eq!(entries[1].1.case_id.as_str(), "R17-p0001");
        assert!(contexts.is_empty());
        let status = budget.status(entries.len());
        assert!(!status.export_complete);
        assert!(status.omissions.iter().any(|omission| {
            omission.kind == OmissionKind::CasesNotExported && omission.omitted == Some(1)
        }));
    }

    #[test]
    fn retention_stops_at_a_sentence_boundary_or_withholds_the_run() {
        let scalars: Vec<char> = "First sentence. Second sentence.".chars().collect();
        assert_eq!(retention_boundary(&scalars, 1_000), Some(scalars.len()));
        assert_eq!(retention_boundary(&scalars, 20), Some(15));

        // A long run with no boundary is withheld rather than cut inside a value.
        let unbroken: Vec<char> = "1234567890".chars().collect();
        assert_eq!(retention_boundary(&unbroken, 4), None);
    }
}
