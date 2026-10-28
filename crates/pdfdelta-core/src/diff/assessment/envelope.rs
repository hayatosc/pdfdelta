//! Joint alternatives for a supplied local literal alignment. No value here
//! establishes source ownership, reading order, or document correspondence.

use serde::{Deserialize, Serialize};

use crate::Result;

use super::{claims::allocation_error, document_claims::LocalTextClaims};

/// Maximum retained cells, including boundary and inactive band cells.
pub const MAX_ENVELOPE_CELLS: usize = 262_144;

/// Bounds the returned graph as well as its construction. Work is supplied
/// separately by the caller and includes building the positional summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTextEnvelopeLimits {
    pub max_cells: usize,
}

impl Default for LocalTextEnvelopeLimits {
    fn default() -> Self {
        Self {
            max_cells: MAX_ENVELOPE_CELLS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeUnavailable {
    WorkLimit,
    CellLimit,
    NormalizationAlternatives,
}

/// A refusal is not an empty graph, an equality result, or a partial proof.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalTextEnvelopeResult {
    Complete { envelope: Box<LocalTextEnvelope> },
    Unavailable { reason: EnvelopeUnavailable },
}

/// Row `i` represents states `(i, new_start + offset)`. Each byte is the
/// outgoing-edge mask: 1 = equal to `(i+1,j+1)`, 2 = delete to `(i+1,j)`,
/// 4 = insert to `(i,j+1)`. Zero means no outgoing edge. Edges are retained
/// only if they lie on a globally optimal source-to-sink path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlignmentDagRow {
    pub new_start: usize,
    pub edges: Vec<u8>,
}

/// The source is `(0,0)` and sink is `(old_length,new_length)`. Every complete
/// path is a valid optimal edit script; the graph is the JOINT constraint on
/// choices. Per-position alternatives must never be multiplied independently.
/// Deserialization carries data only, never a trusted proof certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptimalAlignmentDag {
    pub old_length: usize,
    pub new_length: usize,
    pub lcs_length: usize,
    pub rows: Vec<AlignmentDagRow>,
}

/// Sorted possible counterpart positions and whether some optimal path leaves
/// this token unmatched. An empty `matches` vector means mandatory unmatched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenAlternatives {
    pub matches: Vec<usize>,
    pub can_be_unmatched: bool,
}

/// Facts conditional on exactly the supplied token sequences and order under
/// literal-minimal alignment. Source-backed masks affect counts, not the LCS
/// objective. Synthetic tokens remain in the graph but not mandatory claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalTextEnvelope {
    pub claims: LocalTextClaims,
    pub dag: OptimalAlignmentDag,
    pub old: Vec<TokenAlternatives>,
    pub new: Vec<TokenAlternatives>,
    /// Exact support where operation or counterpart varies; not a filled hull.
    pub ambiguous_old: Vec<usize>,
    pub ambiguous_new: Vec<usize>,
    /// Equal pairs present on every optimal path, including synthetic tokens.
    pub fixed_equal_pairs: Vec<[usize; 2]>,
}

fn allocated<T>(length: usize) -> Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(length)
        .map_err(|_| allocation_error("local alignment envelope"))?;
    Ok(result)
}

impl LocalTextEnvelope {
    pub(super) fn from_dag(claims: LocalTextClaims, dag: OptimalAlignmentDag) -> Result<Self> {
        let mut old = allocated(dag.old_length)?;
        old.resize_with(dag.old_length, TokenAlternatives::default);
        let mut new = allocated(dag.new_length)?;
        new.resize_with(dag.new_length, TokenAlternatives::default);
        // Count before reserving, so a repeated token never causes unbounded
        // geometric Vec growth. The caller has charged the full cell scan.
        let mut old_counts = allocated(dag.old_length)?;
        old_counts.resize(dag.old_length, 0usize);
        let mut new_counts = allocated(dag.new_length)?;
        new_counts.resize(dag.new_length, 0usize);
        for (i, row) in dag.rows.iter().enumerate() {
            for (offset, edge) in row.edges.iter().enumerate() {
                let j = row.new_start + offset;
                if edge & 1 != 0 {
                    old_counts[i] += 1;
                    new_counts[j] += 1;
                }
            }
        }
        for (entry, count) in old.iter_mut().zip(old_counts) {
            entry.matches = allocated(count)?;
        }
        for (entry, count) in new.iter_mut().zip(new_counts) {
            entry.matches = allocated(count)?;
        }
        for (i, row) in dag.rows.iter().enumerate() {
            for (offset, edge) in row.edges.iter().enumerate() {
                let j = row.new_start + offset;
                if edge & 1 != 0 {
                    old[i].matches.push(j);
                    new[j].matches.push(i);
                }
                if edge & 2 != 0 {
                    old[i].can_be_unmatched = true;
                }
                if edge & 4 != 0 {
                    new[j].can_be_unmatched = true;
                }
            }
        }
        let ambiguous = |side: &[TokenAlternatives]| -> Result<Vec<usize>> {
            let mut result = allocated(side.len())?;
            for (position, entry) in side.iter().enumerate() {
                if entry.matches.len() > 1 || (!entry.matches.is_empty() && entry.can_be_unmatched)
                {
                    result.push(position);
                }
            }
            Ok(result)
        };
        let ambiguous_old = ambiguous(&old)?;
        let ambiguous_new = ambiguous(&new)?;
        let mut fixed_equal_pairs = allocated(dag.lcs_length)?;
        for (i, entry) in old.iter().enumerate() {
            if entry.matches.len() == 1 && !entry.can_be_unmatched {
                fixed_equal_pairs.push([i, entry.matches[0]]);
            }
        }
        Ok(Self {
            claims,
            dag,
            old,
            new,
            ambiguous_old,
            ambiguous_new,
            fixed_equal_pairs,
        })
    }
}
