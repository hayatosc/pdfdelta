use pdfdelta_core::{
    Error,
    layout::{Block, BlockId, BlockRole, Line, LineId, SyntheticSpace},
    model::{
        DecodedText, Document, FontId, FontProgramHash, Glyph, GlyphId, GlyphProvenance, PageId,
        Rect, TextRenderMode, Vec2,
    },
    normalize::{
        ComparableToken, MappedText, NormalizationIssueKind, NormalizationKind, ScalarRange,
        TextSource, TextSourceAtom, UnmappedToken, normalize_blocks,
    },
    pdf::ObjectRef,
};

#[test]
fn preserves_reversible_raw_text_and_synthetic_spaces() {
    let document = Document::new(vec![glyph(1, mapped("A"), 0), glyph(2, mapped("B"), 0)]);
    let lines = vec![line(
        1,
        0,
        vec![GlyphId(1), GlyphId(2)],
        vec![SyntheticSpace {
            preceding: GlyphId(1),
            following: GlyphId(2),
        }],
    )];
    let text = normalize(&document, &lines, &[body_block(1, vec![LineId(1)])]);

    assert_eq!(text.raw.text, "A B");
    assert_eq!(text.canonical.text, "A B");
    for scalar_index in 0..text.raw.text.chars().count() {
        assert!(text.raw.source_map.iter().any(|entry| {
            entry.output_range.start <= scalar_index && scalar_index < entry.output_range.end
        }));
    }
    assert!(text.raw.source_map.iter().any(|entry| {
        entry.output_range == ScalarRange { start: 1, end: 2 }
            && entry.source.atoms
                == [TextSourceAtom::SyntheticSpace {
                    preceding: GlyphId(1),
                    following: GlyphId(2),
                }]
    }));
}

#[test]
fn resolves_cjk_and_latin_soft_line_breaks() {
    let cjk = normalize_mapped_lines(&["設定を", "保存する"]);
    let latin = normalize_mapped_lines(&["project", "report"]);
    let explicit_space = normalize_mapped_lines(&["project ", "report"]);

    assert_eq!(cjk.canonical.text, "設定を保存する");
    assert_eq!(latin.canonical.text, "project report");
    assert_eq!(explicit_space.canonical.text, "project report");
    assert!(cjk.normalization_events.iter().any(|event| {
        event.kind == NormalizationKind::SoftLineBreak
            && event.canonical_range.start == event.canonical_range.end
    }));
}

#[test]
fn joins_mixed_script_boundaries_without_flagging_ambiguity() {
    let latin_then_cjk = normalize_mapped_lines(&["更新されたPDF", "ファイルを開く"]);
    let wrapped = normalize_mapped_lines(&["更新されたPDFファ", "イルを開く"]);
    let cjk_then_latin = normalize_mapped_lines(&["バックエンドの", "statusを確認"]);

    assert_eq!(latin_then_cjk.canonical.text, "更新されたPDFファイルを開く");
    assert_eq!(cjk_then_latin.canonical.text, "バックエンドのstatusを確認");
    assert_eq!(wrapped.canonical.text, "更新されたPDFファイルを開く");
    assert!(
        wrapped
            .issues
            .iter()
            .all(|issue| issue.kind != NormalizationIssueKind::AmbiguousLineBreak)
    );
    assert!(
        wrapped
            .normalization_events
            .iter()
            .any(|event| event.kind == NormalizationKind::SoftLineBreak)
    );
}

#[test]
fn joins_wraps_after_japanese_line_end_punctuation_without_ambiguity() {
    let full_stop = normalize_mapped_lines(&["設定を保存した。", "次の画面を開く"]);
    let comma = normalize_mapped_lines(&["ファイルを選び、", "処理を続ける"]);

    assert_eq!(full_stop.canonical.text, "設定を保存した。次の画面を開く");
    assert_eq!(comma.canonical.text, "ファイルを選び、処理を続ける");
    for text in [&full_stop, &comma] {
        assert!(
            text.issues
                .iter()
                .all(|issue| issue.kind != NormalizationIssueKind::AmbiguousLineBreak)
        );
    }
}

