mod support;

use pdfdelta_core::model::{PageId, Rect, Vec2};
use support::extraction_conformance::{
    GeometryTolerance, PrimitiveExtractionSnapshot, SnapshotGlyph, compare_snapshots,
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
    assert!(error.contains("glyph 0 bbox.min.x mismatch"), "{error}");
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
            error.contains(expected_message),
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
    assert!(error.contains("glyph 0 bbox.min.x mismatch"), "{error}");
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
