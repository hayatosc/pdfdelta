//! External assessments returned by a host agent, and the checks they must pass
//! before they may be stored.
//!
//! A decision is a review note about one case. It is not an acceptance API: it
//! never changes the engine's comparison, its coverage, or its exit status, and
//! a schema-valid decision is still only an interpretation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    case::{RequiredEvidence, ReviewCase},
    contract::{BundleId, CaseId, Detail, HypothesisId, Side},
};

/// What a reviewer concluded about one case.
///
/// There is no implicit fourth state: an unanswered or rejected decision is
/// never completed to `UnchangedInScope`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    /// Content differs under the selected correspondence.
    Changed,
    /// Nothing differs within the examined range under the selected
    /// correspondence. This says nothing about the rest of the document.
    UnchangedInScope,
    /// A decision is possible, but more evidence must be retrieved first.
    NeedMoreEvidence,
    /// The available evidence cannot settle the question.
    Undetermined,
}

/// What kind of difference a reviewer identified.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionChangeKind {
    /// Wording differs.
    Wording,
    /// A number, date, unit, or other value differs.
    Value,
    /// A relationship between elements differs.
    Relationship,
    /// Visual content differs.
    VisualContent,
    /// Only presentation differs; the content is the same.
    PresentationOnly,
    /// Several of the above apply together.
    Mixed,
}

/// A retrieval a reviewer is asking for before deciding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalRequest {
    pub detail: Detail,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub case: Option<CaseId>,
    /// Why the retrieval is needed, in the reviewer's own words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One external assessment.
///
/// The reviewer returns a conclusion, a short rationale, references, and
/// limitations. A long chain of reasoning is not requested and not stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDecision {
    pub schema: String,
    pub bundle_id: BundleId,
    pub case_id: CaseId,
    pub status: DecisionStatus,
    /// Selected hypotheses, which may legitimately be empty: "none of these"
    /// and "insufficient evidence" are always admissible answers.
    #[serde(default)]
    pub selected_hypotheses: Vec<HypothesisId>,
    #[serde(default)]
    pub change_kinds: Vec<DecisionChangeKind>,
    /// Qualified aliases such as `old:E31`, resolved against the same bundle.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub limitations: Vec<String>,
    #[serde(default)]
    pub requests: Vec<RetrievalRequest>,
}

/// Why a decision cannot be stored as given.
///
/// Rejections are explicit. A decision that fails any check is never repaired
/// by dropping the offending field.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecisionRejection {
    /// The decision names another schema version.
    SchemaMismatch { found: String },
    /// The decision belongs to another bundle, or to a stale one.
    BundleMismatch { found: BundleId },
    /// The decision names a case this bundle does not contain.
    UnknownCase { case: CaseId },
    /// The decision selects a hypothesis the case does not offer.
    UnknownHypothesis { hypothesis: HypothesisId },
    /// The decision selects hypotheses the case declares incompatible.
    ConflictingHypotheses {
        left: HypothesisId,
        right: HypothesisId,
    },
    /// An evidence reference is not `old:ALIAS` or `new:ALIAS`.
    MalformedEvidenceRef { reference: String },
    /// An evidence reference names material this case does not quote.
    UnknownEvidenceRef { reference: String },
    /// An evidence reference names the other document's side.
    EvidenceSideMismatch { reference: String, expected: Side },
    /// A conclusion was asserted although the case still requires retrieval
    /// that the reviewer did not perform and did not request.
    ConclusionWithoutRequiredEvidence { required: RequiredEvidence },
    /// `Changed` was asserted without naming any kind of change.
    ChangedWithoutKind,
    /// `NeedMoreEvidence` was asserted without requesting anything.
    NeedMoreEvidenceWithoutRequest,
    /// Two decisions cite the same evidence with opposite conclusions.
    ContradictoryClaims {
        reference: String,
        changed: CaseId,
        unchanged: CaseId,
    },
}

