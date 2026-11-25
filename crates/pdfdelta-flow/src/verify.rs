//! Independent replay verification of token-stream edit scripts.
//!
//! Verification only needs token texts and the script. It proves, in one
//! linear pass plus one linear scan per side, that:
//!
//! * every old and every new token is covered exactly once,
//! * old tokens covered by different changes appear in ascending order,
//! * the uncovered old tokens and the uncovered new tokens are the same
//!   sequence of texts (the implicit equal pairs),
//! * both sides of every move and reading-order change are identical, and
//! * both sides of every layout-only change satisfy the layout-only relation.
//!
//! Together these facts mean replaying the script on the old sequence produces
//! exactly the new sequence.

use std::fmt;
use std::ops::Range;

use super::{FlowChange, FlowToken, script::layout_only_equal};

/// Why a script failed replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// A change covers indices outside the token sequences.
    RangeOutOfBounds {
        old: Range<usize>,
        new: Range<usize>,
        old_len: usize,
        new_len: usize,
    },
    /// A change starts before the end of the previous old range.
    OldOutOfOrder { at: usize },
    /// An old token is covered by more than one change.
    OldCoveredTwice { index: usize },
    /// A new token is covered by more than one change.
    NewCoveredTwice { index: usize },
    /// The uncovered old and new token counts differ.
    UncoveredLengthMismatch {
        old_remaining: usize,
        new_remaining: usize,
    },
    /// Paired uncovered tokens are not equal.
    ContentMismatch { old_index: usize, new_index: usize },
    /// A move's old and new runs are not identical.
    MoveMismatch {
        old: Range<usize>,
        new: Range<usize>,
    },
    /// A reading-order change's old and new runs are not identical.
    ReadingOrderMismatch {
        old: Range<usize>,
        new: Range<usize>,
    },
    /// A layout-only change is not layout-only.
    LayoutOnlyMismatch {
        old: Range<usize>,
        new: Range<usize>,
    },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RangeOutOfBounds {
                old,
                new,
                old_len,
                new_len,
            } => write!(
                formatter,
                "change ranges {old:?} and {new:?} exceed token lengths {old_len} and {new_len}"
            ),
            Self::OldOutOfOrder { at } => {
                write!(
                    formatter,
                    "old range starts at {at} before the previous range ends"
                )
            }
            Self::OldCoveredTwice { index } => {
                write!(formatter, "old token {index} is covered twice")
            }
            Self::NewCoveredTwice { index } => {
                write!(formatter, "new token {index} is covered twice")
            }
            Self::UncoveredLengthMismatch {
                old_remaining,
                new_remaining,
            } => write!(
                formatter,
                "{old_remaining} uncovered old tokens and {new_remaining} uncovered new tokens"
            ),
            Self::ContentMismatch {
                old_index,
                new_index,
            } => write!(
                formatter,
                "uncovered old token {old_index} differs from new token {new_index}"
            ),
            Self::MoveMismatch { old, new } => {
                write!(
                    formatter,
                    "move sides {old:?} and {new:?} are not identical"
                )
            }
            Self::ReadingOrderMismatch { old, new } => {
                write!(
                    formatter,
                    "reading-order sides {old:?} and {new:?} are not identical"
                )
            }
            Self::LayoutOnlyMismatch { old, new } => {
                write!(formatter, "ranges {old:?} and {new:?} are not layout-only")
            }
        }
    }
}

impl std::error::Error for VerifyError {}

/// Replays `script` against both token sequences.
///
/// # Errors
///
/// Returns the first violated invariant as a [`VerifyError`].
pub fn verify(
    old: &[FlowToken],
    new: &[FlowToken],
    script: &[FlowChange],
) -> Result<(), VerifyError> {
    let mut old_covered = vec![false; old.len()];
    let mut new_covered = vec![false; new.len()];
    let mut previous_old_end = 0;
    for change in script {
        let old_range = change.old_range();
        let new_range = change.new_range();
        if old_range.start > old_range.end
            || old_range.end > old.len()
            || new_range.start > new_range.end
            || new_range.end > new.len()
        {
            return Err(VerifyError::RangeOutOfBounds {
                old: old_range,
                new: new_range,
                old_len: old.len(),
                new_len: new.len(),
            });
        }
        if !old_range.is_empty() {
            if old_range.start < previous_old_end {
                return Err(VerifyError::OldOutOfOrder {
                    at: old_range.start,
                });
            }
            previous_old_end = old_range.end;
        }
        mark(&mut old_covered, &old_range, true)?;
        mark(&mut new_covered, &new_range, false)?;
        match change {
            FlowChange::Move {
                old: old_side,
                new: new_side,
            } => {
                if old_side.len() != new_side.len()
                    || !texts_equal(&old[old_side.clone()], &new[new_side.clone()])
                {
                    return Err(VerifyError::MoveMismatch {
                        old: old_side.clone(),
                        new: new_side.clone(),
                    });
                }
            }
            FlowChange::ReadingOrder {
                old: old_side,
                new: new_side,
            } if !texts_equal(&old[old_side.clone()], &new[new_side.clone()]) => {
                return Err(VerifyError::ReadingOrderMismatch {
                    old: old_side.clone(),
                    new: new_side.clone(),
                });
            }
            FlowChange::LayoutOnly {
                old: old_side,
                new: new_side,
            } if !layout_only_equal(&old[old_side.clone()], &new[new_side.clone()]) => {
                return Err(VerifyError::LayoutOnlyMismatch {
                    old: old_side.clone(),
                    new: new_side.clone(),
                });
            }
            _ => {}
        }
    }
    let old_remaining = uncovered(&old_covered);
    let new_remaining = uncovered(&new_covered);
    if old_remaining.len() != new_remaining.len() {
        return Err(VerifyError::UncoveredLengthMismatch {
            old_remaining: old_remaining.len(),
            new_remaining: new_remaining.len(),
        });
    }
    for (&old_index, &new_index) in old_remaining.iter().zip(&new_remaining) {
        if old[old_index].text != new[new_index].text {
            return Err(VerifyError::ContentMismatch {
                old_index,
                new_index,
            });
        }
    }
    Ok(())
}

