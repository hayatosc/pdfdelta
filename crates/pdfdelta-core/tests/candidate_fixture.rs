use pdfdelta_core::{
    Error,
    alignment::{
        CandidateGenerator, CandidateSource, ExhaustiveCandidateGenerator,
        InvertedIndexCandidateGenerator, build_block_features, exact_anchors,
    },
    layout::BlockId,
    normalize::{BlockText, ComparableToken, MappedText},
};

#[test]
fn builds_exact_features_from_canonical_not_masked_matching_text() {
    let old = block_text(
        1,
        "The archive contains 10 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let new = block_text(
        2,
        "The archive contains 20 files.",
        "The archive contains <NUM> files.",
        true,
    );
    let features = build_block_features(&[old, new], 3).expect("features should build");

    assert_ne!(features[0].exact_hash, features[1].exact_hash);
    assert_ne!(features[0].canonical_tokens, features[1].canonical_tokens);
    assert_eq!(features[0].matching_tokens, features[1].matching_tokens);
    assert_eq!(features[0].ngrams, features[1].ngrams);
}

#[test]
fn selects_only_unique_long_exact_blocks_as_anchors() {
    let old = build_block_features(
        &[
            block_text(1, "Unique introduction", "Unique introduction", false),
            block_text(2, "Repeated paragraph", "Repeated paragraph", false),
            block_text(3, "Repeated paragraph", "Repeated paragraph", false),
            block_text(4, "tiny", "tiny", false),
        ],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(10, "Unique introduction", "Unique introduction", false),
            block_text(11, "Repeated paragraph", "Repeated paragraph", false),
            block_text(12, "Repeated paragraph", "Repeated paragraph", false),
            block_text(13, "tiny", "tiny", false),
        ],
        3,
    )
    .expect("new features should build");

    let anchors = exact_anchors(&old, &new, 8).expect("anchors should resolve");

    assert_eq!(anchors.len(), 1);
    assert_eq!(anchors[0].old, BlockId(1));
    assert_eq!(anchors[0].new, BlockId(10));
}

#[test]
fn ranks_verified_exact_matches_first_in_the_inverted_index() {
    let old = build_block_features(
        &[block_text(1, "alpha beta gamma", "alpha beta gamma", false)],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(10, "alpha zeta gamma", "alpha zeta gamma", false),
            block_text(11, "unrelated content", "unrelated content", false),
            block_text(12, "alpha beta gamma", "alpha beta gamma", false),
        ],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    let candidates = generator
        .candidates(&old[0], 2)
        .expect("candidate query should succeed");

    assert_eq!(candidates[0].block, BlockId(12));
    assert_eq!(candidates[0].coarse_score, 1.0);
    assert!(candidates[0].sources.contains(&CandidateSource::Exact));
    assert!(
        candidates[0]
            .sources
            .contains(&CandidateSource::NGramInvertedIndex)
    );
}

#[test]
fn masked_number_matches_are_candidates_but_never_exact_sources() {
    let matching = "The archive contains <NUM> files.";
    let old = build_block_features(
        &[block_text(
            1,
            "The archive contains 10 files.",
            matching,
            true,
        )],
        3,
    )
    .expect("old features should build");
    let new = build_block_features(
        &[block_text(
            2,
            "The archive contains 20 files.",
            matching,
            true,
        )],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    let candidates = generator
        .candidates(&old[0], 1)
        .expect("candidate query should succeed");

    assert_eq!(candidates[0].block, BlockId(2));
    assert_eq!(candidates[0].sources, [CandidateSource::NGramInvertedIndex]);
}

#[test]
fn indexes_short_blocks_as_one_content_gram() {
    let old = build_block_features(&[block_text(1, "id", "id", false)], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(2, "id", "id", false)], 3)
        .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    let candidates = generator
        .candidates(&old[0], 1)
        .expect("candidate query should succeed");

    assert_eq!(candidates[0].block, BlockId(2));
    assert!(candidates[0].sources.contains(&CandidateSource::Exact));
}

#[test]
fn keeps_edited_short_blocks_in_the_candidate_set() {
    let old = build_block_features(&[block_text(1, "id", "id", false)], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(2, "ip", "ip", false),
            block_text(3, "ux", "ux", false),
        ],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    let candidates = generator
        .candidates(&old[0], 2)
        .expect("candidate query should succeed");

    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0].block, BlockId(2));
    assert!(
        candidates[0]
            .sources
            .contains(&CandidateSource::NGramInvertedIndex)
    );
    assert!(candidates.iter().all(|candidate| {
        candidate
            .sources
            .contains(&CandidateSource::ShortBlockFallback)
    }));
}

