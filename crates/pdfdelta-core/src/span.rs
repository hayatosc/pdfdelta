//! Source-neutral span vocabulary shared by comparison and reporting layers.
//!
//! These types describe the shape of a change or an inter-block join. They do
//! not establish correspondence, localization, or comparison completeness.

use crate::normalize::ComparableToken;

/// Semantic change kind used by reports, fixtures, and the flow review model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Replacement,
    Insertion,
    Deletion,
    Move,
}

/// Join applied between adjacent blocks when their text is concatenated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlockSeparator {
    Concatenate,
    Space,
    /// Independent joins for a three-block alignment group (the maximum group size).
    /// `true` inserts a space; `false` concatenates. Uniform groups retain the
    /// corresponding scalar variant, including groups produced by recovery.
    PerBoundary([bool; 2]),
}

impl BlockSeparator {
    /// Returns the separator at a zero-based inter-block boundary.
    ///
    /// # Panics
    ///
    /// Panics if a per-boundary pattern is used outside its three-block group.
    #[must_use]
    pub fn at(self, boundary: usize) -> Self {
        match self {
            Self::PerBoundary(spaces) => {
                if spaces[boundary] {
                    Self::Space
                } else {
                    Self::Concatenate
                }
            }
            uniform => uniform,
        }
    }

    pub(crate) fn append(self, combined: &mut Vec<ComparableToken>, next: &[ComparableToken]) {
        let insert_space = self == Self::Space
            && !combined.last().is_some_and(is_space)
            && !next.first().is_some_and(is_space);
        if insert_space {
            combined.push(ComparableToken::Scalar(' '));
        }
        combined.extend_from_slice(next);
    }
}

pub(crate) fn is_space(token: &ComparableToken) -> bool {
    matches!(token, ComparableToken::Scalar(scalar) if scalar.is_whitespace())
}
