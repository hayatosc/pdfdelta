use pdfdelta_core::{
    Error,
    layout::{LineOptions, SyntheticSpace, reconstruct_lines},
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::ObjectRef,
};

fn options() -> LineOptions {
    LineOptions {
        max_baseline_distance_ratio: 0.25,
        min_cross_axis_overlap_ratio: 0.25,
        min_direction_similarity: 0.98,
        max_inline_gap_font_size_ratio: 4.0,
        space_gap_font_size_ratio: 0.2,
        space_gap_advance_ratio: 0.5,
    }
}

#[test]
fn reconstructs_one_column_lines_in_reading_order() {
    let document = Document::new(vec![
        glyph(3, "C", 0, 0.0, 80.0, 10.0, 10.0, 10.0, 80.0),
        glyph(1, "A", 0, 0.0, 100.0, 10.0, 10.0, 10.0, 100.0),
        glyph(2, "B", 0, 11.0, 100.0, 10.0, 10.0, 10.0, 100.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("line reconstruction should work");

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert!(lines[0].synthetic_spaces.is_empty());
    assert_eq!(lines[1].glyphs, [GlyphId(3)]);
}

#[test]
fn keeps_mixed_size_superscript_on_the_same_line() {
    let document = Document::new(vec![
        glyph(1, "x", 0, 0.0, 0.0, 6.0, 10.0, 10.0, 0.0),
        glyph(2, "2", 0, 6.0, 5.0, 4.0, 6.0, 6.0, 5.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("superscript should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert!(lines[0].baseline.y.abs() < f64::EPSILON);
}

#[test]
fn records_a_missing_english_space_between_glyphs() {
    let document = Document::new(vec![
        glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
        glyph(2, "B", 0, 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("space reconstruction should work");

    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0].synthetic_spaces,
        [SyntheticSpace {
            preceding: GlyphId(1),
            following: GlyphId(2),
        }]
    );
}

#[test]
fn does_not_duplicate_an_explicit_space_glyph() {
    let document = Document::new(vec![
        glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0),
        glyph(2, " ", 0, 6.0, 0.0, 3.0, 10.0, 10.0, 0.0),
        glyph(3, "B", 0, 10.0, 0.0, 5.0, 10.0, 10.0, 0.0),
    ]);

    let lines = reconstruct_lines(&document, options()).expect("explicit space should be retained");

    assert_eq!(lines.len(), 1);
    assert!(lines[0].synthetic_spaces.is_empty());
}

#[test]
fn default_spacing_retains_sub_half_advance_word_gaps_at_multiple_scales() {
    for scale in [0.5, 1.0, 3.0] {
        for gap in [1.776, 2.688] {
            let document = Document::new(
                [
                    (1, "a", 0.0),
                    (2, "b", 6.3),
                    (3, "c", 12.3 + gap),
                    (4, "d", 18.6 + gap),
                ]
                .into_iter()
                .map(|(id, text, x)| {
                    glyph(
                        id,
                        text,
                        0,
                        x * scale,
                        0.0,
                        6.0 * scale,
                        12.0 * scale,
                        12.0 * scale,
                        0.0,
                    )
                })
                .collect(),
            );
            let lines = reconstruct_lines(&document, LineOptions::default())
                .expect("reconstruct scaled word gap without interpreting small kerning as spaces");
            assert_eq!(lines.len(), 1);
            assert_eq!(
                lines[0].synthetic_spaces,
                [SyntheticSpace {
                    preceding: GlyphId(2),
                    following: GlyphId(3)
                }]
            );
        }
    }
}

#[test]
fn default_line_gap_separates_narrow_columns_without_splitting_wide_word_spaces() {
    for scale in [0.5, 1.0, 3.0] {
        for (gap, expected_lines) in [(15.0, 1), (22.58, 2), (31.18, 2)] {
            let document = Document::new(
                [
                    (1, "A", 0.0, 0.0),
                    (2, "B", 5.5, 0.0),
                    (3, "C", 10.5 + gap, 2.0),
                    (4, "D", 16.0 + gap, 2.0),
                ]
                .into_iter()
                .map(|(id, text, x, y)| {
                    glyph(
                        id,
                        text,
                        0,
                        x * scale,
                        y * scale,
                        5.0 * scale,
                        10.0 * scale,
                        10.0 * scale,
                        y * scale,
                    )
                })
                .collect(),
            );
            let lines =
                reconstruct_lines(&document, LineOptions::default()).expect("source column gap");
            assert_eq!(lines.len(), expected_lines, "gap={gap}, scale={scale}");
            let mut groups = lines
                .iter()
                .map(|line| line.glyphs.clone())
                .collect::<Vec<_>>();
            groups.sort();
            if expected_lines == 2 {
                assert_eq!(
                    groups,
                    vec![vec![GlyphId(1), GlyphId(2)], vec![GlyphId(3), GlyphId(4)]]
                );
            } else {
                assert_eq!(
                    groups,
                    vec![vec![GlyphId(1), GlyphId(2), GlyphId(3), GlyphId(4)]]
                );
            }
        }
    }
}

#[test]
fn default_spacing_preserves_cjk_latin_typographic_gaps() {
    for scale in [0.5, 1.0, 3.0] {
        for (left, right, gap) in [
            ("は", "1", 2.688),
            ("0", "キ", 2.688),
            ("日", "本", 2.688),
            ("は", "1", 5.904),
            ("0", "キ", 5.904),
            ("日", "本", 5.904),
        ] {
            let document = Document::new(vec![
                glyph(
                    1,
                    left,
                    0,
                    0.0,
                    0.0,
                    12.0 * scale,
                    12.0 * scale,
                    12.0 * scale,
                    0.0,
                ),
                glyph(
                    2,
                    right,
                    0,
                    (12.0 + gap) * scale,
                    0.0,
                    6.0 * scale,
                    12.0 * scale,
                    12.0 * scale,
                    0.0,
                ),
            ]);
            let lines = reconstruct_lines(&document, LineOptions::default())
                .expect("reconstruct CJK typographic gap");
            assert_eq!(lines.len(), 1);
            assert!(
                lines[0].synthetic_spaces.is_empty(),
                "{left}{right}, scale {scale}"
            );
        }
    }
}

#[test]
fn rejects_non_finite_glyph_geometry() {
    let document = Document::new(vec![glyph(1, "A", 0, 0.0, f64::NAN, 5.0, 10.0, 10.0, 0.0)]);

    let error = reconstruct_lines(&document, options()).expect_err("NaN must be rejected");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("non-finite")));
}

#[test]
fn reconstructs_axis_aligned_vertical_glyphs_in_reading_order() {
    let mut first = glyph(1, "A", 0, 0.0, 0.0, 10.0, 5.0, 10.0, 0.0);
    first.direction = Vec2 { x: 0.0, y: 1.0 };
    let mut second = glyph(2, "B", 0, 0.0, 6.0, 10.0, 5.0, 10.0, 6.0);
    second.direction = Vec2 { x: 0.0, y: 1.0 };
    let document = Document::new(vec![second, first]);

    let lines = reconstruct_lines(&document, options()).expect("vertical label should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1), GlyphId(2)]);
    assert_eq!(lines[0].direction, Vec2 { x: 0.0, y: 1.0 });
}

#[test]
fn reconstructs_tilted_glyphs_in_projected_order() {
    let direction = Vec2 {
        x: 0.999_657_376_647_797_5,
        y: 0.026_174_974_950_197_414,
    };
    let mut later = glyph(
        1,
        "B",
        0,
        10.0 * direction.x,
        10.0 * direction.y,
        5.0,
        10.0,
        10.0,
        10.0 * direction.y,
    );
    later.direction = direction;
    let mut earlier = glyph(2, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    earlier.direction = direction;
    let document = Document::new(vec![later, earlier]);

    let lines = reconstruct_lines(&document, options()).expect("tilted text should be resolved");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(2), GlyphId(1)]);
    assert_eq!(lines[0].direction, direction);
}

#[test]
fn keeps_orthogonal_directions_out_of_the_same_line() {
    let horizontal = glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    let mut vertical = glyph(2, "B", 0, 0.0, 0.0, 10.0, 5.0, 10.0, 0.0);
    vertical.direction = Vec2 { x: 0.0, y: 1.0 };
    let document = Document::new(vec![horizontal, vertical]);

    let lines = reconstruct_lines(&document, options())
        .expect("orthogonal axis-aligned glyphs are independently supported");

    assert_eq!(lines.len(), 2);
}

#[test]
fn accepts_negative_horizontal_writing_direction() {
    let mut horizontal = glyph(1, "A", 0, 0.0, 0.0, 5.0, 10.0, 10.0, 0.0);
    horizontal.direction = Vec2 { x: -1.0, y: 0.0 };
    let document = Document::new(vec![horizontal]);

    let lines = reconstruct_lines(&document, options()).expect("horizontal direction is supported");

    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].glyphs, [GlyphId(1)]);
}

#[test]
fn rejects_zero_cross_axis_extent() {
    let document = Document::new(vec![glyph(1, "A", 0, 0.0, 0.0, 5.0, 0.0, 10.0, 0.0)]);

    let error = reconstruct_lines(&document, options()).expect_err("zero-height text is invalid");

    assert!(matches!(error, Error::Unresolved(message) if message.contains("cross-axis")));
}

#[allow(clippy::too_many_arguments)]
fn glyph(
    id: u64,
    text: &str,
    page: u32,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    font_size: f64,
    baseline_y: f64,
) -> Glyph {
    Glyph {
        id: GlyphId(id),
        text: DecodedText::Mapped(text.to_owned()),
        raw_code: text.as_bytes().to_vec(),
        page: PageId(page),
        bbox: Rect {
            min: Vec2 { x, y },
            max: Vec2 {
                x: x + width,
                y: y + height,
            },
        },
        baseline: Vec2 { x, y: baseline_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        font_id: FontId(1),
        font_size,
        render_order: id as u32,
        render_mode: TextRenderMode::Fill,
        crop_status: GlyphCropStatus::Inside,
        path_clip_status: GlyphPathClipStatus::Unclipped,
        provenance: GlyphProvenance {
            content_stream: ObjectRef {
                object_number: 1,
                generation: 0,
            },
            operator_index: id as u32,
        },
    }
}

#[test]
fn externally_rendered_japanese_case1_wrap_fixture_proves_different_line_boundaries()
-> pdfdelta_core::Result<()> {
    use pdfdelta_core::{
        pdf::{LopdfParser, ParseLimits, PdfParser},
        source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
    };
    use std::sync::Arc;

    let old_bytes = include_bytes!("../../../fixtures/external/case1-japanese-typst/old.pdf");
    let new_bytes = include_bytes!("../../../fixtures/external/case1-japanese-typst/new.pdf");

    let old_pdf = LopdfParser.parse(Arc::from(old_bytes.as_slice()), ParseLimits::default())?;
    let new_pdf = LopdfParser.parse(Arc::from(new_bytes.as_slice()), ParseLimits::default())?;

    let old_outcome = ContentStreamGlyphExtractor
        .extract_outcome(old_pdf.as_ref(), ExtractionLimits::default())?;
    let new_outcome = ContentStreamGlyphExtractor
        .extract_outcome(new_pdf.as_ref(), ExtractionLimits::default())?;

    assert!(old_outcome.is_complete());
    assert!(new_outcome.is_complete());
    assert!(old_outcome.issues().is_empty());
    assert!(new_outcome.issues().is_empty());

    let old_glyphs = old_outcome.document().items();
    let new_glyphs = new_outcome.document().items();

    assert_eq!(old_glyphs.len(), 158);
    assert_eq!(new_glyphs.len(), 158);

    for glyph in old_glyphs.iter().chain(new_glyphs.iter()) {
        assert_eq!(glyph.direction, Vec2 { x: 1.0, y: 0.0 });
        assert!(matches!(glyph.text, DecodedText::Mapped(_)));
    }

    let old_text = old_glyphs
        .iter()
        .map(|g| match &g.text {
            DecodedText::Mapped(s) => s.as_str(),
            DecodedText::Unmapped { .. } => panic!("all glyphs must be mapped"),
        })
        .collect::<String>();
    let new_text = new_glyphs
        .iter()
        .map(|g| match &g.text {
            DecodedText::Mapped(s) => s.as_str(),
            DecodedText::Unmapped { .. } => panic!("all glyphs must be mapped"),
        })
        .collect::<String>();

    assert_eq!(old_text, new_text);
    assert_eq!(
        old_text,
        "定期システム運用報告書今後の保守計画およびサービス稼働状況に関する概要です。クラウド基盤およびオンプレミス環境の定期点検を完了し、全システムの稼働率は計画値を上回る高い安定性を維持しています。運用手順書を順次適用し、監視体制の強化と障害検知の自動化を進めます。すべての基幹業務システムは各地域で正常に稼働しています。"
    );

    let old_lines = reconstruct_lines(old_outcome.document(), LineOptions::default())?;
    let new_lines = reconstruct_lines(new_outcome.document(), LineOptions::default())?;

    assert_eq!(old_lines.len(), 6);
    assert_eq!(new_lines.len(), 7);

    let old_line_glyph_counts: Vec<usize> = old_lines.iter().map(|l| l.glyphs.len()).collect();
    let new_line_glyph_counts: Vec<usize> = new_lines.iter().map(|l| l.glyphs.len()).collect();

    assert_eq!(old_line_glyph_counts, vec![11, 41, 41, 3, 34, 28]);
    assert_eq!(new_line_glyph_counts, vec![11, 31, 30, 24, 30, 4, 28]);

    Ok(())
}
