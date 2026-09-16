use serde::{Deserialize, Serialize};

use super::ScopeMatching;

/// Evidence admission is separate from the weighted assignment objective.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartDecisionPolicy {
    #[default]
    PreserveUnmatchedAlternativesV1,
}

/// Competing histories of a proposal's nonempty endpoints. Neither history
/// establishes an operation or owns changed sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartExplanation {
    /// The endpoints are views of corresponding elements, possibly changed.
    Correspondence,
    /// The endpoints are independently present. Removal plus insertion is one
    /// possible history; movement, rename, or another counterpart remains open.
    SeparatePresence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterpartDecisionMissing {
    /// Similarity or inferred structure does not discriminate the histories.
    IndependentCorrespondenceEvidence,
    /// A partial or tied search cannot select a correspondence uniquely.
    ResolvedRivals,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedCounterpartDecision {
    /// Index into the enclosing scope's candidate proposals.
    pub proposal: usize,
    pub explanations: [CounterpartExplanation; 2],
    pub missing: Vec<CounterpartDecisionMissing>,
}

/// Explicit alternatives for proposals that the objective cannot justify.
/// An omitted proposal is not thereby accepted: source premises, completeness,
/// local kernels, and enclosing scope correspondence still require validation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterpartDecisions {
    pub policy: CounterpartDecisionPolicy,
    pub unresolved: Vec<UnresolvedCounterpartDecision>,
}

impl ScopeMatching {
    /// Retains an unmatched explanation independently of weight, including a
    /// positive-weight unique 1:1 optimum. This does not change the legacy
    /// matching objective or turn its zero-valued unmatched option into proof.
    /// Storage is bounded by the already bounded proposal population; ordered
    /// membership lookups do not launch another assignment search.
    #[must_use]
    pub fn counterpart_decisions(&self) -> CounterpartDecisions {
        let mut decisions = CounterpartDecisions::default();
        for component in &self.components {
            let mandatory: std::collections::BTreeSet<_> =
                component.mandatory.iter().copied().collect();
            for &proposal in &component.proposals {
                let inferred = self.inferred_proposals.contains(&proposal);
                let rivals = !mandatory.contains(&proposal);
                if !inferred && !rivals {
                    continue;
                }
                let mut missing = Vec::new();
                if inferred {
                    missing.push(CounterpartDecisionMissing::IndependentCorrespondenceEvidence);
                }
                if rivals {
                    missing.push(CounterpartDecisionMissing::ResolvedRivals);
                }
                decisions.unresolved.push(UnresolvedCounterpartDecision {
                    proposal,
                    explanations: [
                        CounterpartExplanation::Correspondence,
                        CounterpartExplanation::SeparatePresence,
                    ],
                    missing,
                });
            }
        }
        decisions
    }
}
