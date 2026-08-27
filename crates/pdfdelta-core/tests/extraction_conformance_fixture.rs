use pdfdelta_core::{
    extraction_conformance::{
        GeometryTolerance, PrimitiveExtractionSnapshot, SnapshotGlyph, compare_snapshots,
    },
    model::{PageId, Rect, Vec2},
    report::render_extraction_mismatch_svg,
};

#[test]
fn comparator_accepts_values_at_the_geometry_tolerance_boundary() {
    let expected = snapshot("A", 0.0, PageId(0), 0);
    let actual = snapshot("A", 0.25, PageId(0), 0);

    compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.25).expect("valid tolerance"),
    )
    .expect("boundary value is accepted");
}

#[test]
fn comparator_rejects_geometry_beyond_the_tolerance_with_field_context() {
    let expected = snapshot("A", 0.0, PageId(0), 0);
    let actual = snapshot("A", 0.251, PageId(0), 0);

    let error = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.25).expect("valid tolerance"),
    )
    .expect_err("out-of-tolerance geometry must fail");
    assert!(
        error.to_string().contains("glyph 0 bbox.min.x mismatch"),
        "{error}"
    );
}

#[test]
fn comparator_reports_count_text_page_and_order_mismatches() {
    let expected = snapshot("A", 0.0, PageId(0), 0);
    let tolerance = GeometryTolerance::new(0.0).expect("valid tolerance");

    let cases = [
        (
            "count",
            PrimitiveExtractionSnapshot::new(Vec::new()),
            "glyph count mismatch",
        ),
        (
            "text",
            snapshot("B", 0.0, PageId(0), 0),
            "glyph 0 text mismatch",
        ),
        (
            "page",
            snapshot("A", 0.0, PageId(1), 0),
            "glyph 0 page mismatch",
        ),
        (
            "order",
            snapshot("A", 0.0, PageId(0), 1),
            "glyph 0 render_order mismatch",
        ),
    ];
    for (name, actual, expected_message) in cases {
        let error = compare_snapshots(&expected, &actual, tolerance)
            .expect_err("mismatched snapshot must fail");
        assert!(
            error.to_string().contains(expected_message),
            "{name}: expected {expected_message:?} in {error:?}"
        );
    }
}

#[test]
fn comparator_rejects_invalid_tolerances() {
    for tolerance in [f64::NAN, f64::INFINITY, -0.1] {
        assert!(GeometryTolerance::new(tolerance).is_err());
    }
}

#[test]
fn comparator_rejects_non_finite_geometry() {
    let expected = snapshot("A", 0.0, PageId(0), 0);
    let mut actual = snapshot("A", 0.0, PageId(0), 0);
    actual.glyphs[0].bbox.min.x = f64::NAN;

    let error = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("non-finite geometry must fail");
    assert!(
        error.to_string().contains("glyph 0 bbox.min.x mismatch"),
        "{error}"
    );
}

