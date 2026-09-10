use pdfdelta_core::{
    Error,
    diff::{LocalTextSide, local_text_claims},
    normalize::ComparableToken,
};

fn tokens(text: &str) -> Vec<ComparableToken> {
    text.chars().map(ComparableToken::Scalar).collect()
}

#[test]
fn source_mask_length_must_match_tokens() {
    let text = tokens("ab");
    let error = local_text_claims(
        LocalTextSide {
            tokens: &text,
            source: &[true; 1],
            optional: &[false; 2],
        },
        LocalTextSide {
            tokens: &text,
            source: &[true; 2],
            optional: &[false; 2],
        },
        &mut 10_000,
    )
    .expect_err("a short source mask must be rejected instead of silently truncated");
    assert!(matches!(error, Error::InvalidConfiguration(_)));
}

#[test]
fn repeated_text_has_exact_quantity_without_an_invented_position() {
    let old = tokens("AAA");
    let new = tokens("AA");
    let claims = local_text_claims(
        LocalTextSide {
            tokens: &old,
            source: &[true; 3],
            optional: &[false; 3],
        },
        LocalTextSide {
            tokens: &new,
            source: &[true; 2],
            optional: &[false; 2],
        },
        &mut 10_000,
    )
    .expect("valid local claim input and sufficient proof budget")
    .expect("valid local claim input and sufficient proof budget");
    assert_eq!(claims.changed_source_lower, 1);
    assert_eq!(claims.changed_source_upper, 1);
    assert_eq!(claims.mandatory_old, vec![false; 3]);
    assert_eq!(claims.mandatory_new, vec![false; 2]);
}

#[test]
fn synthetic_separator_cannot_inflate_changed_source_masks() {
    let old = tokens("a b");
    let new = tokens("ab");
    let claims = local_text_claims(
        LocalTextSide {
            tokens: &old,
            source: &[true, false, true],
            optional: &[false; 3],
        },
        LocalTextSide {
            tokens: &new,
            source: &[true; 2],
            optional: &[false; 2],
        },
        &mut 10_000,
    )
    .expect("valid local claim input and sufficient proof budget")
    .expect("valid local claim input and sufficient proof budget");
    assert_eq!(claims.changed_source_lower, 0);
    assert_eq!(claims.changed_source_upper, 0);
    assert_eq!(claims.mandatory_old, vec![false; 3]);
}

#[test]
fn optional_normalization_quantifies_every_interpretation() {
    let old = tokens("a-b");
    let new = tokens("ab");
    let claims = local_text_claims(
        LocalTextSide {
            tokens: &old,
            source: &[true; 3],
            optional: &[false, true, false],
        },
        LocalTextSide {
            tokens: &new,
            source: &[true; 2],
            optional: &[false; 2],
        },
        &mut 10_000,
    )
    .expect("valid local claim input and sufficient proof budget")
    .expect("valid local claim input and sufficient proof budget");
    assert_eq!(claims.normalization_pairs, 2);
    assert_eq!(claims.changed_source_lower, 0);
    assert_eq!(claims.changed_source_upper, 1);
    assert_eq!(claims.mandatory_old, vec![false; 3]);
    assert!(
        local_text_claims(
            LocalTextSide {
                tokens: &old,
                source: &[true; 3],
                optional: &[false, true, false]
            },
            LocalTextSide {
                tokens: &new,
                source: &[true; 2],
                optional: &[false; 2]
            },
            &mut 0,
        )
        .expect("valid local claim input and sufficient proof budget")
        .is_none()
    );
}