impl std::fmt::Display for DecisionRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaMismatch { found } => {
                write!(formatter, "decision schema {found:?} is not supported")
            }
            Self::BundleMismatch { found } => {
                write!(formatter, "decision belongs to bundle {found}")
            }
            Self::UnknownCase { case } => write!(formatter, "unknown case {case}"),
            Self::UnknownHypothesis { hypothesis } => {
                write!(formatter, "unknown hypothesis {hypothesis}")
            }
            Self::ConflictingHypotheses { left, right } => write!(
                formatter,
                "hypotheses {left} and {right} cannot both be selected"
            ),
            Self::MalformedEvidenceRef { reference } => {
                write!(formatter, "evidence reference {reference:?} is malformed")
            }
            Self::UnknownEvidenceRef { reference } => {
                write!(
                    formatter,
                    "evidence reference {reference:?} is not in the case"
                )
            }
            Self::EvidenceSideMismatch {
                reference,
                expected,
            } => write!(
                formatter,
                "evidence reference {reference:?} is not on the {} side",
                expected.label()
            ),
            Self::ConclusionWithoutRequiredEvidence { required } => write!(
                formatter,
                "the case still requires {required:?} evidence for a conclusion"
            ),
            Self::ChangedWithoutKind => {
                formatter.write_str("a changed decision must name at least one change kind")
            }
            Self::NeedMoreEvidenceWithoutRequest => {
                formatter.write_str("a need-more-evidence decision must request a retrieval")
            }
            Self::ContradictoryClaims {
                reference,
                changed,
                unchanged,
            } => write!(
                formatter,
                "{reference:?} is cited as changed by {changed} and unchanged by {unchanged}"
            ),
        }
    }
}

impl std::error::Error for DecisionRejection {}

/// Checks one decision against the case and bundle it claims to answer.
///
/// This validates identity, reference integrity, and internal consistency. It
/// cannot validate that the conclusion is correct: schema conformance is not
/// semantic correctness, and no check here promotes a decision to a proof.
///
/// # Errors
/// Returns every failed check rather than the first, so a host can repair a
/// decision in one pass.
pub fn validate_decision(
    decision: &AgentDecision,
    case: &ReviewCase,
    bundle: &BundleId,
) -> Result<(), Vec<DecisionRejection>> {
    let mut rejections = Vec::new();
    if decision.schema != super::contract::DECISION_SCHEMA {
        rejections.push(DecisionRejection::SchemaMismatch {
            found: decision.schema.clone(),
        });
    }
    if &decision.bundle_id != bundle {
        rejections.push(DecisionRejection::BundleMismatch {
            found: decision.bundle_id.clone(),
        });
    }
    if decision.case_id != case.case_id {
        rejections.push(DecisionRejection::UnknownCase {
            case: decision.case_id.clone(),
        });
    }

    for selected in &decision.selected_hypotheses {
        if !case
            .hypotheses
            .iter()
            .any(|hypothesis| &hypothesis.id == selected)
        {
            rejections.push(DecisionRejection::UnknownHypothesis {
                hypothesis: selected.clone(),
            });
        }
    }
    for hypothesis in &case.hypotheses {
        if !decision.selected_hypotheses.contains(&hypothesis.id) {
            continue;
        }
        for conflict in &hypothesis.conflicts_with {
            if decision.selected_hypotheses.contains(conflict) {
                let (left, right) = if hypothesis.id <= *conflict {
                    (hypothesis.id.clone(), conflict.clone())
                } else {
                    (conflict.clone(), hypothesis.id.clone())
                };
                let rejection = DecisionRejection::ConflictingHypotheses { left, right };
                if !rejections.contains(&rejection) {
                    rejections.push(rejection);
                }
            }
        }
    }

    for reference in &decision.evidence_refs {
        let Some((side, alias)) = reference.split_once(':') else {
            rejections.push(DecisionRejection::MalformedEvidenceRef {
                reference: reference.clone(),
            });
            continue;
        };
        let side = match side {
            "old" => Side::Old,
            "new" => Side::New,
            _ => {
                rejections.push(DecisionRejection::MalformedEvidenceRef {
                    reference: reference.clone(),
                });
                continue;
            }
        };
        let matched = case
            .evidence
            .iter()
            .filter(|candidate| candidate.alias().as_str() == alias)
            .collect::<Vec<_>>();
        if matched.is_empty() {
            rejections.push(DecisionRejection::UnknownEvidenceRef {
                reference: reference.clone(),
            });
        } else if !matched.iter().any(|candidate| candidate.side == side) {
            rejections.push(DecisionRejection::EvidenceSideMismatch {
                reference: reference.clone(),
                expected: matched[0].side,
            });
        }
    }

    match decision.status {
        DecisionStatus::Changed if decision.change_kinds.is_empty() => {
            rejections.push(DecisionRejection::ChangedWithoutKind);
        }
        DecisionStatus::NeedMoreEvidence if decision.requests.is_empty() => {
            rejections.push(DecisionRejection::NeedMoreEvidenceWithoutRequest);
        }
        DecisionStatus::Changed | DecisionStatus::UnchangedInScope => {
            if let Some(required) = case
                .required_evidence
                .iter()
                .find(|required| !matches!(required, RequiredEvidence::Unavailable))
            {
                rejections.push(DecisionRejection::ConclusionWithoutRequiredEvidence {
                    required: *required,
                });
            }
        }
        DecisionStatus::NeedMoreEvidence | DecisionStatus::Undetermined => {}
    }

    if rejections.is_empty() {
        Ok(())
    } else {
        Err(rejections)
    }
}

