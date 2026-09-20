//! Shared planning machinery: identifiers, aliases, text projection, budgets,
//! and final assembly of a bundle's manifest and cases.
//!
//! Planning is pure. It reads the comparison a run already produced and writes
//! no files, so the same inputs always produce the same plan and the engine's
//! result cannot be altered by exporting it.

use std::collections::{BTreeMap, BTreeSet};

use sha2::{Digest, Sha256};

use super::{
    AgentReviewManifest, BundleId, BundleIdentity, CaseCensus, CaseId, Detail, EngineOutcome,
    EvidenceRef, ExportOmission, ExportStatus, GapId, InventoryGap, OmissionKind, OmissionScope,
    OmittedRun, QuestionCount, RetrievalAction, RetrievalCapability, ReviewCase, ReviewQuestion,
    ReviewText, ScalarInterval, Side, SourceAlias, TextOrder, UnlocalizedGap, UnmappedMark,
    case::OmissionReason, lowercase_hex,
};
use crate::{
    document::{Channel, SourceRef, TextView, ViewBasis},
    normalize::ComparableToken,
};

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
            max_cases: 4_096,
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
        for length in [12, 20, 32, 64] {
            let candidate = format!("{prefix}{}", &digest[..length.min(digest.len())]);
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
        }
        // Two cases with the same full digest are the same case; disambiguate
        // by ordinal rather than returning a duplicate identifier.
        let mut ordinal = 1u32;
        loop {
            let candidate = format!("{prefix}{digest}-{ordinal}");
            if self.used.insert(candidate.clone()) {
                return candidate;
            }
            ordinal += 1;
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
        if self.cases >= self.limits.max_cases {
            self.stop(OmissionScope::Bundle, OmissionKind::CasesNotExported, None);
            return false;
        }
        self.cases += 1;
        true
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
fn retention_boundary(scalars: &[char], budget: usize) -> Option<usize> {
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

/// Everything a finished projection hands to assembly.
pub(super) struct Assembly<'a> {
    pub contexts: Vec<super::CaseContext>,
    pub identity: BundleIdentity,
    pub outcome: EngineOutcome,
    pub channels: &'a BTreeSet<Channel>,
    pub inventory_gaps: Vec<InventoryGap>,
    pub unlocalized_gaps: Vec<UnlocalizedGap>,
    pub cases: Vec<ReviewCase>,
    /// Whether any locally rendered evidence exists to answer a visual case.
    pub visual_available: bool,
}

/// How many related or conflicting cases one case names.
///
/// The links are navigation aids; a longer list would grow with document size
/// without telling a reviewer anything more.
const fn limits_for_links() -> usize {
    16
}

/// Links cases that quote the same evidence.
///
/// Two cases sharing a source reference are two views of the same material.
/// That is ordinarily context overlap, so it is recorded as a relation. It
/// becomes a declared conflict only when both cases ask which counterpart the
/// material corresponds to, because answering both would give the same source
/// two owners.
fn link_cases(cases: &mut [ReviewCase], limit: usize) {
    let mut holders: BTreeMap<(Side, SourceRef), Vec<usize>> = BTreeMap::new();
    for (index, case) in cases.iter().enumerate() {
        for reference in &case.evidence {
            holders
                .entry((reference.side, reference.source))
                .or_default()
                .push(index);
        }
    }
    let mut related: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); cases.len()];
    let mut conflicting: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); cases.len()];
    for sharing in holders.into_values().filter(|sharing| sharing.len() > 1) {
        for (position, index) in sharing.iter().enumerate() {
            for other in sharing.iter().skip(position + 1) {
                related[*index].insert(*other);
                related[*other].insert(*index);
                if cases[*index].question == ReviewQuestion::ResolveCorrespondence
                    && cases[*other].question == ReviewQuestion::ResolveCorrespondence
                {
                    conflicting[*index].insert(*other);
                    conflicting[*other].insert(*index);
                }
            }
        }
    }
    let identifiers: Vec<CaseId> = cases.iter().map(|case| case.case_id.clone()).collect();
    for (index, case) in cases.iter_mut().enumerate() {
        case.related_cases = related[index]
            .iter()
            .filter(|other| !conflicting[index].contains(other))
            .take(limit)
            .map(|other| identifiers[*other].clone())
            .collect();
        case.conflicts_with = conflicting[index]
            .iter()
            .take(limit)
            .map(|other| identifiers[*other].clone())
            .collect();
    }
}

/// Final assembly: order the cases, census them, and build the index.
pub(super) fn assemble(assembly: Assembly<'_>, budget: &Budget) -> ReviewPlan {
    let Assembly {
        contexts,
        identity,
        outcome,
        channels,
        inventory_gaps,
        unlocalized_gaps,
        mut cases,
        visual_available,
    } = assembly;
    // Cases the engine already settled something about come first, so a
    // reviewer that stops early stops on the material the engine could reach
    // rather than at an arbitrary point. Within one finding the order is
    // stable and readable, and it implies no ranking among equals.
    cases.sort_by(|left, right| {
        let key = |case: &ReviewCase| {
            (
                case.finding.rank(),
                format!("{:?}", case.question),
                case.old.as_ref().and_then(|side| side.page_number),
                case.new.as_ref().and_then(|side| side.page_number),
                case.case_id.as_str().to_owned(),
            )
        };
        key(left).cmp(&key(right))
    });
    link_cases(&mut cases, limits_for_links());
    let mut by_question: BTreeMap<String, (ReviewQuestion, usize)> = BTreeMap::new();
    for case in &cases {
        let entry = by_question
            .entry(format!("{:?}", case.question))
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
            total: cases.len(),
            by_question: by_question
                .into_values()
                .map(|(question, cases)| QuestionCount { question, cases })
                .collect(),
        },
        unlocalized_gaps,
        export: budget.status(cases.len()),
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
    let retained: BTreeSet<_> = cases.iter().map(|case| case.case_id.clone()).collect();
    ReviewPlan {
        manifest,
        cases,
        // Context for a case the budget dropped would describe nothing.
        contexts: contexts
            .into_iter()
            .filter(|context| retained.contains(&context.case_id))
            .collect(),
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
        assert_ne!(identifiers.case(&digest), identifiers.case(&digest));
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