#[test]
fn separates_decimal_digits_across_soft_line_breaks_like_ascii_digits() {
    let fullwidth = normalize_mapped_lines(&["１２３", "４５６"]);
    let ascii = normalize_mapped_lines(&["123", "456"]);
    let han_numerals = normalize_mapped_lines(&["五", "十"]);

    assert_eq!(fullwidth.canonical.text, "１２３ ４５６");
    assert_eq!(ascii.canonical.text, "123 456");
    assert_eq!(han_numerals.canonical.text, "五十");
    assert!(fullwidth.normalization_events.iter().any(|event| {
        event.kind == NormalizationKind::SoftLineBreak
            && event.canonical_range == ScalarRange { start: 3, end: 4 }
    }));
    assert!(
        fullwidth
            .issues
            .iter()
            .all(|issue| issue.kind != NormalizationIssueKind::AmbiguousLineBreak)
    );
}

#[test]
fn joins_line_end_hyphenation_and_records_deleted_evidence() {
    let text = normalize_mapped_lines(&["adminis-", "tration"]);

    assert_eq!(text.raw.text, "adminis-\ntration");
    assert_eq!(text.canonical.text, "administration");
    let event = text
        .normalization_events
        .iter()
        .find(|event| event.kind == NormalizationKind::HyphenationJoin)
        .expect("hyphenation event should be retained");
    assert_eq!(event.raw_range, ScalarRange { start: 7, end: 9 });
    assert_eq!(event.canonical_range, ScalarRange { start: 7, end: 7 });
    assert_eq!(event.source.atoms.len(), 2);
}

#[test]
fn collapses_explicit_and_synthetic_whitespace_with_all_sources() {
    let document = Document::new(vec![
        glyph(1, mapped("A"), 0),
        glyph(2, mapped(" "), 0),
        glyph(3, mapped("B"), 0),
    ]);
    let lines = vec![line(
        1,
        0,
        vec![GlyphId(1), GlyphId(2), GlyphId(3)],
        vec![SyntheticSpace {
            preceding: GlyphId(2),
            following: GlyphId(3),
        }],
    )];
    let text = normalize(&document, &lines, &[body_block(1, vec![LineId(1)])]);

    assert_eq!(text.raw.text, "A  B");
    assert_eq!(text.canonical.text, "A B");
    let event = text
        .normalization_events
        .iter()
        .find(|event| event.kind == NormalizationKind::WhitespaceCollapse)
        .expect("whitespace event should be retained");
    assert_eq!(event.raw_range, ScalarRange { start: 1, end: 3 });
    assert!(
        event
            .source
            .atoms
            .contains(&TextSourceAtom::Glyph(GlyphId(2)))
    );
    assert!(event.source.atoms.iter().any(|source| matches!(
        source,
        TextSourceAtom::SyntheticSpace {
            preceding: GlyphId(2),
            following: GlyphId(3)
        }
    )));
}

#[test]
fn applies_nfc_with_scalar_ranges_and_combined_glyph_sources() {
    let text = normalize_mapped_lines(&["e\u{301}"]);

    assert_eq!(text.raw.text, "e\u{301}");
    assert_eq!(text.canonical.text, "é");
    assert_eq!(
        text.canonical.source_map[0].output_range,
        ScalarRange { start: 0, end: 1 }
    );
    assert_eq!(text.canonical.source_map[0].source.atoms.len(), 2);
    let event = text
        .normalization_events
        .iter()
        .find(|event| event.kind == NormalizationKind::Nfc)
        .expect("NFC event should be retained");
    assert_eq!(event.raw_range, ScalarRange { start: 0, end: 2 });
    assert_eq!(event.canonical_range, ScalarRange { start: 0, end: 1 });
}

