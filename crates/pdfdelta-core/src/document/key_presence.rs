//! Presence under raw PDF key contracts, independently of character equality.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    BackendKind, EvidenceStore, KeyDomain, SourceRef, StructuredEvidence, StructuredValue,
};

/// A key's absence does not imply that its former value disappeared elsewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPresencePolicy {
    NativeDocumentKeysV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceSide {
    Old,
    New,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingKeyEvidence {
    CompletePopulation,
    UniqueKeys,
    NamedMembers,
    RenameOrRepartitionResolution,
    RivalSearch,
}

/// Supported acquisition cannot currently prove semantic identity after renaming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyEvidenceAction {
    ReextractNativePopulation,
    RebuildNativeGraph,
    IncreaseSearchBudget,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyWorkStatus {
    Unavailable,
    Incomplete,
    Limited,
}

/// A document-domain obligation. Claim indexes refer to this report's entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvidenceObligation {
    pub domain: KeyDomain,
    pub side: PresenceSide,
    pub sources: Vec<SourceRef>,
    pub dependent_claims: Vec<usize>,
    pub missing: MissingKeyEvidence,
    pub next_action: KeyEvidenceAction,
    pub work: KeyWorkStatus,
}

/// References an inspected native population in the indicated immutable revision.
/// Members include unnamed elements. A report is descriptive evidence, not an
/// authorization to reuse an absence certificate with a modified store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPopulationWitness {
    pub revision: String,
    pub domain: KeyDomain,
    pub backend: usize,
    pub members: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeyCounterpart {
    Matched {
        source: SourceRef,
    },
    /// Present locally; the opposite population could not establish a counterpart.
    PresentOnly,
    /// The raw key is absent from the entire opposite domain, not merely a page.
    AbsentInDocument {
        population: usize,
    },
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPresenceClaim {
    pub side: PresenceSide,
    pub domain: KeyDomain,
    pub source: SourceRef,
    /// Exact name UTF-8 bytes or exact PDF structure ID bytes, never graph labels.
    pub key: Vec<u8>,
    pub counterpart: KeyCounterpart,
}

/// Key claims own only structured identity evidence. They do not mark glyphs or
/// saved-value characters changed, and cannot discharge text-channel coverage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyPresenceComparison {
    pub policy: KeyPresencePolicy,
    pub old_revision: String,
    pub new_revision: String,
    pub populations: Vec<KeyPopulationWitness>,
    pub claims: Vec<KeyPresenceClaim>,
    pub obligations: Vec<KeyEvidenceObligation>,
    pub work: usize,
    pub exhaustive: bool,
    /// Graph membership and identity ownership checked against the same inputs.
    #[serde(default)]
    pub scoped: Option<super::ScopedKeyComparison>,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyPresenceLimits {
    /// Includes raw key bytes, population members, emitted references, and the
    /// subsequent scoped graph validation/operation work when requested.
    pub max_work: usize,
}

impl Default for KeyPresenceLimits {
    fn default() -> Self {
        Self {
            max_work: 1_000_000,
        }
    }
}

struct Population<'a> {
    backend: Option<usize>,
    members: Vec<SourceRef>,
    keys: BTreeMap<&'a [u8], Vec<SourceRef>>,
    missing: Option<MissingKeyEvidence>,
}

pub(super) fn raw_key(value: &StructuredValue, domain: KeyDomain) -> Option<Option<&[u8]>> {
    match (value, domain) {
        (StructuredValue::FormField { name, .. }, KeyDomain::PdfFieldName) => {
            Some((!name.is_empty()).then_some(name.as_bytes()))
        }
        (StructuredValue::StructureElement { identifier, .. }, KeyDomain::PdfStructureId) => {
            Some(identifier.as_deref().filter(|key| !key.is_empty()))
        }
        _ => None,
    }
}

fn population(store: &EvidenceStore, domain: KeyDomain) -> Population<'_> {
    let inventories: Vec<_> = store
        .key_inventories
        .iter()
        .filter(|inventory| inventory.domain == domain)
        .collect();
    let backend = (inventories.len() == 1).then(|| inventories[0].backend);
    let mut result = Population {
        backend,
        members: Vec::new(),
        keys: BTreeMap::new(),
        missing: (!inventories.first().is_some_and(|inventory| {
            inventories.len() == 1
                && inventory.complete
                && store
                    .backends
                    .get(inventory.backend)
                    .is_some_and(|backend| backend.kind == BackendKind::NativeParser)
        }))
        .then_some(MissingKeyEvidence::CompletePopulation),
    };
    for StructuredEvidence {
        id,
        backend: origin,
        value,
        ..
    } in &store.structured
    {
        let Some(key) = raw_key(value, domain) else {
            continue;
        };
        if Some(*origin) != backend {
            result.missing = Some(MissingKeyEvidence::CompletePopulation);
        }
        let source = SourceRef::Structured { element: *id };
        result.members.push(source);
        match key {
            Some(key) => result.keys.entry(key).or_default().push(source),
            None => {
                result
                    .missing
                    .get_or_insert(MissingKeyEvidence::NamedMembers);
            }
        }
    }
    if result.keys.values().any(|members| members.len() != 1) {
        result.missing.get_or_insert(MissingKeyEvidence::UniqueKeys);
    }
    result
}