/// One decision's verdict after validation.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionOutcome {
    /// Position of the decision in the submitted set.
    pub index: usize,
    pub case_id: CaseId,
    pub rejections: Vec<DecisionRejection>,
}

/// Checks a whole set of decisions against the cases they answer.
///
/// Each decision is checked on its own, and then the set is checked for claims
/// that cannot both hold: the same evidence cannot be cited as changed by one
/// decision and unchanged by another. Contradictions are returned rather than
/// resolved, because choosing between two external answers is not something
/// this library can do from the evidence.
#[must_use]
pub fn validate_decisions(
    decisions: &[AgentDecision],
    cases: &[ReviewCase],
    bundle: &BundleId,
) -> Vec<DecisionOutcome> {
    let mut outcomes: Vec<DecisionOutcome> = decisions
        .iter()
        .enumerate()
        .map(|(index, decision)| {
            let rejections = cases
                .iter()
                .find(|case| case.case_id == decision.case_id)
                .map_or_else(
                    || {
                        vec![DecisionRejection::UnknownCase {
                            case: decision.case_id.clone(),
                        }]
                    },
                    |case| {
                        validate_decision(decision, case, bundle)
                            .err()
                            .unwrap_or_default()
                    },
                );
            DecisionOutcome {
                index,
                case_id: decision.case_id.clone(),
                rejections,
            }
        })
        .collect();

    let cited = |decision: &AgentDecision| -> BTreeSet<String> {
        decision.evidence_refs.iter().cloned().collect()
    };
    for (left, decision) in decisions.iter().enumerate() {
        if decision.status != DecisionStatus::Changed {
            continue;
        }
        let changed = cited(decision);
        for (right, other) in decisions.iter().enumerate() {
            if right == left || other.status != DecisionStatus::UnchangedInScope {
                continue;
            }
            for reference in changed.intersection(&cited(other)) {
                let rejection = DecisionRejection::ContradictoryClaims {
                    reference: reference.clone(),
                    changed: decision.case_id.clone(),
                    unchanged: other.case_id.clone(),
                };
                for outcome in [left, right] {
                    if !outcomes[outcome].rejections.contains(&rejection) {
                        outcomes[outcome].rejections.push(rejection.clone());
                    }
                }
            }
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::{
        document::SourceRef,
        model::GlyphId,
        review::contract::{
            CaseCompleteness, DECISION_SCHEMA, EngineClass, EvidenceRef, PipelineContract,
            ReviewQuestion,
        },
    };

    fn case() -> ReviewCase {
        ReviewCase {
            case_id: CaseId::new("R17").expect("case id"),
            content_digest: "00".repeat(8),
            question: ReviewQuestion::CompareContent,
            pipeline: PipelineContract::SharedEvidence,
            engine_class: EngineClass::Strict,
            channels: BTreeSet::new(),
            completeness: CaseCompleteness::unknown(),
            reasons: Vec::new(),
            assumptions: Vec::new(),
            old: None,
            new: None,
            old_text: None,
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
            regions: Vec::new(),
            evidence: vec![EvidenceRef::new(
                Side::Old,
                SourceRef::Native { glyph: GlyphId(31) },
            )],
        }
    }

    fn decision() -> AgentDecision {
        AgentDecision {
            schema: DECISION_SCHEMA.into(),
            bundle_id: BundleId::new("b0").expect("bundle id"),
            case_id: CaseId::new("R17").expect("case id"),
            status: DecisionStatus::Changed,
            selected_hypotheses: Vec::new(),
            change_kinds: vec![DecisionChangeKind::Value],
            evidence_refs: vec!["old:g31".into()],
            rationale: "The deadline value differs.".into(),
            limitations: Vec::new(),
            requests: Vec::new(),
        }
    }

    #[test]
    fn accepts_a_consistent_decision() {
        let bundle = BundleId::new("b0").expect("bundle id");
        assert_eq!(validate_decision(&decision(), &case(), &bundle), Ok(()));
    }

    #[test]
    fn rejects_a_reference_from_the_other_side() {
        let bundle = BundleId::new("b0").expect("bundle id");
        let mut decision = decision();
        decision.evidence_refs = vec!["new:g31".into()];
        let rejections = validate_decision(&decision, &case(), &bundle).expect_err("rejected");
        assert!(
            rejections.contains(&DecisionRejection::EvidenceSideMismatch {
                reference: "new:g31".into(),
                expected: Side::Old,
            })
        );
    }

    #[test]
    fn rejects_a_stale_bundle_and_reports_every_failed_check() {
        let bundle = BundleId::new("b1").expect("bundle id");
        let mut decision = decision();
        decision.change_kinds.clear();
        decision.evidence_refs = vec!["sideways".into()];
        let rejections = validate_decision(&decision, &case(), &bundle).expect_err("rejected");
        assert!(rejections.len() >= 3, "{rejections:?}");
        assert!(
            rejections
                .iter()
                .any(|rejection| matches!(rejection, DecisionRejection::BundleMismatch { .. }))
        );
        assert!(
            rejections
                .iter()
                .any(|rejection| matches!(rejection, DecisionRejection::ChangedWithoutKind))
        );
        assert!(
            rejections.iter().any(|rejection| matches!(
                rejection,
                DecisionRejection::MalformedEvidenceRef { .. }
            ))
        );
    }

    #[test]
    fn opposite_claims_about_the_same_evidence_are_returned_as_a_conflict() {
        let bundle = BundleId::new("b0").expect("bundle id");
        let mut second = case();
        second.case_id = CaseId::new("R18").expect("case id");

        let changed = decision();
        let mut unchanged = decision();
        unchanged.case_id = CaseId::new("R18").expect("case id");
        unchanged.status = DecisionStatus::UnchangedInScope;
        unchanged.change_kinds.clear();

        let outcomes = validate_decisions(&[changed, unchanged], &[case(), second], &bundle);
        assert_eq!(outcomes.len(), 2);
        for outcome in &outcomes {
            assert!(
                outcome.rejections.iter().any(|rejection| matches!(
                    rejection,
                    DecisionRejection::ContradictoryClaims { .. }
                )),
                "both sides of a contradiction are reported: {outcome:?}"
            );
        }
    }

    #[test]
    fn an_unrelated_pair_of_decisions_passes_the_set_check() {
        let bundle = BundleId::new("b0").expect("bundle id");
        let mut second = case();
        second.case_id = CaseId::new("R18").expect("case id");
        second.evidence[0] = EvidenceRef::new(Side::Old, SourceRef::Native { glyph: GlyphId(99) });

        let mut other = decision();
        other.case_id = CaseId::new("R18").expect("case id");
        other.status = DecisionStatus::UnchangedInScope;
        other.change_kinds.clear();
        other.evidence_refs = vec!["old:g99".into()];

        let outcomes = validate_decisions(&[decision(), other], &[case(), second], &bundle);
        assert!(
            outcomes.iter().all(|outcome| outcome.rejections.is_empty()),
            "{outcomes:?}"
        );
    }

    #[test]
    fn an_unanswered_requirement_blocks_a_conclusion_but_not_a_hold() {
        let bundle = BundleId::new("b0").expect("bundle id");
        let mut case = case();
        case.required_evidence = vec![RequiredEvidence::Visual];

        let rejections = validate_decision(&decision(), &case, &bundle).expect_err("rejected");
        assert!(
            rejections.contains(&DecisionRejection::ConclusionWithoutRequiredEvidence {
                required: RequiredEvidence::Visual,
            })
        );

        let mut held = decision();
        held.status = DecisionStatus::Undetermined;
        held.change_kinds.clear();
        assert_eq!(validate_decision(&held, &case, &bundle), Ok(()));
    }
}
