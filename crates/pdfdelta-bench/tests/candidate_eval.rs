use pdfdelta_bench::{
    candidate_eval::evaluate_candidate_generation,
    canonical::{CanonicalDocument, Paragraph},
    cases::{BenchmarkCase, built_in_cases},
    mutation::Mutation,
    renderers::RendererKind,
};

/// A 20-paragraph document whose character 3-grams are shared only inside
/// four groups of five paragraphs, so the inverted index returns a bounded
/// candidate set (group members plus same-suffix paragraphs) instead of
/// every new block. Common function words would otherwise share n-grams
/// across the whole document and hide the pruning effect.
fn grouped_document() -> CanonicalDocument {
    let prefixes = ["aaaaaa", "bbbbbb", "cccccc", "dddddd"];
    let suffixes = ["efgh", "ijkl", "mnop", "qrst", "uvwx"];
    let paragraphs = prefixes
        .iter()
        .enumerate()
        .flat_map(|(group, prefix)| {
            suffixes.iter().enumerate().map(move |(position, suffix)| {
                let index = group * suffixes.len() + position;
                Paragraph::new(format!("p{index:02}"), format!("{prefix}{suffix}"))
                    .expect("valid paragraph")
            })
        })
        .collect();
    CanonicalDocument::new(paragraphs).expect("valid document")
}

#[test]
fn built_in_fixtures_recover_true_counterparts_at_top_k() {
    let top_k = [5, 10];
    for case in built_in_cases().expect("built-in cases are valid") {
        let record = evaluate_candidate_generation(&case, RendererKind::LopdfTj, &top_k)
            .expect("candidate evaluation completes");
        assert!(
            record.counterpart_old_blocks > 0,
            "{} must have at least one counterpart old block",
            record.case_name
        );
        assert_eq!(
            record.counterpart_old_blocks + record.unmatched_old_blocks,
            record.old_blocks,
            "{} counterpart and unmatched counts must partition old blocks",
            record.case_name
        );
        assert_eq!(
            record.recall_at_k.last(),
            Some(&1.0),
            "{} inverted-index recall@{}",
            record.case_name,
            top_k.last().expect("top_k is nonempty")
        );
        assert_eq!(
            record.oracle_recall_at_k.last(),
            Some(&1.0),
            "{} exhaustive recall@{}",
            record.case_name,
            top_k.last().expect("top_k is nonempty")
        );
        if case.name() != "double-line-wrap-only" {
            assert_eq!(
                record.minhash_recall_at_k.last(),
                Some(&1.0),
                "{} minhash recall@{}",
                record.case_name,
                top_k.last().expect("top_k is nonempty")
            );
        } else {
            assert!(
                record.minhash_recall_at_k.last().copied().unwrap_or(0.0) >= 0.6,
                "{} minhash recall@{}",
                record.case_name,
                top_k.last().expect("top_k is nonempty")
            );
        }
        assert!(
            record.candidate_count_max <= record.oracle_candidate_count_max,
            "{} inverted-index candidate count exceeds exhaustive",
            record.case_name
        );
        assert!(
            record.minhash_candidate_count_max <= record.oracle_candidate_count_max,
            "{} minhash candidate count exceeds exhaustive",
            record.case_name
        );
    }
}

#[test]
fn built_in_fixture_counterpart_denominators_match_mutation_semantics() {
    for case in built_in_cases().expect("built-in cases are valid") {
        let record = evaluate_candidate_generation(&case, RendererKind::LopdfTj, &[5])
            .expect("candidate evaluation completes");
        // Only the deleted paragraph loses its counterpart; every other
        // built-in mutation keeps all old paragraphs on the new side.
        let expected_unmatched = usize::from(case.name() == "paragraph-deletion");
        assert_eq!(
            record.unmatched_old_blocks, expected_unmatched,
            "{} unmatched old blocks",
            record.case_name
        );
        assert_eq!(
            record.counterpart_old_blocks,
            record.old_blocks - expected_unmatched,
            "{} counterpart old blocks",
            record.case_name
        );
    }
}

