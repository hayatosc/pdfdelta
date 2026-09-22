//! Projection of a shared-evidence comparison into review cases.
//!
//! The projection reads the comparison a run already produced. It reaches the
//! obligations from four independent directions, because no single one of them
//! sees all of the material:
//!
//! * local comparisons and scope searches that retained an obligation,
//! * non-owning range reviews and inferred observations,
//! * acquisition failures and channels whose interpretation is missing,
//! * discovered source references that no comparison ever reached.
//!
//! The last direction is what keeps material with no candidate from vanishing:
//! a page that yielded nothing, a scope that was never visited, and an
//! inventory that never closed all leave references behind, and every one of
//! them ends up in a case or in an explicit gap.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    BundleIdentity, CaseCompleteness, CaseFinding, Completeness, Detail, EngineClass,
    EngineOutcome, EvidenceRef, GapScope, Hypothesis, HypothesisId, InventoryGap, OmissionKind,
    OmissionScope, PlannerLimits, ReasonRecord, RequiredEvidence, RetrievalAction,
    ReviewAssumption, ReviewCase, ReviewPlan, ReviewQuestion, ReviewReason, ReviewText, Side,
    SideLocation, TextOrder, UnlocalizedGap,
    case::Cardinality,
    planner::{Assembly, Budget, CaseKey, Identifiers, assemble, evidence_ref, review_text},
};
use crate::{
    document::{
        Channel, ChannelSourceAccounting, DocumentView, DocumentViewComparison, EvidenceFailure,
        EvidenceIssue, FieldValue, GraphNode, InterpretationStatus, MatchingComponent, NodeContent,
        NodeId, ScopeViewComparison, SourceRef, StructuredValue, UnresolvedObligation,
        UnresolvedReason, document_source_accounting, unresolved_classification,
    },
    model::PageId,
};

/// One comparison run, ready to project.
///
/// The views, graphs, and comparison must be the ones the run produced. A
/// re-parse would produce different execution-local source identifiers, and the
/// packet's evidence references would then point at different material.
pub struct SharedEvidenceReview<'a> {
    pub identity: BundleIdentity,
    pub outcome: EngineOutcome,
    pub old: DocumentView<'a>,
    pub new: DocumentView<'a>,
    pub comparison: &'a DocumentViewComparison,
    pub channels: &'a BTreeSet<Channel>,
}

/// The fields a case is built from, before identity and evidence are derived.
struct CaseDraft<'n> {
    question: ReviewQuestion,
    engine_class: EngineClass,
    /// What the engine established for this material.
    finding: CaseFinding,
    old_nodes: &'n [NodeId],
    new_nodes: &'n [NodeId],
    reasons: Vec<ReasonRecord>,
    assumptions: Vec<ReviewAssumption>,
    completeness: CaseCompleteness,
    hypotheses: Vec<Hypothesis>,
    /// `None` when candidate enumeration never closed, so the number of
    /// competing hypotheses is unknown rather than equal to what was returned.
    alternatives_total: Option<usize>,
    /// References the case quotes that its member views do not own, such as a
    /// range review's interior or a relationship's endpoints.
    extra_old: Vec<SourceRef>,
    extra_new: Vec<SourceRef>,
    /// A location established outside the graph, such as a failing page.
    location: Option<(Side, SideLocation)>,
}

impl CaseDraft<'_> {
    /// A draft with no extra references and no external location.
    fn located(self, side: Side, location: SideLocation) -> Self {
        Self {
            location: Some((side, location)),
            ..self
        }
    }
}

struct Planner<'a> {
    input: &'a SharedEvidenceReview<'a>,
    limits: PlannerLimits,
    budget: Budget,
    identifiers: Identifiers,
    cases: Vec<ReviewCase>,
    gaps: Vec<UnlocalizedGap>,
    explained: BTreeSet<(Side, SourceRef)>,
    /// Content digest to case index, so the same question asked twice about the
    /// same material is one case rather than two competing identifiers.
    by_digest: BTreeMap<String, usize>,
    contexts: Vec<super::CaseContext>,
    old_nodes: BTreeMap<NodeId, &'a GraphNode>,
    new_nodes: BTreeMap<NodeId, &'a GraphNode>,
}

/// Projects a shared-evidence comparison into a bounded review plan.
///
/// The comparison is not modified and not recomputed. Planning stops at its
/// budgets, recording what it could not describe as an explicit omission rather
/// than as a resolved obligation.
#[must_use]
pub fn plan_shared_evidence(input: &SharedEvidenceReview<'_>, limits: PlannerLimits) -> ReviewPlan {
    let planner = Planner {
        input,
        limits,
        budget: Budget::new(limits),
        identifiers: Identifiers::new(),
        cases: Vec::new(),
        gaps: Vec::new(),
        explained: BTreeSet::new(),
        by_digest: BTreeMap::new(),
        contexts: Vec::new(),
        old_nodes: input
            .old
            .graph
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect(),
        new_nodes: input
            .new
            .graph
            .nodes
            .iter()
            .map(|node| (node.id, node))
            .collect(),
    };
    planner.plan()
}

impl<'a> Planner<'a> {
    fn plan(mut self) -> ReviewPlan {
        let accounting = document_source_accounting(
            self.input.old,
            self.input.new,
            self.input.comparison,
            self.input.channels,
        );
        self.scopes();
        self.relations();
        self.acquisition();
        self.visual_only_pages();
        self.residual_sources(&accounting);
        let visual_available = !self.input.old.evidence.rendered.is_empty()
            || !self.input.new.evidence.rendered.is_empty();
        let inventory_gaps = accounting
            .iter()
            .flat_map(|channel| {
                [
                    (Side::Old, &channel.old, channel.channel),
                    (Side::New, &channel.new, channel.channel),
                ]
                .map(|(side, accounting, name)| InventoryGap {
                    side,
                    channel: name,
                    inventory_complete: accounting.inventory_complete,
                    discovered_sources: accounting.discovered.len(),
                    compared_sources: accounting
                        .discovered
                        .intersection(&accounting.compared)
                        .count(),
                    uncompared_sources: accounting.uncompared().count(),
                })
            })
            .filter(|gap| !gap.inventory_complete || gap.uncompared_sources > 0)
            .collect();
        assemble(
            Assembly {
                contexts: self.contexts,
                identity: self.input.identity.clone(),
                outcome: self.input.outcome.clone(),
                channels: self.input.channels,
                inventory_gaps,
                unlocalized_gaps: self.gaps,
                cases: self.cases,
                visual_available,
            },
            &self.budget,
        )
    }