#[test]
fn estimates_repeated_short_block_visits_conservatively() {
    let old = build_block_features(&[block_text(1, "id", "id", false)], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(2, "id", "id", false),
            block_text(3, "id", "id", false),
            block_text(4, "id", "id", false),
        ],
        3,
    )
    .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    assert_eq!(
        generator
            .estimated_visits(&old[0], 3)
            .expect("visit estimate should succeed"),
        15
    );
}

#[test]
fn falls_back_for_single_token_replacements_without_shared_content() {
    let old = build_block_features(&[block_text(1, "A", "A", false)], 3)
        .expect("old features should build");
    let new = build_block_features(&[block_text(2, "B", "B", false)], 3)
        .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    let candidates = generator
        .candidates(&old[0], 1)
        .expect("candidate query should succeed");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].block, BlockId(2));
    assert_eq!(candidates[0].coarse_score, 0.0);
    assert_eq!(candidates[0].sources, [CandidateSource::ShortBlockFallback]);
}

#[test]
fn does_not_treat_long_text_as_short_when_using_unigrams() {
    let old = build_block_features(&[block_text(1, "AAAA", "AAAA", false)], 1)
        .expect("old features should build");
    let new = build_block_features(&[block_text(2, "BBBB", "BBBB", false)], 1)
        .expect("new features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&new).expect("index should be constructed");

    assert!(
        generator
            .candidates(&old[0], 1)
            .expect("candidate query should succeed")
            .is_empty()
    );
}

#[test]
fn exhaustive_generator_remains_an_all_pair_oracle() {
    let old = build_block_features(&[block_text(1, "alpha beta", "alpha beta", false)], 3)
        .expect("old features should build");
    let new = build_block_features(
        &[
            block_text(10, "unrelated", "unrelated", false),
            block_text(11, "alpha beta", "alpha beta", false),
            block_text(12, "alpha zeta", "alpha zeta", false),
        ],
        3,
    )
    .expect("new features should build");
    let generator = ExhaustiveCandidateGenerator::new(&new).expect("oracle should be constructed");

    let candidates = generator
        .candidates(&old[0], usize::MAX)
        .expect("candidate query should succeed");

    assert_eq!(candidates.len(), new.len());
    assert_eq!(
        generator
            .estimated_visits(&old[0], usize::MAX)
            .expect("visit estimate should succeed"),
        new.len()
    );
    assert_eq!(candidates[0].block, BlockId(11));
    assert_eq!(candidates[0].coarse_score, 1.0);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.sources == [CandidateSource::Exhaustive])
    );
}

#[test]
fn validates_feature_and_generator_configuration() {
    let block = block_text(1, "text", "text", false);
    assert!(matches!(
        build_block_features(std::slice::from_ref(&block), 0),
        Err(Error::InvalidConfiguration(message)) if message.contains("ngram_size")
    ));
    assert!(matches!(
        build_block_features(&[block.clone(), block], 3),
        Err(Error::Unresolved(message)) if message.contains("duplicate")
    ));

    let features = build_block_features(&[block_text(1, "text", "text", false)], 3)
        .expect("features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&features).expect("index should be constructed");
    assert!(
        generator
            .candidates(&features[0], 0)
            .expect("candidate query should succeed")
            .is_empty()
    );
    assert!(matches!(
        exact_anchors(&features, &features, 0),
        Err(Error::InvalidConfiguration(message)) if message.contains("min_token_count")
    ));

    let query = build_block_features(&[block_text(2, "abcd", "abcd", false)], 3)
        .expect("query features should build");
    let indexed = build_block_features(&[block_text(3, "abxd", "abxd", false)], 2)
        .expect("indexed features should build");
    let generator =
        InvertedIndexCandidateGenerator::new(&indexed).expect("index should be constructed");
    assert!(matches!(
        generator.candidates(&query[0], 1),
        Err(Error::InvalidConfiguration(message)) if message.contains("same ngram_size")
    ));

    let mut mixed = query;
    mixed.extend(indexed);
    assert!(matches!(
        InvertedIndexCandidateGenerator::new(&mixed),
        Err(Error::InvalidConfiguration(message)) if message.contains("one non-zero ngram_size")
    ));
}

fn block_text(id: u64, canonical: &str, matching: &str, numeric_mask_applied: bool) -> BlockText {
    BlockText {
        block: BlockId(id),
        raw: mapped_text(canonical),
        canonical: mapped_text(canonical),
        matching: matching.to_owned(),
        matching_tokens: matching.chars().map(ComparableToken::Scalar).collect(),
        numeric_mask_applied,
        normalization_events: vec![],
        issues: vec![],
        pages: Vec::new(),
    }
}

fn mapped_text(text: &str) -> MappedText {
    MappedText {
        text: text.to_owned(),
        source_map: vec![],
        unmapped: vec![],
    }
}