#[test]
fn paragraph_move_counterpart_is_recovered_in_top_k() {
    let case = built_in_cases()
        .expect("built-in cases are valid")
        .into_iter()
        .find(|case| case.name() == "paragraph-move")
        .expect("paragraph-move case exists");
    let record = evaluate_candidate_generation(&case, RendererKind::LopdfTj, &[5])
        .expect("candidate evaluation completes");

    // The moved paragraph keeps its canonical id, so every old block must
    // have a counterpart; a cross-document span-overlap definition would
    // drop the moved block and silently inflate recall.
    assert_eq!(record.unmatched_old_blocks, 0);
    assert_eq!(record.counterpart_old_blocks, record.old_blocks);
    assert_eq!(record.recall_at_k, [1.0]);
    assert_eq!(record.minhash_recall_at_k, [1.0]);
    assert_eq!(record.oracle_recall_at_k, [1.0]);
}

#[test]
fn inverted_index_prunes_candidates_without_losing_recall() {
    let document = grouped_document();
    let case = BenchmarkCase::new(
        "grouped-replacement",
        document,
        Mutation::TextReplace {
            paragraph_id: "p00".to_owned(),
            new_text: "aaaaaaefgi".to_owned(),
        },
        30,
    )
    .expect("valid benchmark case");

    // K=2 keeps the top-K comparison meaningful: every old block has more
    // candidates than K, so recall requires the true counterpart to rank
    // above unrelated group and suffix peers.
    let top_k = [2, 5];
    let record = evaluate_candidate_generation(&case, RendererKind::LopdfTj, &top_k)
        .expect("candidate evaluation completes");

    // Every old paragraph survives the replacement, so the recall
    // denominator is the full old block count.
    assert_eq!(record.unmatched_old_blocks, 0);
    assert_eq!(record.counterpart_old_blocks, record.old_blocks);

    // The inverted index must return strictly fewer candidates than the
    // exhaustive oracle (which returns every new block), demonstrating that
    // candidate counts are pruned rather than trivially saturated.
    assert!(
        record.candidate_count_max < record.oracle_candidate_count_max,
        "inverted index must prune candidates ({} vs {})",
        record.candidate_count_max,
        record.oracle_candidate_count_max
    );
    assert!(
        record.candidate_count_p50 < record.oracle_candidate_count_p50,
        "inverted index must prune candidates at the median"
    );

    // Pruning must not cost recall: every true counterpart stays in top-K.
    assert_eq!(record.recall_at_k, vec![1.0, 1.0]);
    assert_eq!(record.minhash_recall_at_k, vec![1.0, 1.0]);
    assert_eq!(record.oracle_recall_at_k, vec![1.0, 1.0]);
}

#[test]
fn minhash_lsh_recovers_counterparts_and_bounds_candidates() {
    let document = grouped_document();
    let case = BenchmarkCase::new(
        "grouped-replacement-minhash",
        document,
        Mutation::TextReplace {
            paragraph_id: "p00".to_owned(),
            new_text: "aaaaaaefgi".to_owned(),
        },
        30,
    )
    .expect("valid benchmark case");

    let top_k = [2, 5];
    let record = evaluate_candidate_generation(&case, RendererKind::LopdfTj, &top_k)
        .expect("candidate evaluation completes");

    assert_eq!(record.unmatched_old_blocks, 0);
    assert_eq!(record.counterpart_old_blocks, record.old_blocks);
    assert_eq!(record.minhash_recall_at_k, vec![1.0, 1.0]);
    assert!(
        record.minhash_candidate_count_max <= record.oracle_candidate_count_max,
        "minhash candidates must not exceed exhaustive oracle"
    );
    assert!(
        record.minhash_estimated_visits_upper_bound_total > 0,
        "minhash estimated visits must be tracked"
    );
}