#[test]
fn expands_typographic_ligatures_without_losing_the_glyph_source() {
    let text = normalize_mapped_lines(&["oﬃce"]);

    assert_eq!(text.canonical.text, "office");
    let event = text
        .normalization_events
        .iter()
        .find(|event| event.kind == NormalizationKind::LigatureExpansion)
        .expect("ligature event should be retained");
    assert_eq!(event.raw_range, ScalarRange { start: 1, end: 2 });
    assert_eq!(event.canonical_range, ScalarRange { start: 1, end: 4 });
    assert_eq!(event.source.atoms, [TextSourceAtom::Glyph(GlyphId(2))]);
}

#[test]
fn keeps_compatibility_width_differences() {
    let full_width = normalize_mapped_lines(&["１０ files"]);
    let ascii = normalize_mapped_lines(&["10 files"]);

    assert_eq!(full_width.canonical.text, "１０ files");
    assert_ne!(full_width.canonical.text, ascii.canonical.text);
    assert_eq!(full_width.matching, ascii.matching);
}

#[test]
fn masks_sparse_numbers_for_matching_without_changing_canonical_text() {
    let old = normalize_mapped_lines(&["The archive contains 10 files in total."]);
    let new = normalize_mapped_lines(&["The archive contains 20 files in total."]);

    assert_ne!(old.canonical.text, new.canonical.text);
    assert_eq!(old.matching, new.matching);
    assert!(old.matching.contains("<NUM> files"));
    assert!(old.numeric_mask_applied);
    assert!(new.numeric_mask_applied);
}

#[test]
fn falls_back_to_unmasked_matching_for_number_dense_text() {
    let old = normalize_mapped_lines(&["10"]);
    let new = normalize_mapped_lines(&["20"]);

    assert_ne!(old.matching, new.matching);
    assert!(!old.numeric_mask_applied);
    assert!(!new.numeric_mask_applied);
}

#[test]
fn preserves_ambiguous_line_breaks_as_unresolved_evidence() {
    let text = normalize_mapped_lines(&["status:", "ready"]);

    assert_eq!(text.canonical.text, "status:\nready");
    assert_eq!(text.issues.len(), 1);
    assert_eq!(
        text.issues[0].kind,
        NormalizationIssueKind::AmbiguousLineBreak
    );
}

#[test]
fn retains_unmapped_tokens_in_comparison_order() {
    let hash = FontProgramHash(vec![1, 2, 3]);
    let document = Document::new(vec![
        glyph(1, mapped("A"), 0),
        glyph(
            2,
            DecodedText::Unmapped {
                font_hash: hash.clone(),
                glyph_id: 42,
            },
            0,
        ),
        glyph(3, mapped("B"), 0),
    ]);
    let lines = vec![line(1, 0, vec![GlyphId(1), GlyphId(2), GlyphId(3)], vec![])];
    let text = normalize(&document, &lines, &[body_block(1, vec![LineId(1)])]);

    assert_eq!(text.raw.text, "AB");
    assert_eq!(text.raw.unmapped[0].scalar_index, 1);
    assert!(matches!(
        &text.matching_tokens[1],
        ComparableToken::Unmapped {
            font_hash,
            glyph_id: 42
        } if font_hash == &hash
    ));
    assert_eq!(
        text.canonical
            .comparable_tokens()
            .expect("normalized token indices should be valid"),
        [
            ComparableToken::Scalar('A'),
            ComparableToken::Unmapped {
                font_hash: hash,
                glyph_id: 42,
            },
            ComparableToken::Scalar('B'),
        ]
    );
}

#[test]
fn canonical_text_is_idempotent() {
    let first = normalize_mapped_lines(&["oﬃce  report"]);
    let second = normalize_mapped_lines(&[&first.canonical.text]);

    assert_eq!(second.canonical.text, first.canonical.text);
    assert!(second.normalization_events.is_empty());
}

#[test]
fn rejects_unassigned_lines_and_glyphs() {
    let document = Document::new(vec![glyph(1, mapped("A"), 0), glyph(2, mapped("B"), 0)]);
    let lines = vec![
        line(1, 0, vec![GlyphId(1)], vec![]),
        line(2, 0, vec![GlyphId(2)], vec![]),
    ];

    let line_error = normalize_blocks(&document, &lines, &[body_block(1, vec![LineId(1)])])
        .expect_err("unassigned lines must not disappear");
    assert!(matches!(line_error, Error::Unresolved(message) if message.contains("1 of 2 lines")));

    let glyph_error = normalize_blocks(&document, &lines[..1], &[body_block(1, vec![LineId(1)])])
        .expect_err("unassigned glyphs must not disappear");
    assert!(matches!(glyph_error, Error::Unresolved(message) if message.contains("1 of 2 glyphs")));
}

