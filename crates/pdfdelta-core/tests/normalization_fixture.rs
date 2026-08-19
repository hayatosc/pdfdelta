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
    let cjk = normalize_mapped_lines(&["旧版で", "ある"]);
    let latin = normalize_mapped_lines(&["adult", "dose"]);
    let explicit_space = normalize_mapped_lines(&["adult ", "dose"]);

    assert_eq!(cjk.canonical.text, "旧版である");
    assert_eq!(latin.canonical.text, "adult dose");
    assert_eq!(explicit_space.canonical.text, "adult dose");
    assert!(cjk.normalization_events.iter().any(|event| {
        event.kind == NormalizationKind::SoftLineBreak
            && event.canonical_range.start == event.canonical_range.end
    }));
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
    let full_width = normalize_mapped_lines(&["１０ mg"]);
    let ascii = normalize_mapped_lines(&["10 mg"]);

    assert_eq!(full_width.canonical.text, "１０ mg");
    assert_ne!(full_width.canonical.text, ascii.canonical.text);
}

#[test]
fn preserves_ambiguous_line_breaks_as_unresolved_evidence() {
    let text = normalize_mapped_lines(&["dose:", "daily"]);

    assert_eq!(text.canonical.text, "dose:\ndaily");
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
    let first = normalize_mapped_lines(&["oﬃce  dose"]);
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