#[test]
fn mismatch_svg_overlays_expected_and_actual_glyph_evidence() {
    let expected = snapshot("Expected", 0.0, PageId(0), 0);
    let actual = snapshot("Actual", 0.0, PageId(0), 0);
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different text must produce a mismatch");

    let svg = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect("valid snapshots render");

    assert!(svg.contains(r#"role="img""#));
    assert!(svg.contains("<title id=\"diagnostic-title\">Primitive extraction mismatch</title>"));
    assert!(svg.contains("Glyph 0 field mismatch: text"));
    assert!(svg.contains(r#"class="snapshot-glyph expected mismatch""#));
    assert!(svg.contains(r#"class="snapshot-glyph actual mismatch""#));
    assert!(svg.contains("Expected glyph 0"));
    assert!(svg.contains("Text: Expected"));
    assert!(svg.contains("Actual glyph 0"));
    assert!(svg.contains("Text: Actual"));
    assert_eq!(svg.matches(r#"<line class="snapshot-baseline""#).count(), 2);
}

#[test]
fn mismatch_svg_does_not_guess_the_missing_glyph_for_a_count_mismatch() {
    let mut expected = snapshot("A", 0.0, PageId(0), 0);
    expected.glyphs.push(SnapshotGlyph::mapped(
        "B",
        PageId(0),
        1,
        Rect {
            min: Vec2 { x: 4.0, y: 2.0 },
            max: Vec2 { x: 7.0, y: 4.0 },
        },
        Vec2 { x: 4.0, y: 2.0 },
        Vec2 { x: 1.0, y: 0.0 },
    ));
    let actual = snapshot("A", 0.0, PageId(0), 0);
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different counts must produce a mismatch");

    let svg = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect("valid snapshots render");

    assert!(svg.contains("Glyph count mismatch: expected 2, actual 1"));
    assert!(!svg.contains(r#"class="snapshot-glyph expected mismatch""#));
    assert!(!svg.contains(r#"class="snapshot-glyph actual mismatch""#));
}

#[test]
fn mismatch_svg_rejects_excessive_glyph_count() {
    // Quantitative upper bound without this gate: `ExtractionLimits::default`
    // allows 5M glyphs per snapshot. A count mismatch would retain all
    // 10M glyphs and attempt ~5–6 GiB of SVG text (500–600 B per glyph),
    // exhausting process memory.
    let expected = repeated_snapshot(25_001, "E");
    let actual = repeated_snapshot(25_001, "A");
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different glyph text must produce a mismatch");

    let error = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect_err("diagnostic must refuse excessive glyphs");
    assert!(
        error.to_string().contains("diagnostic glyph count") && error.to_string().contains("50000"),
        "{error}"
    );
    assert!(matches!(error, pdfdelta_core::Error::LimitExceeded { .. }));
}

#[test]
fn mismatch_svg_rejects_excessive_output_bytes() {
    // Even when the glyph count is within the 50 000 limit, worst-case SVG
    // serialization can exceed single-digit MiB (e.g. 20 000 glyphs × ~500 B
    // ≈ 10 MiB). The renderer enforces an 8 MiB output cap incrementally.
    let expected = repeated_snapshot_with_long_text(15_000, "E", 600);
    let actual = repeated_snapshot_with_long_text(15_000, "A", 600);
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different glyph text must produce a mismatch");

    let error = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect_err("diagnostic must refuse excessive output bytes");
    assert!(
        error.to_string().contains("diagnostic svg output")
            && error.to_string().contains("8388608"),
        "{error}"
    );
    assert!(matches!(error, pdfdelta_core::Error::LimitExceeded { .. }));
}

#[test]
fn mismatch_svg_rejects_excessive_glyph_text_before_allocation() {
    // Without a preflight cap, `glyph_title` and `xml_escape` would build
    // intermediate `String`s of up to 64 MiB (one valid oracle glyph) and
    // then escape them to hundreds of MiB before `BoundedWriter` can reject.
    // The 8 KiB per-glyph cap bounds that transient allocation.
    let long = "X".repeat(9 * 1024); // exceeds 8192
    let expected = snapshot(&long, 0.0, PageId(0), 0);
    let actual = snapshot("A", 0.0, PageId(0), 0);
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different glyph text must produce a mismatch");

    let error = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect_err("diagnostic must refuse excessive glyph text");
    assert!(
        error.to_string().contains("diagnostic glyph text") && error.to_string().contains("8192"),
        "{error}"
    );
    assert!(matches!(error, pdfdelta_core::Error::LimitExceeded { .. }));
}

#[test]
fn mismatch_svg_rejects_excessive_mismatch_field_before_allocation() {
    // `SnapshotMismatch::GlyphField` carries `expected`/`actual` debug strings.
    // A single mapped value at 64 MiB would otherwise be cloned into both the
    // mismatch and the SVG title/description before the byte cap. The
    // dedicated field cap bounds that intermediate. Even when glyph text is
    // just under the 8192 glyph-text cap, its debug representation
    // `Mapped("...")` exceeds 8192 and must be rejected via the mismatch-field
    // cap.
    let just_under = "Y".repeat(8 * 1024 - 5); // 8187, plus `Mapped("` overhead => >8192
    let expected = snapshot(&just_under, 0.0, PageId(0), 0);
    let actual = snapshot("A", 0.0, PageId(0), 0);
    let mismatch = compare_snapshots(
        &expected,
        &actual,
        GeometryTolerance::new(0.0).expect("valid tolerance"),
    )
    .expect_err("different glyph text must produce a mismatch");

    let error = render_extraction_mismatch_svg(&expected, &actual, &mismatch)
        .expect_err("diagnostic must refuse excessive mismatch field");
    assert!(
        error.to_string().contains("diagnostic mismatch field")
            && error.to_string().contains("8192"),
        "{error}"
    );
    assert!(matches!(error, pdfdelta_core::Error::LimitExceeded { .. }));
}

fn snapshot(
    text: &str,
    bbox_min_x: f64,
    page: PageId,
    render_order: u32,
) -> PrimitiveExtractionSnapshot {
    PrimitiveExtractionSnapshot::new(vec![SnapshotGlyph::mapped(
        text,
        page,
        render_order,
        Rect {
            min: Vec2 {
                x: bbox_min_x,
                y: 2.0,
            },
            max: Vec2 { x: 3.0, y: 4.0 },
        },
        Vec2 { x: 1.0, y: 2.0 },
        Vec2 { x: 1.0, y: 0.0 },
    )])
}

fn repeated_snapshot(count: usize, text: &str) -> PrimitiveExtractionSnapshot {
    let mut glyphs = Vec::with_capacity(count);
    for index in 0..count {
        glyphs.push(SnapshotGlyph::mapped(
            text,
            PageId(0),
            index as u32,
            Rect {
                min: Vec2 { x: 1.0, y: 2.0 },
                max: Vec2 { x: 3.0, y: 4.0 },
            },
            Vec2 { x: 1.0, y: 2.0 },
            Vec2 { x: 1.0, y: 0.0 },
        ));
    }
    PrimitiveExtractionSnapshot::new(glyphs)
}

fn repeated_snapshot_with_long_text(
    count: usize,
    text: &str,
    text_len: usize,
) -> PrimitiveExtractionSnapshot {
    let long_text = format!("{text}{}", "X".repeat(text_len));
    let mut glyphs = Vec::with_capacity(count);
    for index in 0..count {
        glyphs.push(SnapshotGlyph::mapped(
            long_text.clone(),
            PageId(0),
            index as u32,
            Rect {
                min: Vec2 { x: 1.0, y: 2.0 },
                max: Vec2 { x: 3.0, y: 4.0 },
            },
            Vec2 { x: 1.0, y: 2.0 },
            Vec2 { x: 1.0, y: 0.0 },
        ));
    }
    PrimitiveExtractionSnapshot::new(glyphs)
}