#[test]
fn rejects_out_of_bounds_and_unordered_unmapped_indices() {
    let source = TextSource {
        atoms: vec![TextSourceAtom::Glyph(GlyphId(1))],
    };
    let out_of_bounds = MappedText {
        text: "A".to_owned(),
        source_map: vec![],
        unmapped: vec![UnmappedToken {
            scalar_index: 2,
            font_hash: FontProgramHash(vec![1]),
            glyph_id: 1,
            source: source.clone(),
        }],
    };
    assert!(matches!(
        out_of_bounds.comparable_tokens(),
        Err(Error::Unresolved(message)) if message.contains("ordered scalar offsets")
    ));

    let unordered = MappedText {
        text: "AB".to_owned(),
        source_map: vec![],
        unmapped: vec![
            UnmappedToken {
                scalar_index: 1,
                font_hash: FontProgramHash(vec![1]),
                glyph_id: 1,
                source: source.clone(),
            },
            UnmappedToken {
                scalar_index: 0,
                font_hash: FontProgramHash(vec![1]),
                glyph_id: 2,
                source,
            },
        ],
    };
    assert!(unordered.comparable_tokens().is_err());
}

#[test]
fn lexical_hyphen_with_capitalized_suffix_is_retained_in_canonical_text() {
    let text = normalize_mapped_lines(&["Franco-", "Prussian"]);

    assert_eq!(text.raw.text, "Franco-\nPrussian");
    assert_eq!(
        text.canonical.text, "Franco-Prussian",
        "Lexical hyphen before capitalized suffix must not be deleted"
    );
    assert!(
        text.normalization_events
            .iter()
            .all(|event| event.kind != NormalizationKind::HyphenationJoin),
        "Lexical hyphen must not be recorded as a HyphenationJoin event"
    );
}

#[test]
fn lexical_hyphen_with_numeric_suffix_is_retained_in_canonical_text() {
    let text = normalize_mapped_lines(&["pre-", "1990"]);

    assert_eq!(text.raw.text, "pre-\n1990");
    assert_eq!(
        text.canonical.text, "pre-1990",
        "Lexical hyphen before numeric suffix must not be deleted"
    );
}

#[test]
fn lexical_hyphen_with_single_letter_prefix_is_retained() {
    let text = normalize_mapped_lines(&["X-", "ray"]);

    assert_eq!(text.raw.text, "X-\nray");
    assert_eq!(
        text.canonical.text, "X-ray",
        "Single letter prefix hyphen must not be deleted"
    );
}

#[test]
fn bidirectional_source_mapping_projects_ranges_and_glyph_ids() {
    let text = normalize_mapped_lines(&["adminis-", "tration"]);

    // Raw: "adminis-\ntration" (16 chars)
    // Canonical: "administration" (14 chars)
    assert_eq!(text.canonical.text, "administration");

    // Canonical "adminis" (0..7) -> Raw "adminis" (0..7)
    let raw_prefix = text.canonical_to_raw_range(ScalarRange { start: 0, end: 7 });
    assert_eq!(raw_prefix, ScalarRange { start: 0, end: 7 });

    // Canonical "tration" (7..14) -> Raw "tration" (9..16)
    let raw_suffix = text.canonical_to_raw_range(ScalarRange { start: 7, end: 14 });
    assert_eq!(raw_suffix, ScalarRange { start: 9, end: 16 });

    // Boundary range across joined hyphen: Canonical "is-t" boundary "st" (6..8)
    let raw_boundary = text.canonical_to_raw_range(ScalarRange { start: 6, end: 8 });
    assert_eq!(raw_boundary, ScalarRange { start: 6, end: 10 });

    // Raw boundary "s-\nt" (6..10) -> Canonical "st" (6..8)
    let can_boundary = text.raw_to_canonical_range(ScalarRange { start: 6, end: 10 });
    assert_eq!(can_boundary, ScalarRange { start: 6, end: 8 });

    // Glyph ID projection
    let prefix_glyphs = text
        .canonical
        .project_glyph_ids(ScalarRange { start: 0, end: 7 });
    assert_eq!(prefix_glyphs.len(), 7);
    assert_eq!(prefix_glyphs[0], GlyphId(1));
    assert_eq!(prefix_glyphs[6], GlyphId(7));
}