    fn node(&self, side: Side, id: NodeId) -> Option<&'a GraphNode> {
        match side {
            Side::Old => self.old_nodes.get(&id).copied(),
            Side::New => self.new_nodes.get(&id).copied(),
        }
    }

    fn location(&self, side: Side, nodes: &[NodeId]) -> Option<SideLocation> {
        let node = nodes.iter().find_map(|id| self.node(side, *id))?;
        Some(
            SideLocation::unknown(side)
                .with_page(node.pages.first().copied())
                .view(node.kind),
        )
    }

    /// Retained text for a view, including a stored field value.
    ///
    /// A stored value has no painting order of its own, so it is reported with
    /// an unknown order rather than being presented as read text.
    fn node_text(&self, side: Side, nodes: &[NodeId]) -> Option<ReviewText> {
        let mut parts = Vec::new();
        for id in nodes {
            let Some(node) = self.node(side, *id) else {
                continue;
            };
            match &node.content {
                NodeContent::Text { view } => {
                    parts.push(review_text(side, node.basis, view, None, self.limits));
                }
                NodeContent::Value { value } => {
                    let text = match value {
                        FieldValue::Text(text) => text.clone(),
                        FieldValue::Choices(choices) => choices.join("\n"),
                        FieldValue::Selected(selected) => selected.to_string(),
                        FieldValue::Name(_) | FieldValue::Empty | FieldValue::Unresolved { .. } => {
                            String::new()
                        }
                    };
                    parts.push(ReviewText {
                        side,
                        scalar_count: text.chars().count(),
                        text,
                        order: TextOrder::Unknown,
                        unmapped: Vec::new(),
                        omitted: Vec::new(),
                        token_range: None,
                        canonical_range: None,
                        sources: node
                            .sources
                            .iter()
                            .map(|source| evidence_ref(side, *source))
                            .collect(),
                    });
                }
                NodeContent::Container | NodeContent::Visual { .. } | NodeContent::Unknown => {}
            }
        }
        if parts.is_empty() {
            return None;
        }
        let mut merged = parts.remove(0);
        for part in parts {
            // Members are concatenated only in the order the engine retained;
            // no separator is invented between them.
            merged.text.push_str(&part.text);
            merged.scalar_count += part.scalar_count;
            merged.sources.extend(part.sources);
            merged.omitted.extend(part.omitted);
            if merged.order != part.order {
                merged.order = TextOrder::Unknown;
            }
        }
        merged.sources.sort();
        merged.sources.dedup();
        Some(merged)
    }

    /// Pages a case covers, with the union of its sources' retained geometry.
    ///
    /// Geometry that was not retained leaves the bounds unknown rather than
    /// producing an invented box; a reader is then shown the whole page.
    fn regions(
        &self,
        old_sources: &BTreeSet<SourceRef>,
        new_sources: &BTreeSet<SourceRef>,
    ) -> Vec<super::CaseRegion> {
        let mut regions = Vec::new();
        for (side, view, sources) in [
            (Side::Old, self.input.old, old_sources),
            (Side::New, self.input.new, new_sources),
        ] {
            let mut pages: BTreeMap<PageId, Option<[f64; 4]>> = BTreeMap::new();
            for source in sources {
                let Some((page, bounds)) = source_geometry(view, *source) else {
                    continue;
                };
                let entry = pages.entry(page).or_insert(None);
                *entry = match (*entry, bounds) {
                    (Some(current), Some(next)) => Some(union(current, next)),
                    (Some(current), None) => Some(current),
                    (None, next) => next,
                };
            }
            regions.extend(pages.into_iter().map(|(page, bounds)| super::CaseRegion {
                side,
                page_number: page.0.saturating_add(1),
                page_index: page,
                bounds,
            }));
        }
        regions
    }

    fn sources_of(&self, side: Side, nodes: &[NodeId]) -> BTreeSet<SourceRef> {
        nodes
            .iter()
            .filter_map(|id| self.node(side, *id))
            .flat_map(|node| node.sources.iter().copied())
            .collect()
    }

    /// Records a case and marks the evidence it quotes as explained.
    ///
    /// A case whose canonical key repeats an existing one is the same question
    /// about the same material, so its references are merged into that case
    /// instead of producing a second identifier for one decision.
    fn push(&mut self, case: Option<ReviewCase>) {
        let Some(case) = case else {
            return;
        };
        if let Some(index) = self.by_digest.get(&case.content_digest).copied() {
            let existing = &mut self.cases[index];
            for reference in case.evidence {
                if !existing.evidence.contains(&reference) {
                    existing.evidence.push(reference);
                }
            }
            for reference in &existing.evidence {
                self.explained.insert((reference.side, reference.source));
            }
            return;
        }
        if !self.budget.accept_case() {
            return;
        }
        for reference in &case.evidence {
            self.explained.insert((reference.side, reference.source));
        }
        self.by_digest
            .insert(case.content_digest.clone(), self.cases.len());
        self.cases.push(case);
    }

    fn build_case(&mut self, draft: CaseDraft<'_>) -> Option<ReviewCase> {
        let CaseDraft {
            question,
            engine_class,
            finding,
            old_nodes,
            new_nodes,
            reasons,
            assumptions,
            completeness,
            hypotheses,
            alternatives_total,
            extra_old,
            extra_new,
            location,
        } = draft;
        let mut old_sources = self.sources_of(Side::Old, old_nodes);
        let mut new_sources = self.sources_of(Side::New, new_nodes);
        old_sources.extend(extra_old);
        new_sources.extend(extra_new);
        let mut key = CaseKey::new(question);
        key.debug(&engine_class);
        key.debug(&finding);
        key.sources(Side::Old, &old_sources);
        key.sources(Side::New, &new_sources);
        for reason in &reasons {
            key.debug(&reason.reason);
            if let Some(message) = &reason.message {
                key.field(message.as_bytes());
            }
        }
        let digest = key.finish();
        if let Some(index) = self.by_digest.get(&digest).copied() {
            let existing = &mut self.cases[index];
            for source in old_sources
                .iter()
                .map(|source| evidence_ref(Side::Old, *source))
                .chain(
                    new_sources
                        .iter()
                        .map(|source| evidence_ref(Side::New, *source)),
                )
            {
                if !existing.evidence.contains(&source) {
                    existing.evidence.push(source);
                }
            }
            for reference in &existing.evidence {
                self.explained.insert((reference.side, reference.source));
            }
            return None;
        }
        let case_id = self.identifiers.case(&digest);
        let mut has_context = false;
        if !old_nodes.is_empty() || !new_nodes.is_empty() {
            let context = super::context::gather(
                &case_id,
                self.input.old,
                self.input.new,
                (old_nodes, new_nodes),
                self.limits,
            );
            if !context.items.is_empty() || !context.complete {
                self.contexts.push(context);
                has_context = true;
            }
        }
        let old_text = self.node_text(Side::Old, old_nodes);
        let new_text = self.node_text(Side::New, new_nodes);
        let pages: BTreeSet<PageId> = old_nodes
            .iter()
            .filter_map(|id| self.node(Side::Old, *id))
            .flat_map(|node| node.pages.iter().copied())
            .collect();
        let rendered = pages.iter().any(|page| {
            self.input
                .old
                .evidence
                .rendered
                .iter()
                .any(|region| region.page == *page)
        }) || new_nodes
            .iter()
            .filter_map(|id| self.node(Side::New, *id))
            .flat_map(|node| node.pages.iter().copied())
            .any(|page| {
                self.input
                    .new
                    .evidence
                    .rendered
                    .iter()
                    .any(|region| region.page == page)
            });
        let readable = old_text.as_ref().is_some_and(|text| !text.text.is_empty())
            || new_text.as_ref().is_some_and(|text| !text.text.is_empty());
        let required_evidence = if readable {
            Vec::new()
        } else if rendered {
            vec![RequiredEvidence::Visual]
        } else {
            vec![RequiredEvidence::Unavailable]
        };
        let mut available_actions = vec![RetrievalAction::Show {
            case: case_id.clone(),
            detail: Detail::Text,
            cursor: None,
        }];
        // Unexamined material is located rather than quoted at the text level,
        // so the action that quotes it is the one a reviewer needs next.
        if !finding.examined() && readable {
            available_actions.push(RetrievalAction::Show {
                case: case_id.clone(),
                detail: Detail::Quote,
                cursor: None,
            });
        }
        // Offering context for a case that gathered none invites a retrieval
        // that can only answer "nothing", once per case.
        if has_context {
            available_actions.push(RetrievalAction::Show {
                case: case_id.clone(),
                detail: Detail::Context,
                cursor: None,
            });
        }
        // An unknown total is a reason to ask, but only where a search ran at
        // all: material nothing examined has no competitors to enumerate, and
        // offering the level would invite one call per case to hear so.
        if !hypotheses.is_empty() || (alternatives_total.is_none() && finding.examined()) {
            available_actions.push(RetrievalAction::Show {
                case: case_id.clone(),
                detail: Detail::Alternatives,
                cursor: None,
            });
        }
        if rendered {
            available_actions.push(RetrievalAction::Render {
                case: case_id.clone(),
            });
        }
        // The plan is the bundle's content, so a case lists every reference it
        // quotes. Response-sized views truncate with their own explicit
        // omissions; truncating here would leave an obligation unexplained.
        let evidence: Vec<EvidenceRef> = old_sources
            .iter()
            .map(|source| evidence_ref(Side::Old, *source))
            .chain(
                new_sources
                    .iter()
                    .map(|source| evidence_ref(Side::New, *source)),
            )
            .collect();
        let regions = self.regions(&old_sources, &new_sources);
        let (old_location, new_location) = match location {
            Some((Side::Old, location)) => (Some(location), self.location(Side::New, new_nodes)),
            Some((Side::New, location)) => (self.location(Side::Old, old_nodes), Some(location)),
            None => (
                self.location(Side::Old, old_nodes),
                self.location(Side::New, new_nodes),
            ),
        };
        Some(ReviewCase {
            case_id,
            content_digest: digest,
            question,
            pipeline: self.input.identity.pipeline,
            engine_class,
            finding,
            channels: self.input.channels.clone(),
            completeness,
            reasons,
            assumptions,
            old: old_location,
            new: new_location,
            old_text,
            new_text,
            alternatives_returned: hypotheses.len(),
            hypotheses,
            alternatives_total,
            omitted: Vec::new(),
            next_cursor: None,
            required_evidence,
            related_cases: Vec::new(),
            conflicts_with: Vec::new(),
            available_actions,
            evidence,
            // This contract accounts for material through source references.
            covered: Vec::new(),
            regions,
        })
    }

    fn scopes(&mut self) {
        for scope_index in 0..self.input.comparison.scopes.len() {
            let scope = &self.input.comparison.scopes[scope_index];
            if !self.budget.visit_sources(scope.result.comparisons.len()) {
                return;
            }
            let inferred_parent = scope.interpretation == InterpretationStatus::Inferred;
            self.local_comparisons(scope_index, inferred_parent);
            self.scope_obligations(scope_index, inferred_parent);
            self.text_scope_reviews(scope_index, inferred_parent);
        }
    }

    fn local_comparisons(&mut self, scope_index: usize, inferred_parent: bool) {
        let scope = &self.input.comparison.scopes[scope_index].result;
        let attached = scope_obligations_by_comparison(scope);
        for index in 0..scope.comparisons.len() {
            let local = &self.input.comparison.scopes[scope_index].result.comparisons[index];
            if local.compared && local.unresolved.is_empty() {
                continue;
            }
            let mut reasons: Vec<ReasonRecord> = local
                .unresolved
                .iter()
                .enumerate()
                .map(|(position, message)| {
                    ReasonRecord::new(
                        unresolved_classification(&local.obligations, position)
                            .reason
                            .into(),
                    )
                    .with_message(message.clone())
                })
                .collect();
            reasons.extend(attached.get(&index).into_iter().flatten().cloned());
            let engine_class =
                if inferred_parent || local.interpretation == InterpretationStatus::Inferred {
                    EngineClass::Inferred
                } else {
                    EngineClass::Strict
                };
            let mut assumptions = vec![ReviewAssumption::AcceptedCorrespondence];
            if inferred_parent {
                assumptions.push(ReviewAssumption::InferredParentScope);
            }
            let completeness = CaseCompleteness {
                evidence: Completeness::Unknown,
                candidate_enumeration: Completeness::from_flag(
                    self.input.comparison.scopes[scope_index]
                        .result
                        .candidates
                        .exhaustive,
                ),
                solver_search: Completeness::from_flag(
                    self.input.comparison.scopes[scope_index]
                        .result
                        .matching
                        .conflict_search_complete,
                ),
                response: Completeness::Complete,
            };
            let old = local.old.clone();
            let new = local.new.clone();
            let finding = finding_of(local);
            let case = self.build_case(CaseDraft {
                question: ReviewQuestion::CompareContent,
                engine_class,
                finding,
                old_nodes: &old,
                new_nodes: &new,
                reasons,
                assumptions,
                completeness,
                hypotheses: Vec::new(),
                alternatives_total: Some(0),
                extra_old: Vec::new(),
                extra_new: Vec::new(),
                location: None,
            });
            self.push(case);
        }
    }

    fn scope_obligations(&mut self, scope_index: usize, inferred_parent: bool) {
        let scope = &self.input.comparison.scopes[scope_index].result;
        // Obligations about the same proposal or the same competing component
        // are one question, not several: a reviewer chooses once among the
        // alternatives that compete with each other.
        let mut grouped: BTreeMap<ObligationGroup, Vec<ReasonRecord>> = BTreeMap::new();
        for (position, message) in scope.unresolved.iter().enumerate() {
            let obligation = unresolved_classification(&scope.obligations, position);
            if obligation.comparison.is_some() {
                // Already carried by the local comparison's own case.
                continue;
            }
            let group = match (obligation.proposal, obligation.component) {
                (Some(proposal), _) => ObligationGroup::Proposal(proposal),
                (None, Some(component)) => ObligationGroup::Component(component),
                (None, None) => ObligationGroup::Scope,
            };
            grouped
                .entry(group)
                .or_default()
                .push(ReasonRecord::new(obligation.reason.into()).with_message(message.clone()));
        }
        for (group, reasons) in grouped {
            let (old_nodes, new_nodes, hypotheses, total) = match group {
                ObligationGroup::Proposal(index) => self.proposal_view(scope_index, index),
                ObligationGroup::Component(index) => self.component_view(scope_index, index),
                ObligationGroup::Scope => {
                    let scope = &self.input.comparison.scopes[scope_index]
                        .result
                        .matching
                        .scope;
                    (vec![scope.old], vec![scope.new], Vec::new(), None)
                }
            };
            let scope = &self.input.comparison.scopes[scope_index].result;
            let completeness = CaseCompleteness {
                evidence: Completeness::Unknown,
                candidate_enumeration: Completeness::from_flag(scope.candidates.exhaustive),
                solver_search: Completeness::from_flag(scope.matching.conflict_search_complete),
                response: Completeness::Complete,
            };
            let mut assumptions = Vec::new();
            if inferred_parent {
                assumptions.push(ReviewAssumption::InferredParentScope);
            }
            let case = self.build_case(CaseDraft {
                question: ReviewQuestion::ResolveCorrespondence,
                engine_class: if inferred_parent {
                    EngineClass::Inferred
                } else {
                    EngineClass::Unavailable
                },
                // The correspondence itself is what is open here.
                finding: CaseFinding::NotEstablished,
                old_nodes: &old_nodes,
                new_nodes: &new_nodes,
                reasons,
                assumptions,
                completeness,
                hypotheses,
                alternatives_total: total,
                extra_old: Vec::new(),
                extra_new: Vec::new(),
                location: None,
            });
            self.push(case);
        }
    }

    /// The nodes and competing hypotheses of one solver component.
    ///
    /// A component is the set of proposals that compete for the same material,
    /// which is exactly the choice a reviewer is being asked to make.
    fn component_view(
        &mut self,
        scope_index: usize,
        component_index: usize,
    ) -> (Vec<NodeId>, Vec<NodeId>, Vec<Hypothesis>, Option<usize>) {
        let scope = &self.input.comparison.scopes[scope_index].result;
        let Some(component) = scope.matching.components.get(component_index) else {
            return (Vec::new(), Vec::new(), Vec::new(), None);
        };
        let indexes = component.proposals.clone();
        if !self.budget.visit_candidates(indexes.len()) {
            return (Vec::new(), Vec::new(), Vec::new(), None);
        }
        let total = enumerated_total(scope, Some(component));
        let mut old_nodes = BTreeSet::new();
        let mut new_nodes = BTreeSet::new();
        for index in &indexes {
            if let Some(proposal) = scope.candidates.proposals.get(*index) {
                old_nodes.extend(proposal.old.iter().copied());
                new_nodes.extend(proposal.new.iter().copied());
            }
        }
        let component = component.clone();
        let hypotheses = indexes
            .iter()
            .take(self.limits.max_hypotheses_per_case)
            .filter_map(|index| self.hypothesis(scope_index, *index, Some(&component)))
            .collect();
        (
            old_nodes.into_iter().collect(),
            new_nodes.into_iter().collect(),
            hypotheses,
            total,
        )
    }

    /// The nodes and competing hypotheses around one correspondence proposal.
    fn proposal_view(
        &mut self,
        scope_index: usize,
        proposal: usize,
    ) -> (Vec<NodeId>, Vec<NodeId>, Vec<Hypothesis>, Option<usize>) {
        let scope = &self.input.comparison.scopes[scope_index].result;
        let Some(candidate) = scope.candidates.proposals.get(proposal) else {
            return (Vec::new(), Vec::new(), Vec::new(), None);
        };
        let component = scope
            .matching
            .components
            .iter()
            .find(|component| component.proposals.contains(&proposal));
        let indexes: Vec<usize> =
            component.map_or_else(|| vec![proposal], |component| component.proposals.clone());
        if !self.budget.visit_candidates(indexes.len()) {
            return (
                candidate.old.clone(),
                candidate.new.clone(),
                Vec::new(),
                None,
            );
        }
        let total = enumerated_total(scope, component);
        let hypotheses = indexes
            .iter()
            .take(self.limits.max_hypotheses_per_case)
            .filter_map(|index| self.hypothesis(scope_index, *index, component))
            .collect();
        (
            candidate.old.clone(),
            candidate.new.clone(),
            hypotheses,
            total,
        )
    }

    fn hypothesis(
        &self,
        scope_index: usize,
        proposal: usize,
        component: Option<&MatchingComponent>,
    ) -> Option<Hypothesis> {
        let scope = &self.input.comparison.scopes[scope_index].result;
        let candidate = scope.candidates.proposals.get(proposal)?;
        let conflicts = component
            .map(|component| {
                component
                    .proposals
                    .iter()
                    .filter(|index| **index != proposal)
                    .filter_map(|index| HypothesisId::new(format!("H{index}")).ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut counterevidence = Vec::new();
        if scope.matching.inferred_proposals.contains(&proposal) {
            counterevidence.push(ReasonRecord::new(ReviewReason::InferredCorrespondence));
        }
        Some(Hypothesis {
            id: HypothesisId::new(format!("H{proposal}")).ok()?,
            cardinality: Cardinality::from_counts(candidate.old.len(), candidate.new.len()),
            supplier: candidate.supplier.clone(),
            objective_weight: Some(candidate.weight),
            mandatory_in_examined_optima: component
                .is_some_and(|component| component.mandatory.contains(&proposal)),
            old: self.location(Side::Old, &candidate.old),
            new: self.location(Side::New, &candidate.new),
            old_text: self.node_text(Side::Old, &candidate.old),
            new_text: self.node_text(Side::New, &candidate.new),
            evidence: self
                .sources_of(Side::Old, &candidate.old)
                .into_iter()
                .map(|source| evidence_ref(Side::Old, source))
                .chain(
                    self.sources_of(Side::New, &candidate.new)
                        .into_iter()
                        .map(|source| evidence_ref(Side::New, source)),
                )
                .collect(),
            conflicts_with: conflicts,
            counterevidence,
        })
    }

    fn text_scope_reviews(&mut self, scope_index: usize, inferred_parent: bool) {
        let count = self.input.comparison.scopes[scope_index]
            .result
            .text_scope_reviews
            .len();
        for index in 0..count {
            let review = &self.input.comparison.scopes[scope_index]
                .result
                .text_scope_reviews[index];
            let engine_class = if inferred_parent
                || review.comparison.interpretation == InterpretationStatus::Inferred
            {
                EngineClass::Inferred
            } else {
                EngineClass::NonOwningRange
            };
            let mut reasons = vec![
                ReasonRecord::new(ReviewReason::NonOwningRangeOnly)
                    .with_message(review.convention.clone()),
            ];
            reasons.extend(review.comparison.unresolved.iter().enumerate().map(
                |(position, message)| {
                    ReasonRecord::new(
                        unresolved_classification(&review.comparison.obligations, position)
                            .reason
                            .into(),
                    )
                    .with_message(message.clone())
                },
            ));
            let completeness = CaseCompleteness {
                evidence: Completeness::Unknown,
                candidate_enumeration: Completeness::from_observation(
                    review.candidate_search_exhaustive,
                ),
                solver_search: Completeness::Unknown,
                response: Completeness::Complete,
            };
            let mut assumptions = vec![ReviewAssumption::CanonicalNormalization];
            if review.spacing.is_some() {
                assumptions.push(ReviewAssumption::ReconstructedSpacing);
            }
            if inferred_parent {
                assumptions.push(ReviewAssumption::InferredParentScope);
            }
            let old = review.comparison.old.clone();
            let new = review.comparison.new.clone();
            let case = self.build_case(CaseDraft {
                question: ReviewQuestion::CompareContent,
                engine_class,
                finding: finding_of(&review.comparison),
                old_nodes: &old,
                new_nodes: &new,
                reasons,
                assumptions,
                completeness,
                hypotheses: Vec::new(),
                alternatives_total: None,
                // A range review locates interior sources its member views do
                // not own.
                extra_old: review.old_sources.clone(),
                extra_new: review.new_sources.clone(),
                location: None,
            });
            self.push(case);
        }
    }

    fn relations(&mut self) {
        let comparison = self.input.comparison;
        for (position, message) in comparison.relation_unresolved.iter().enumerate() {
            let obligation = unresolved_classification(&comparison.relation_obligations, position);
            let reasons = vec![
                ReasonRecord::new(obligation.reason.into())
                    .with_message(message.clone())
                    .with_channel(Channel::Relations),
            ];
            let case = self.build_case(CaseDraft {
                question: ReviewQuestion::CompareRelationship,
                engine_class: EngineClass::Unavailable,
                finding: CaseFinding::NotEstablished,
                old_nodes: &[],
                new_nodes: &[],
                reasons,
                assumptions: Vec::new(),
                completeness: CaseCompleteness {
                    evidence: Completeness::Unknown,
                    candidate_enumeration: Completeness::Unknown,
                    solver_search: Completeness::Incomplete,
                    response: Completeness::Complete,
                },
                hypotheses: Vec::new(),
                alternatives_total: None,
                extra_old: Vec::new(),
                extra_new: Vec::new(),
                location: None,
            });
            self.push(case);
        }
        for relation in &comparison.relations {
            if relation.interpretation != InterpretationStatus::Inferred {
                continue;
            }
            let case = self.build_case(CaseDraft {
                question: ReviewQuestion::CompareRelationship,
                engine_class: EngineClass::Inferred,
                finding: if relation.changed() {
                    CaseFinding::DifferenceEstablished
                } else {
                    CaseFinding::NotEstablished
                },
                old_nodes: &[],
                new_nodes: &[],
                reasons: vec![
                    ReasonRecord::new(ReviewReason::InferredCorrespondence)
                        .with_channel(Channel::Relations),
                ],
                assumptions: Vec::new(),
                completeness: CaseCompleteness {
                    evidence: Completeness::Unknown,
                    candidate_enumeration: Completeness::Unknown,
                    solver_search: Completeness::Unknown,
                    response: Completeness::Complete,
                },
                hypotheses: Vec::new(),
                alternatives_total: None,
                extra_old: relation.old_sources.clone(),
                extra_new: relation.new_sources.clone(),
                location: None,
            });
            self.push(case);
        }
    }

    fn acquisition(&mut self) {
        for side in [Side::Old, Side::New] {
            let view = match side {
                Side::Old => self.input.old,
                Side::New => self.input.new,
            };
            let mut channel_gaps: BTreeMap<(Channel, String), Vec<EvidenceIssue>> = BTreeMap::new();
            for issue in &view.evidence.issues {
                if !self.input.channels.contains(&issue.channel) {
                    continue;
                }
                if issue.kind == EvidenceFailure::Unsupported && issue.sources.is_empty() {
                    // An unimplemented interpretation is a property of the
                    // channel; enumerating one gap per page would repeat the
                    // same fact without locating anything.
                    channel_gaps
                        .entry((issue.channel, issue.reason.clone()))
                        .or_default()
                        .push(issue.clone());
                    continue;
                }
                // A form obligation that names a stored field is a question
                // about that field's value and its appearance, not a failure to
                // acquire anything. The distinction is drawn from the evidence
                // the issue points at, never from its wording.
                let appearance = issue.channel == Channel::Forms
                    && issue.kind == EvidenceFailure::Unresolved
                    && issue.sources.iter().any(|source| {
                        matches!(source, SourceRef::Structured { element }
                        if view.evidence.structured.iter().any(|stored| {
                            stored.id == *element
                                && matches!(stored.value, StructuredValue::FormField { .. })
                        }))
                    });
                let reasons = vec![
                    ReasonRecord::new(if appearance {
                        ReviewReason::ValueAppearanceUnverified
                    } else {
                        issue.kind.into()
                    })
                    .with_message(issue.reason.clone())
                    .with_channel(issue.channel)
                    .with_page(issue.page)
                    .with_sources(
                        issue
                            .sources
                            .iter()
                            .map(|source| evidence_ref(side, *source))
                            .collect(),
                    ),
                ];
                let (extra_old, extra_new) = match side {
                    Side::Old => (issue.sources.clone(), Vec::new()),
                    Side::New => (Vec::new(), issue.sources.clone()),
                };
                let case = self.build_case(
                    CaseDraft {
                        question: if appearance {
                            ReviewQuestion::CheckValueAppearance
                        } else {
                            ReviewQuestion::AcquisitionGap
                        },
                        engine_class: EngineClass::Unavailable,
                        finding: CaseFinding::NotEstablished,
                        old_nodes: &[],
                        new_nodes: &[],
                        reasons,
                        assumptions: Vec::new(),
                        completeness: CaseCompleteness {
                            evidence: Completeness::Incomplete,
                            candidate_enumeration: Completeness::Unknown,
                            solver_search: Completeness::Unknown,
                            response: Completeness::Complete,
                        },
                        hypotheses: Vec::new(),
                        alternatives_total: None,
                        extra_old,
                        extra_new,
                        location: None,
                    }
                    .located(side, SideLocation::unknown(side).with_page(issue.page)),
                );
                self.push(case);
            }
            for ((channel, message), issues) in channel_gaps {
                let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
                key.field(side.label().as_bytes());
                key.debug(&channel);
                key.field(message.as_bytes());
                let gap_id = self.identifiers.gap(&key.finish());
                self.gaps.push(UnlocalizedGap {
                    gap_id,
                    scope: GapScope::Channel,
                    channel: Some(channel),
                    side: Some(side),
                    evidence_complete: Completeness::Incomplete,
                    reasons: vec![
                        ReasonRecord::new(
                            issues
                                .first()
                                .map_or(ReviewReason::UnsupportedChannel, |issue| {
                                    issue.kind.into()
                                }),
                        )
                        .with_message(message)
                        .with_channel(channel),
                    ],
                    sources: Vec::new(),
                });
            }
        }
    }

    /// Pages whose pixels exist but whose text was never acquired.
    ///
    /// These are cases, not gaps: a reviewer can answer them from an image. The
    /// case records that a text-only answer would have no basis.
    fn visual_only_pages(&mut self) {
        if !self.input.channels.contains(&Channel::Text) {
            return;
        }
        for side in [Side::Old, Side::New] {
            let view = match side {
                Side::Old => self.input.old,
                Side::New => self.input.new,
            };
            let text_pages: BTreeSet<PageId> = view
                .graph
                .nodes
                .iter()
                .filter(|node| matches!(node.content, NodeContent::Text { .. }))
                .flat_map(|node| node.pages.iter().copied())
                .collect();
            for page in &view.evidence.pages {
                if text_pages.contains(&page.page) {
                    continue;
                }
                let regions: Vec<_> = view
                    .evidence
                    .rendered
                    .iter()
                    .filter(|region| region.page == page.page)
                    .collect();
                if regions.is_empty() {
                    continue;
                }
                let mut key = CaseKey::new(ReviewQuestion::InterpretVisualRegion);
                key.field(side.label().as_bytes());
                key.debug(&page.page);
                let case_id = self.identifiers.case(&key.finish());
                let digest = case_id.as_str()[1..].to_owned();
                let evidence: Vec<EvidenceRef> = regions
                    .iter()
                    .map(|region| evidence_ref(side, SourceRef::Rendered { region: region.id }))
                    .collect();
                let location = SideLocation::unknown(side).with_page(Some(page.page));
                let case = ReviewCase {
                    content_digest: digest,
                    question: ReviewQuestion::InterpretVisualRegion,
                    pipeline: self.input.identity.pipeline,
                    engine_class: EngineClass::Unavailable,
                    // Pixels exist, but nothing was compared.
                    finding: CaseFinding::NotEstablished,
                    channels: self.input.channels.clone(),
                    completeness: CaseCompleteness {
                        evidence: Completeness::Incomplete,
                        candidate_enumeration: Completeness::Unknown,
                        solver_search: Completeness::Unknown,
                        response: Completeness::Complete,
                    },
                    reasons: vec![
                        ReasonRecord::new(ReviewReason::VisualOnlyRegion)
                            .with_channel(Channel::Text)
                            .with_page(Some(page.page)),
                    ],
                    assumptions: vec![ReviewAssumption::RenderedObservation],
                    old: (side == Side::Old).then(|| location.clone()),
                    new: (side == Side::New).then_some(location),
                    old_text: None,
                    new_text: None,
                    hypotheses: Vec::new(),
                    alternatives_total: None,
                    alternatives_returned: 0,
                    omitted: Vec::new(),
                    next_cursor: None,
                    required_evidence: vec![RequiredEvidence::Visual],
                    related_cases: Vec::new(),
                    conflicts_with: Vec::new(),
                    available_actions: vec![
                        RetrievalAction::Render {
                            case: case_id.clone(),
                        },
                        RetrievalAction::Show {
                            case: case_id.clone(),
                            detail: Detail::Context,
                            cursor: None,
                        },
                    ],
                    evidence,
                    covered: Vec::new(),
                    regions: vec![super::CaseRegion {
                        side,
                        page_number: page.page.0.saturating_add(1),
                        page_index: page.page,
                        bounds: page
                            .bounds
                            .map(|bounds| [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]),
                    }],
                    case_id,
                };
                self.push(Some(case));
            }
        }
    }

    /// Discovered references that no case above accounts for.
    ///
    /// A reference owned by a view becomes a correspondence question for that
    /// view. One that belongs to no view cannot be turned into a question, so it
    /// is retained as an explicit gap instead of being dropped.
    fn residual_sources(&mut self, accounting: &[ChannelSourceAccounting]) {
        for channel in accounting {
            for side in [Side::Old, Side::New] {
                let (view, side_accounting) = match side {
                    Side::Old => (self.input.old, &channel.old),
                    Side::New => (self.input.new, &channel.new),
                };
                let residual: Vec<SourceRef> = side_accounting
                    .uncompared()
                    .filter(|source| !self.explained.contains(&(side, *source)))
                    .collect();
                if residual.is_empty() {
                    continue;
                }
                if !self.budget.visit_sources(residual.len()) {
                    return;
                }
                let mut owners: BTreeMap<NodeId, BTreeSet<SourceRef>> = BTreeMap::new();
                let mut orphans: BTreeSet<SourceRef> = BTreeSet::new();
                for source in residual {
                    match view
                        .graph
                        .nodes
                        .iter()
                        .find(|node| node.sources.contains(&source))
                    {
                        Some(node) => {
                            owners.entry(node.id).or_default().insert(source);
                        }
                        None => {
                            orphans.insert(source);
                        }
                    }
                }
                // Unexamined material is grouped by page. Splitting it per
                // view would assert boundaries the comparison never
                // established, and would ask a reviewer the same question once
                // per paragraph; the page is the smallest unit this evidence
                // actually supports.
                let mut by_page: BTreeMap<Option<PageId>, (Vec<NodeId>, BTreeSet<SourceRef>)> =
                    BTreeMap::new();
                for (node, sources) in owners {
                    let page = self
                        .node(side, node)
                        .and_then(|node| node.pages.first().copied());
                    let entry = by_page.entry(page).or_default();
                    entry.0.push(node);
                    entry.1.extend(sources);
                }
                for (page, (members, sources)) in by_page {
                    // Members supply the quoted text and the located view; the
                    // full source set travels as evidence either way, so the
                    // obligation stays explained even when the text is bounded.
                    let quoted: Vec<NodeId> = members
                        .iter()
                        .copied()
                        .take(self.limits.max_members_per_case)
                        .collect();
                    if quoted.len() < members.len() {
                        self.budget.stop(
                            OmissionScope::Channel {
                                channel: channel.channel,
                            },
                            OmissionKind::TextOmitted,
                            Some(members.len() - quoted.len()),
                        );
                    }
                    let (old_nodes, new_nodes) = match side {
                        Side::Old => (quoted, Vec::new()),
                        Side::New => (Vec::new(), quoted),
                    };
                    let (extra_old, extra_new) = match side {
                        Side::Old => (sources.into_iter().collect(), Vec::new()),
                        Side::New => (Vec::new(), sources.into_iter().collect()),
                    };
                    let reasons = vec![
                        ReasonRecord::new(ReviewReason::DiscoveredButUnexamined)
                            .with_channel(channel.channel)
                            .with_page(page),
                    ];
                    let case = self.build_case(CaseDraft {
                        question: ReviewQuestion::ResolveCorrespondence,
                        engine_class: EngineClass::Unavailable,
                        // Discovered, never reached by any comparison.
                        finding: CaseFinding::NotExamined,
                        old_nodes: &old_nodes,
                        new_nodes: &new_nodes,
                        reasons,
                        assumptions: Vec::new(),
                        completeness: CaseCompleteness {
                            evidence: Completeness::from_flag(side_accounting.inventory_complete),
                            candidate_enumeration: Completeness::Unknown,
                            solver_search: Completeness::Unknown,
                            response: Completeness::Complete,
                        },
                        hypotheses: Vec::new(),
                        alternatives_total: None,
                        extra_old,
                        extra_new,
                        location: Some((side, SideLocation::unknown(side).with_page(page))),
                    });
                    self.push(case);
                }
                if !orphans.is_empty() {
                    if orphans.len() > self.limits.max_sources_per_case {
                        self.budget.stop(
                            OmissionScope::Channel {
                                channel: channel.channel,
                            },
                            OmissionKind::GapSourcesOmitted,
                            Some(orphans.len() - self.limits.max_sources_per_case),
                        );
                    }
                    let mut key = CaseKey::new(ReviewQuestion::AcquisitionGap);
                    key.field(b"orphan-sources");
                    key.field(side.label().as_bytes());
                    key.debug(&channel.channel);
                    key.sources(side, &orphans);
                    let gap_id = self.identifiers.gap(&key.finish());
                    self.gaps.push(UnlocalizedGap {
                        gap_id,
                        scope: GapScope::Channel,
                        channel: Some(channel.channel),
                        side: Some(side),
                        evidence_complete: Completeness::from_flag(
                            side_accounting.inventory_complete,
                        ),
                        reasons: vec![
                            ReasonRecord::new(ReviewReason::DiscoveredButUnexamined)
                                .with_channel(channel.channel),
                        ],
                        sources: orphans
                            .into_iter()
                            .take(self.limits.max_sources_per_case)
                            .map(|source| evidence_ref(side, source))
                            .collect(),
                    });
                }
            }
        }
    }
}

/// The page and retained geometry of one source reference.
fn source_geometry(
    view: DocumentView<'_>,
    source: SourceRef,
) -> Option<(PageId, Option<[f64; 4]>)> {
    let store = view.evidence;
    match source {
        SourceRef::Native { glyph } => store
            .native
            .items()
            .iter()
            .find(|item| item.id == glyph)
            .map(|item| {
                (
                    item.page,
                    Some([
                        item.bbox.min.x,
                        item.bbox.min.y,
                        item.bbox.max.x,
                        item.bbox.max.y,
                    ]),
                )
            }),
        SourceRef::NativeVector { line } => store
            .native
            .vector_lines()
            .iter()
            .find(|item| item.id == line)
            .map(|item| {
                (
                    item.page,
                    Some([
                        item.from.x.min(item.to.x),
                        item.from.y.min(item.to.y),
                        item.from.x.max(item.to.x),
                        item.from.y.max(item.to.y),
                    ]),
                )
            }),
        SourceRef::Rendered { region } => store
            .rendered
            .iter()
            .find(|item| item.id == region)
            .map(|item| (item.page, polygon_bounds(&item.polygon))),
        SourceRef::Structured { element } => store
            .structured
            .iter()
            .find(|item| item.id == element)
            .and_then(|item| {
                item.page.map(|page| {
                    (
                        page,
                        item.bounds
                            .map(|bounds| [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y]),
                    )
                })
            }),
    }
}

fn polygon_bounds(polygon: &[crate::model::Vec2]) -> Option<[f64; 4]> {
    let first = polygon.first()?;
    let mut bounds = [first.x, first.y, first.x, first.y];
    for point in polygon {
        bounds = union(bounds, [point.x, point.y, point.x, point.y]);
    }
    Some(bounds)
}

fn union(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    [
        left[0].min(right[0]),
        left[1].min(right[1]),
        left[2].max(right[2]),
        left[3].max(right[3]),
    ]
}

/// The engine's own finding for one local comparison.
///
/// An operation means a difference was established, whether or not its exact
/// position was resolved. A comparison that ran and produced none means the
/// material was equal within the range it examined. Anything else established
/// nothing.
fn finding_of(comparison: &crate::document::LocalViewComparison) -> CaseFinding {
    if comparison.operation.is_some() {
        CaseFinding::DifferenceEstablished
    } else if comparison.compared {
        CaseFinding::EqualityEstablished
    } else {
        CaseFinding::NotEstablished
    }
}

/// What a group of scope obligations is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ObligationGroup {
    /// One proposed correspondence.
    Proposal(usize),
    /// One set of proposals competing for the same material.
    Component(usize),
    /// The scope as a whole, with no narrower locator.
    Scope,
}

/// Scope obligations that name one local comparison, grouped by that index.
fn scope_obligations_by_comparison(
    scope: &ScopeViewComparison,
) -> BTreeMap<usize, Vec<ReasonRecord>> {
    let mut grouped: BTreeMap<usize, Vec<ReasonRecord>> = BTreeMap::new();
    for (position, message) in scope.unresolved.iter().enumerate() {
        let obligation: UnresolvedObligation =
            unresolved_classification(&scope.obligations, position);
        if let Some(index) = obligation.comparison {
            grouped
                .entry(index)
                .or_default()
                .push(ReasonRecord::new(obligation.reason.into()).with_message(message.clone()));
        }
    }
    grouped
}

/// The number of competing hypotheses, when enumeration actually closed.
fn enumerated_total(
    scope: &ScopeViewComparison,
    component: Option<&MatchingComponent>,
) -> Option<usize> {
    let component = component?;
    (scope.candidates.exhaustive && component.exhaustive).then_some(component.proposals.len())
}

impl From<UnresolvedReason> for ReviewReason {
    fn from(reason: UnresolvedReason) -> Self {
        match reason {
            UnresolvedReason::ScopeCandidateEnumerationIncomplete
            | UnresolvedReason::SourceCandidateEnumerationIncomplete
            | UnresolvedReason::VisualCandidateEnumerationIncomplete
            | UnresolvedReason::TextCandidateEnumerationIncomplete
            | UnresolvedReason::StructuralCandidateEnumerationIncomplete
            | UnresolvedReason::ComponentDependsOnOmittedCandidates => {
                Self::CandidateEnumerationIncomplete
            }
            UnresolvedReason::ComponentSearchBudget => Self::SearchIncomplete,
            UnresolvedReason::CompetingOptima => Self::CompetingCorrespondence,
            UnresolvedReason::UnexaminedRivals => Self::CandidateEnumerationIncomplete,
            UnresolvedReason::UnpaddedTextBoundaryOnly => Self::NonOwningRangeOnly,
            UnresolvedReason::ExtractionBoundary => Self::ExtractionGap,
            UnresolvedReason::ExtractionDependencyBudget | UnresolvedReason::LocalProofBudget => {
                Self::WorkLimit
            }
            UnresolvedReason::TextComparisonLimit => Self::WorkLimit,
            UnresolvedReason::NormalizationInterpretationMissing
            | UnresolvedReason::NormalizationInterpretationsDisagree
            | UnresolvedReason::SpacingInterpretationsRetained => Self::NormalizationUncertainty,
            UnresolvedReason::MultiplicityProofWithoutMask
            | UnresolvedReason::NativeOrderUnresolved => Self::AmbiguousEditLocation,
            UnresolvedReason::PixelMaskOutputLimit => Self::OutputLimit,
            UnresolvedReason::IncompatibleRenderProfile => Self::VisualOnlyRegion,
            UnresolvedReason::UnsupportedLocalContent => Self::DomainNotClosed,
            UnresolvedReason::InferredGroupSuggestion => Self::InferredCorrespondence,
            UnresolvedReason::RelationEndpointsUnmatched
            | UnresolvedReason::RelationGraphIncomplete
            | UnresolvedReason::CounterpartRefinementIncomplete => Self::RelationSearchIncomplete,
            UnresolvedReason::Other => Self::Other,
        }
    }
}
