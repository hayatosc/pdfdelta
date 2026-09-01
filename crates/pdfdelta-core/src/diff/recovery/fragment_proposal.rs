use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ExactSingleTokenEditKind<T> {
    Deletion { old_token: T },
    Insertion { new_token: T },
    Replacement { old_token: T, new_token: T },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactSingleTokenEdit<T> {
    pub(crate) old: Range<usize>,
    pub(crate) new: Range<usize>,
    pub(crate) kind: ExactSingleTokenEditKind<T>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExactSingleTokenEditAnalysis<T> {
    pub(crate) edit: Option<ExactSingleTokenEdit<T>>,
    pub(crate) comparisons: usize,
}

/// Finds one exact changed token after removing equal, non-overlapping edges.
///
/// `safe_changed_token` is evaluated only for tokens inside the changed ranges.
/// This allows exact unmapped tokens in unchanged context while failing closed
/// when an unmapped token would become part of the proposed edit.
pub(crate) fn exact_single_token_edit<T: Clone + Eq>(
    old: &[T],
    new: &[T],
    safe_changed_token: impl Fn(&T) -> bool,
) -> ExactSingleTokenEditAnalysis<T> {
    let shorter = old.len().min(new.len());
    let mut comparisons = 0usize;
    let mut prefix = 0usize;
    while prefix < shorter {
        comparisons += 1;
        if old[prefix] != new[prefix] {
            break;
        }
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < shorter - prefix {
        let old_index = old.len() - suffix - 1;
        let new_index = new.len() - suffix - 1;
        if old_index == prefix && new_index == prefix {
            break;
        }
        comparisons += 1;
        if old[old_index] != new[new_index] {
            break;
        }
        suffix += 1;
    }

    let old_range = prefix..old.len() - suffix;
    let new_range = prefix..new.len() - suffix;
    let edit = match (old_range.len(), new_range.len()) {
        (0, 0) => None,
        (1, 0) => {
            let old_token = old[old_range.start].clone();
            safe_changed_token(&old_token).then_some(ExactSingleTokenEdit {
                old: old_range,
                new: new_range,
                kind: ExactSingleTokenEditKind::Deletion { old_token },
            })
        }
        (0, 1) => {
            let new_token = new[new_range.start].clone();
            safe_changed_token(&new_token).then_some(ExactSingleTokenEdit {
                old: old_range,
                new: new_range,
                kind: ExactSingleTokenEditKind::Insertion { new_token },
            })
        }
        (1, 1) => {
            let old_token = old[old_range.start].clone();
            let new_token = new[new_range.start].clone();
            (safe_changed_token(&old_token) && safe_changed_token(&new_token)).then_some(
                ExactSingleTokenEdit {
                    old: old_range,
                    new: new_range,
                    kind: ExactSingleTokenEditKind::Replacement {
                        old_token,
                        new_token,
                    },
                },
            )
        }
        _ => None,
    };

    ExactSingleTokenEditAnalysis { edit, comparisons }
}

#[cfg(test)]
mod tests {
    use super::{ExactSingleTokenEditKind, exact_single_token_edit};

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Token {
        Scalar(char),
        Unmapped(u16),
    }

    fn scalar(token: &Token) -> bool {
        matches!(token, Token::Scalar(_))
    }

    fn chars(value: &str) -> Vec<Token> {
        value.chars().map(Token::Scalar).collect()
    }

    fn has_one_edit(old: &[Token], new: &[Token]) -> bool {
        if old.len() == new.len() {
            return old.iter().zip(new).filter(|(old, new)| old != new).count() == 1;
        }
        if old.len() + 1 == new.len() {
            return (0..new.len()).any(|removed| {
                old.iter()
                    .eq(new[..removed].iter().chain(&new[removed + 1..]))
            });
        }
        if new.len() + 1 == old.len() {
            return (0..old.len()).any(|removed| {
                new.iter()
                    .eq(old[..removed].iter().chain(&old[removed + 1..]))
            });
        }
        false
    }

    fn binary_sequence(length: usize, bits: usize) -> Vec<Token> {
        (0..length)
            .map(|offset| Token::Scalar(if bits & (1 << offset) == 0 { 'a' } else { 'b' }))
            .collect()
    }

    #[test]
    fn matches_single_edit_oracle_exhaustively() {
        for old_len in 0..=5 {
            for new_len in 0..=5 {
                for old_bits in 0..1 << old_len {
                    for new_bits in 0..1 << new_len {
                        let old = binary_sequence(old_len, old_bits);
                        let new = binary_sequence(new_len, new_bits);
                        let analysis = exact_single_token_edit(&old, &new, scalar);
                        assert_eq!(analysis.edit.is_some(), has_one_edit(&old, &new));
                        assert!(analysis.comparisons <= old_len.min(new_len) + 1);
                    }
                }
            }
        }
    }

    #[test]
    fn reports_comma_deletion_and_insertion() {
        let old = chars("use, rather");
        let new = chars("use rather");
        let deletion = exact_single_token_edit(&old, &new, scalar)
            .edit
            .expect("comma deletion is exact");
        assert_eq!(deletion.old, 3..4);
        assert_eq!(deletion.new, 3..3);
        assert_eq!(
            deletion.kind,
            ExactSingleTokenEditKind::Deletion {
                old_token: Token::Scalar(',')
            }
        );

        let insertion = exact_single_token_edit(&new, &old, scalar)
            .edit
            .expect("comma insertion is exact");
        assert_eq!(insertion.old, 3..3);
        assert_eq!(insertion.new, 3..4);
        assert_eq!(
            insertion.kind,
            ExactSingleTokenEditKind::Insertion {
                new_token: Token::Scalar(',')
            }
        );
    }

    #[test]
    fn reports_punctuation_substitution() {
        let analysis = exact_single_token_edit(&chars("purpose."), &chars("purpose;"), scalar);
        assert_eq!(analysis.comparisons, 8);
        let edit = analysis.edit.expect("punctuation substitution is exact");
        assert_eq!(edit.old, 7..8);
        assert_eq!(edit.new, 7..8);
        assert_eq!(
            edit.kind,
            ExactSingleTokenEditKind::Replacement {
                old_token: Token::Scalar('.'),
                new_token: Token::Scalar(';')
            }
        );
    }

    #[test]
    fn rejects_identical_and_multiple_edits() {
        assert!(
            exact_single_token_edit(&chars("same"), &chars("same"), scalar)
                .edit
                .is_none()
        );
        assert!(
            exact_single_token_edit(&chars("abcd"), &chars("axyd"), scalar)
                .edit
                .is_none()
        );
    }

    #[test]
    fn allows_unmapped_context_but_rejects_unmapped_changes() {
        let context_old = [Token::Unmapped(7), Token::Scalar(','), Token::Scalar('x')];
        let context_new = [Token::Unmapped(7), Token::Scalar('x')];
        assert!(
            exact_single_token_edit(&context_old, &context_new, scalar)
                .edit
                .is_some()
        );

        let changed_old = [Token::Scalar('a'), Token::Unmapped(7)];
        let changed_new = [Token::Scalar('a')];
        assert!(
            exact_single_token_edit(&changed_old, &changed_new, scalar)
                .edit
                .is_none()
        );
        let replaced_new = [Token::Scalar('a'), Token::Unmapped(8)];
        assert!(
            exact_single_token_edit(&changed_old, &replaced_new, scalar)
                .edit
                .is_none()
        );
    }
}