#[test]
fn soft_line_break_inserted_space_source_mapping() {
    let text = normalize_mapped_lines(&["hello", "world"]);

    assert_eq!(text.raw.text, "hello\nworld");
    assert_eq!(text.canonical.text, "hello world");

    // The inserted canonical space is at index 5..6
    let space_source = text
        .canonical
        .project_source(ScalarRange { start: 5, end: 6 });
    assert_eq!(
        space_source.atoms,
        [TextSourceAtom::LineBreak {
            preceding: GlyphId(5),
            following: GlyphId(6),
        }]
    );

    let space_glyphs = text
        .canonical
        .project_glyph_ids(ScalarRange { start: 5, end: 6 });
    assert_eq!(space_glyphs, [GlyphId(5), GlyphId(6)]);

    // Bidirectional range mapping
    let raw_space = text.canonical_to_raw_range(ScalarRange { start: 5, end: 6 });
    assert_eq!(raw_space, ScalarRange { start: 5, end: 6 });
    assert_eq!(&text.raw.text[raw_space.start..raw_space.end], "\n");
}

#[test]
fn cjk_soft_line_break_source_mapping_and_event() {
    let text = normalize_mapped_lines(&["東京", "特許"]);

    assert_eq!(text.raw.text, "東京\n特許");
    assert_eq!(text.canonical.text, "東京特許");

    // Event records deleted soft line break
    let event = text
        .normalization_events
        .iter()
        .find(|e| e.kind == NormalizationKind::SoftLineBreak)
        .expect("SoftLineBreak event expected for CJK join");
    assert_eq!(event.raw_range, ScalarRange { start: 2, end: 3 });
    assert_eq!(event.canonical_range, ScalarRange { start: 2, end: 2 });

    // Canonical characters map to their exact CJK glyphs
    let tokyo_glyphs = text
        .canonical
        .project_glyph_ids(ScalarRange { start: 0, end: 2 });
    assert_eq!(tokyo_glyphs, [GlyphId(1), GlyphId(2)]);
    let tokkyo_glyphs = text
        .canonical
        .project_glyph_ids(ScalarRange { start: 2, end: 4 });
    assert_eq!(tokkyo_glyphs, [GlyphId(3), GlyphId(4)]);
}

#[test]
fn unmapped_glyph_evidence_is_not_empty_text_and_has_exact_source_mapping() {
    let hash = FontProgramHash(vec![10, 20, 30]);
    let document = Document::new(vec![
        glyph(1, mapped("hello"), 0),
        glyph(
            2,
            DecodedText::Unmapped {
                font_hash: hash.clone(),
                glyph_id: 99,
            },
            0,
        ),
        glyph(3, mapped("world"), 0),
    ]);
    let lines = vec![line(1, 0, vec![GlyphId(1), GlyphId(2), GlyphId(3)], vec![])];
    let text = normalize(&document, &lines, &[body_block(1, vec![LineId(1)])]);

    assert_eq!(text.raw.text, "helloworld");
    assert_eq!(text.canonical.text, "helloworld");
    assert_eq!(text.canonical.unmapped.len(), 1);
    assert_eq!(text.canonical.unmapped[0].glyph_id, 99);
    assert_eq!(text.canonical.unmapped[0].scalar_index, 5);
    assert_eq!(
        text.canonical.unmapped[0].source.atoms,
        [TextSourceAtom::Glyph(GlyphId(2))]
    );

    let tokens = text
        .canonical
        .comparable_tokens()
        .expect("comparable tokens");
    assert_eq!(tokens.len(), 11); // 5 ('hello') + 1 (unmapped) + 5 ('world')
    assert_eq!(
        tokens[5],
        ComparableToken::Unmapped {
            font_hash: hash,
            glyph_id: 99,
        }
    );
}

