//! Incidence indexing connects every ownership dependency without constructing
//! the clique of pairwise exclusions around each shared endpoint.

use std::collections::{BTreeMap, BTreeSet};

use super::{DocumentGraph, MatchingLimits, Ownership, SourceRef, charge_ownership};
use crate::Result;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Node(super::NodeId),
    Source(SourceRef),
    Physical(usize),
    Partition(usize),
}

pub(super) fn dependencies(
    ownership: &[(Ownership, Ownership)],
    old: &DocumentGraph,
    new: &DocumentGraph,
    eligible: &mut [bool],
    limits: MatchingLimits,
    budget: &mut usize,
) -> Result<Vec<BTreeSet<usize>>> {
    let mut dependencies = vec![BTreeSet::new(); ownership.len()];
    for (is_old, graph) in [(true, old), (false, new)] {
        let mut physical = BTreeMap::<SourceRef, Vec<usize>>::new();
        for (index, conflict) in graph.source_conflicts.iter().enumerate() {
            charge_ownership(budget, limits)?;
            for source in &conflict.sources {
                charge_ownership(budget, limits)?;
                physical.entry(*source).or_default().push(index);
            }
        }
        let mut first = BTreeMap::<Key, usize>::new();
        for (index, pair) in ownership.iter().enumerate() {
            let side = if is_old { &pair.0 } else { &pair.1 };
            let mut connect = |key: Key| -> Result<()> {
                charge_ownership(budget, limits)?;
                if let Some(prior) = first.get(&key).copied() {
                    if prior == index {
                        return Ok(());
                    }
                    dependencies[index].insert(prior);
                    dependencies[prior].insert(index);
                    let other = if is_old {
                        &ownership[prior].0
                    } else {
                        &ownership[prior].1
                    };
                    // Sharing evidence is an endpoint exclusion only if both
                    // proposals refer to the same independent leaf endpoint.
                    if matches!(key, Key::Source(_) | Key::Physical(_)) && side.nodes != other.nodes
                    {
                        eligible[index] = false;
                        eligible[prior] = false;
                    }
                } else {
                    first.insert(key, index);
                }
                Ok(())
            };
            for node in &side.nodes {
                connect(Key::Node(*node))?;
            }
            for source in &side.sources {
                connect(Key::Source(*source))?;
                for conflict in physical.get(source).into_iter().flatten() {
                    connect(Key::Physical(*conflict))?;
                }
            }
            for group in side.partitions.keys() {
                connect(Key::Partition(*group))?;
            }
        }
    }
    Ok(dependencies)
}