/// Compares validated native identity populations across the whole document.
/// Both sides must be closed, uniquely keyed populations before key absence is
/// emitted. Simultaneous removals and additions retain the rename/repartition
/// alternative. Unknown/unkeyed members and absent inventories remain unknown.
///
/// This does not establish graph scope correspondence or semantic element
/// insertion/deletion. Callers must validate those premises independently before
/// deriving element operations. No character mask is produced here.
#[must_use]
pub fn compare_document_keys(
    old: &EvidenceStore,
    new: &EvidenceStore,
    domains: &BTreeSet<KeyDomain>,
    limits: KeyPresenceLimits,
) -> KeyPresenceComparison {
    let mut result = KeyPresenceComparison {
        policy: KeyPresencePolicy::NativeDocumentKeysV1,
        old_revision: old.revision.clone(),
        new_revision: new.revision.clone(),
        populations: Vec::new(),
        claims: Vec::new(),
        obligations: Vec::new(),
        work: 0,
        exhaustive: true,
        scoped: None,
    };
    for domain in domains {
        // Charge before indexes and output allocations. The factor covers both
        // directional claims, population membership, and obligation references.
        let cost = domain_work(
            old,
            new,
            *domain,
            limits.max_work.saturating_sub(result.work),
        );
        let next = cost.and_then(|cost| result.work.checked_add(cost));
        if next.is_none_or(|work| work > limits.max_work) {
            result.exhaustive = false;
            result.obligations.push(KeyEvidenceObligation {
                domain: *domain,
                side: PresenceSide::Old,
                sources: Vec::new(),
                dependent_claims: Vec::new(),
                missing: MissingKeyEvidence::RivalSearch,
                next_action: KeyEvidenceAction::IncreaseSearchBudget,
                work: KeyWorkStatus::Limited,
            });
            continue;
        }
        result.work = next.unwrap_or(result.work);
        let a = population(old, *domain);
        let b = population(new, *domain);
        let renamed = a.keys.keys().any(|key| !b.keys.contains_key(key))
            && b.keys.keys().any(|key| !a.keys.contains_key(key));
        for (side, own, other, opposite) in [
            (PresenceSide::Old, &a, &b, new),
            (PresenceSide::New, &b, &a, old),
        ] {
            let missing = own.missing.or(other.missing);
            let witness = if missing.is_none() {
                other.backend.map(|backend| {
                    let index = result.populations.len();
                    result.populations.push(KeyPopulationWitness {
                        revision: opposite.revision.clone(),
                        domain: *domain,
                        backend,
                        members: other.members.clone(),
                    });
                    index
                })
            } else {
                None
            };
            let mut pending: BTreeMap<MissingKeyEvidence, (Vec<usize>, Vec<SourceRef>)> =
                BTreeMap::new();
            for (key, members) in &own.keys {
                for source in members {
                    let rival = other.keys.get(key);
                    let reason = missing.or_else(|| {
                        (rival.is_none() && renamed)
                            .then_some(MissingKeyEvidence::RenameOrRepartitionResolution)
                    });
                    let counterpart = if let Some(reason) = reason {
                        let entry = pending.entry(reason).or_default();
                        entry.0.push(result.claims.len());
                        entry.1.push(*source);
                        if rival.is_none() {
                            KeyCounterpart::PresentOnly
                        } else {
                            KeyCounterpart::Unknown
                        }
                    } else if let Some(rival) = rival {
                        KeyCounterpart::Matched { source: rival[0] }
                    } else if let Some(population) = witness {
                        KeyCounterpart::AbsentInDocument { population }
                    } else {
                        KeyCounterpart::Unknown
                    };
                    result.claims.push(KeyPresenceClaim {
                        side,
                        domain: *domain,
                        source: *source,
                        key: key.to_vec(),
                        counterpart,
                    });
                }
            }
            if pending.is_empty()
                && let Some(reason) = missing
            {
                pending.insert(reason, (Vec::new(), own.members.clone()));
            }
            for (missing, (dependent_claims, sources)) in pending {
                let (next_action, work) = match missing {
                    MissingKeyEvidence::CompletePopulation => (
                        KeyEvidenceAction::ReextractNativePopulation,
                        KeyWorkStatus::Incomplete,
                    ),
                    _ => (KeyEvidenceAction::Unavailable, KeyWorkStatus::Unavailable),
                };
                result.obligations.push(KeyEvidenceObligation {
                    domain: *domain,
                    side,
                    sources,
                    dependent_claims,
                    missing,
                    next_action,
                    work,
                });
            }
        }
    }
    result
}

fn domain_work(
    old: &EvidenceStore,
    new: &EvidenceStore,
    domain: KeyDomain,
    limit: usize,
) -> Option<usize> {
    let inventories = old
        .key_inventories
        .len()
        .checked_add(new.key_inventories.len())?;
    let members = old.structured.len().checked_add(new.structured.len())?;
    if members.checked_mul(4)?.checked_add(inventories)? > limit {
        return None;
    }
    old.structured
        .iter()
        .chain(&new.structured)
        .try_fold(inventories, |sum, element| {
            let bytes = raw_key(&element.value, domain)
                .flatten()
                .map_or(0, <[u8]>::len);
            let sum = sum.checked_add(bytes.checked_add(4)?)?;
            (sum <= limit).then_some(sum)
        })
}