#[test]
fn lexical_hyphen_with_alphanumeric_compounds() {
    let covid = normalize_mapped_lines(&["COVID-", "19"]);
    let date_range = normalize_mapped_lines(&["1990-", "2000"]);

    assert_eq!(covid.canonical.text, "COVID-19");
    assert_eq!(date_range.canonical.text, "1990-2000");

    assert!(
        covid
            .normalization_events
            .iter()
            .all(|e| e.kind != NormalizationKind::HyphenationJoin)
    );
    assert!(
        date_range
            .normalization_events
            .iter()
            .all(|e| e.kind != NormalizationKind::HyphenationJoin)
    );
}

fn normalize_mapped_lines(line_texts: &[&str]) -> pdfdelta_core::normalize::BlockText {
    let mut glyphs = Vec::new();
    let mut lines = Vec::new();
    let mut next_glyph_id = 1_u64;

    for (line_index, text) in line_texts.iter().enumerate() {
        let mut line_glyphs = Vec::new();
        for scalar in text.chars() {
            let glyph_id = GlyphId(next_glyph_id);
            glyphs.push(glyph(
                next_glyph_id,
                DecodedText::Mapped(scalar.to_string()),
                line_index as u32,
            ));
            line_glyphs.push(glyph_id);
            next_glyph_id += 1;
        }
        lines.push(line(
            line_index as u64,
            line_index as u32,
            line_glyphs,
            vec![],
        ));
    }

    let block_lines = lines.iter().map(|line| line.id).collect();
    normalize(
        &Document::new(glyphs),
        &lines,
        &[body_block(1, block_lines)],
    )
}

fn normalize(
    document: &Document<Glyph>,
    lines: &[Line],
    blocks: &[Block],
) -> pdfdelta_core::normalize::BlockText {
    normalize_blocks(document, lines, blocks)
        .expect("normalization should succeed")
        .into_iter()
        .next()
        .expect("fixture should contain one block")
}

fn body_block(id: u64, lines: Vec<LineId>) -> Block {
    Block {
        id: BlockId(id),
        lines,
        role: BlockRole::Body,
    }
}

fn line(id: u64, page: u32, glyphs: Vec<GlyphId>, synthetic_spaces: Vec<SyntheticSpace>) -> Line {
    Line {
        id: LineId(id),
        page: PageId(page),
        glyphs,
        synthetic_spaces,
        bbox: Rect {
            min: Vec2 { x: 0.0, y: 0.0 },
            max: Vec2 { x: 100.0, y: 10.0 },
        },
        baseline: Vec2 { x: 0.0, y: 0.0 },
        direction: Vec2 { x: 1.0, y: 0.0 },
        text_direction: pdfdelta_core::layout::LineTextDirection::LeftToRight,
        render_order: id as u32..=id as u32,
    }
}

fn mapped(text: &str) -> DecodedText {
    DecodedText::Mapped(text.to_owned())
}

fn glyph(id: u64, text: DecodedText, page: u32) -> Glyph {
    Glyph {
        id: GlyphId(id),
        raw_code: match &text {
            DecodedText::Mapped(text) => text.as_bytes().to_vec(),
            DecodedText::Unmapped { glyph_id, .. } => glyph_id.to_be_bytes().to_vec(),
        },
        text,
        page: PageId(page),
        bbox: Rect {
            min: Vec2 {
                x: id as f64 * 10.0,
                y: 0.0,
            },
            max: Vec2 {
                x: id as f64 * 10.0 + 8.0,
                y: 10.0,
            },
        },
        baseline: Vec2 {
            x: id as f64 * 10.0,
            y: 0.0,
        },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size: 10.0,
        render_order: id as u32,
        render_mode: TextRenderMode::Fill,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: id as u32,
        },
    }
}