fn mark(covered: &mut [bool], range: &Range<usize>, is_old: bool) -> Result<(), VerifyError> {
    for (offset, flag) in covered[range.clone()].iter_mut().enumerate() {
        if *flag {
            return Err(if is_old {
                VerifyError::OldCoveredTwice {
                    index: range.start + offset,
                }
            } else {
                VerifyError::NewCoveredTwice {
                    index: range.start + offset,
                }
            });
        }
        *flag = true;
    }
    Ok(())
}

fn uncovered(covered: &[bool]) -> Vec<usize> {
    covered
        .iter()
        .enumerate()
        .filter_map(|(index, &is_covered)| (!is_covered).then_some(index))
        .collect()
}

fn texts_equal(old: &[FlowToken], new: &[FlowToken]) -> bool {
    old.len() == new.len()
        && old
            .iter()
            .zip(new)
            .all(|(old_token, new_token)| old_token.text == new_token.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::model::PageId;

    fn tokens(texts: &[&str]) -> Vec<FlowToken> {
        texts
            .iter()
            .map(|&text| FlowToken {
                text: text.to_owned(),
                page: PageId(0),
                bounds: None,
                block: 0,
                uncertain: false,
                line_break_after: false,
                font_size: None,
                raw: None,
                position: None,
                page_break_after: false,
                normalization: 0,
            })
            .collect()
    }

    #[test]
    fn rejects_ranges_outside_the_sequences() {
        let error = verify(
            &tokens(&["a"]),
            &tokens(&["a"]),
            &[FlowChange::Insert { new: 0..2 }],
        )
        .expect_err("the script must be rejected");
        assert!(matches!(error, VerifyError::RangeOutOfBounds { .. }));
    }

    #[test]
    fn rejects_old_ranges_out_of_order() {
        let script = [
            FlowChange::Delete { old: 2..3 },
            FlowChange::Delete { old: 1..2 },
        ];
        let error = verify(&tokens(&["a", "b", "c"]), &tokens(&[]), &script)
            .expect_err("the script must be rejected");
        assert_eq!(error, VerifyError::OldOutOfOrder { at: 1 });
    }

    #[test]
    fn rejects_double_covered_new_tokens() {
        let script = [
            FlowChange::Replace {
                old: 1..2,
                new: 0..1,
            },
            FlowChange::Replace {
                old: 2..3,
                new: 0..1,
            },
        ];
        let error = verify(&tokens(&["a", "b", "c"]), &tokens(&["b"]), &script)
            .expect_err("the script must be rejected");
        assert_eq!(error, VerifyError::NewCoveredTwice { index: 0 });
    }

    #[test]
    fn rejects_unequal_uncovered_counts() {
        let error = verify(&tokens(&["a", "b"]), &tokens(&["a"]), &[])
            .expect_err("the script must be rejected");
        assert_eq!(
            error,
            VerifyError::UncoveredLengthMismatch {
                old_remaining: 2,
                new_remaining: 1,
            }
        );
    }

    #[test]
    fn rejects_differing_uncovered_texts() {
        let error = verify(&tokens(&["a", "b"]), &tokens(&["a", "c"]), &[])
            .expect_err("the script must be rejected");
        assert_eq!(
            error,
            VerifyError::ContentMismatch {
                old_index: 1,
                new_index: 1,
            }
        );
    }

    #[test]
    fn rejects_moves_whose_sides_differ() {
        let script = [FlowChange::Move {
            old: 0..2,
            new: 0..2,
        }];
        let error = verify(&tokens(&["a", "b"]), &tokens(&["a", "c"]), &script)
            .expect_err("the script must be rejected");
        assert!(matches!(error, VerifyError::MoveMismatch { .. }));
    }

    #[test]
    fn rejects_layout_only_changes_that_alter_text() {
        let script = [FlowChange::LayoutOnly {
            old: 0..1,
            new: 0..1,
        }];
        let error = verify(&tokens(&["a"]), &tokens(&["b"]), &script)
            .expect_err("the script must be rejected");
        assert!(matches!(error, VerifyError::LayoutOnlyMismatch { .. }));
    }

    #[test]
    fn accepts_a_reading_order_change_beside_uncovered_equal_tokens() {
        let script = [FlowChange::ReadingOrder {
            old: 0..1,
            new: 1..2,
        }];
        verify(&tokens(&["a", "b"]), &tokens(&["b", "a"]), &script)
            .expect("reading-order sides are identical");
    }

    #[test]
    fn rejects_reading_order_changes_that_alter_text() {
        let script = [FlowChange::ReadingOrder {
            old: 0..1,
            new: 0..1,
        }];
        let error = verify(&tokens(&["a"]), &tokens(&["b"]), &script)
            .expect_err("the script must be rejected");
        assert!(matches!(error, VerifyError::ReadingOrderMismatch { .. }));
    }

    #[test]
    fn accepts_a_move_beside_uncovered_equal_tokens() {
        let script = [FlowChange::Move {
            old: 0..1,
            new: 2..3,
        }];
        verify(
            &tokens(&["a", "b", "c"]),
            &tokens(&["b", "c", "a"]),
            &script,
        )
        .expect("move plus equal uncovered tokens replays");
    }
}
