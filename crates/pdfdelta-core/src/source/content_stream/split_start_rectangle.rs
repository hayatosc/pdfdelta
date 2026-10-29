//! Regression tests for exact implicit closure with a split starting edge.
use super::*;

fn path(points: &[Vec2]) -> CurrentPath {
    let mut path = CurrentPath::default();
    path.move_to(points[0]);
    for point in &points[1..] {
        path.line_to(*point).expect("connected line path");
    }
    path
}

fn fixture() -> [Vec2; 5] {
    [(0.0, 3.0), (0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)].map(|(x, y)| Vec2 { x, y })
}

#[test]
fn split_start_rectangle_matches_independent_cell_winding_oracle() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("split_start_rectangle/oracle.json")).expect("oracle");
    let truth = oracle["truth_bits"]
        .as_str()
        .expect("oracle bitset")
        .as_bytes();
    let points = (0..5)
        .flat_map(|x| {
            (0..5).map(move |y| Vec2 {
                x: f64::from(x),
                y: f64::from(y),
            })
        })
        .collect::<Vec<_>>();
    let neighbors = |p: Vec2| {
        (0..5)
            .filter(|&y| f64::from(y) != p.y)
            .map(|y| Vec2 {
                x: p.x,
                y: f64::from(y),
            })
            .chain((0..5).filter(|&x| f64::from(x) != p.x).map(|x| Vec2 {
                x: f64::from(x),
                y: p.y,
            }))
            .collect::<Vec<_>>()
    };
    let mut cases = 0;
    let mut accepted = 0;
    for a in points {
        for b in neighbors(a) {
            for c in neighbors(b) {
                for d in neighbors(c) {
                    for e in neighbors(d) {
                        let points = [a, b, c, d, e];
                        if let Some(rect) = path(&points).split_start_clipping_rectangle() {
                            assert_eq!(
                                truth[cases], b'1',
                                "false rectangle at case {cases}: {points:?}"
                            );
                            assert_eq!(
                                rect.min.x,
                                points.iter().map(|p| p.x).fold(f64::INFINITY, f64::min)
                            );
                            assert_eq!(
                                rect.min.y,
                                points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min)
                            );
                            assert_eq!(
                                rect.max.x,
                                points.iter().map(|p| p.x).fold(f64::NEG_INFINITY, f64::max)
                            );
                            assert_eq!(
                                rect.max.y,
                                points.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max)
                            );
                            accepted += 1;
                        }
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(cases, truth.len());
    assert_eq!(cases, 102_400);
    assert!(accepted > 0);
    println!(
        "split-start exact oracle: {cases} paths, {accepted} certificates, zero false rectangles"
    );
}

#[test]
fn split_start_rectangle_accepts_exact_physical_case_and_axis_transforms() -> Result<()> {
    for matrix in [
        Matrix::IDENTITY,
        Matrix::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0)?,
        Matrix::new(-1.0, 0.0, 0.0, 2.0, 10.0, -5.0)?,
    ] {
        let points = fixture().map(|p| {
            let (x, y) = matrix.transform_point(p.x, p.y).expect("finite transform");
            Vec2 { x, y }
        });
        assert!(path(&points).split_start_clipping_rectangle().is_some());
    }
    let matrix = Matrix::new(0.23071, 0.0, 0.0, 0.23071, 104.622, 218.611)?;
    let points = [
        (185.0, 2107.64),
        (185.0, 2103.21),
        (192.0, 2103.21),
        (192.0, 2107.641),
        (185.0, 2107.641),
    ]
    .map(|(x, y)| {
        let (x, y) = matrix.transform_point(x, y).expect("finite transform");
        Vec2 { x, y }
    });
    let path = path(&points);
    assert!(
        path.clipping_rectangle().is_none(),
        "ordinary rectangle keeps its refusal"
    );
    assert!(
        path.clipping_quad().is_none(),
        "ordinary quad keeps its refusal"
    );
    let rect = path
        .split_start_clipping_rectangle()
        .expect("exact split-edge rectangle");
    let glyph = Rect {
        min: Vec2 {
            x: 147.710_091_73,
            y: 702.765_463_323,
        },
        max: Vec2 {
            x: 148.893_634_03,
            y: 704.899_530_823,
        },
    };
    assert_eq!(
        ClipRegion::Rectangle(rect).glyph_status(glyph)?,
        GlyphPathClipStatus::PartiallyOutside
    );
    Ok(())
}

#[test]
fn split_start_rectangle_rejects_nonliteral_or_conflicting_evidence() {
    let base = path(&fixture());
    let mutations: &[fn(&mut CurrentPath)] = &[
        |p| p.drawn_subpaths = 2,
        |p| p.has_unsupported_segments = true,
        |p| p.has_unclosed_prior_subpath = true,
        |p| p.primitive_certainty = ClipCertainty::Unproven,
        |p| p.current_point = None,
        |p| p.subpath_start = None,
        |p| p.segments[1].from.x = p.segments[1].from.x.next_up(),
        |p| {
            p.segments[0].from.x = 0.0_f64.next_up();
            p.subpath_start = Some(p.segments[0].from);
        },
        |p| {
            p.segments[3].to.x = 0.0_f64.next_up();
            p.current_point = Some(p.segments[3].to);
        },
        |p| {
            p.segments[0].from.y = 5.0;
            p.subpath_start = Some(p.segments[0].from);
        },
        |p| {
            p.segments[0].from.y = f64::NAN;
            p.subpath_start = Some(p.segments[0].from);
        },
        |p| {
            p.segments[0].from.y = f64::INFINITY;
            p.subpath_start = Some(p.segments[0].from);
        },
        |p| p.segments.push(p.segments[0]),
        |p| {
            p.segments[1].to.x = 0.0;
            p.segments[2].from = p.segments[1].to;
        },
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut candidate = base.clone();
        mutate(&mut candidate);
        assert!(
            candidate.split_start_clipping_rectangle().is_none(),
            "mutation {index}"
        );
    }
}

#[test]
fn split_start_rectangle_prepaid_work_never_publishes_a_partial_certificate() {
    let path = path(&fixture());
    for remainder in 0..SPLIT_START_RECTANGLE_WORK {
        let mut used = 17;
        assert!(
            path.prepaid_split_start_rectangle(&mut used, 17 + remainder)
                .is_none()
        );
        assert_eq!(used, 17);
    }
    let mut used = 17;
    assert!(
        path.prepaid_split_start_rectangle(&mut used, 17 + SPLIT_START_RECTANGLE_WORK)
            .is_some()
    );
    assert_eq!(used, 17 + SPLIT_START_RECTANGLE_WORK);
    let mut invalid = path.clone();
    invalid.current_point = None;
    used = 17;
    assert!(
        invalid
            .prepaid_split_start_rectangle(&mut used, 17 + SPLIT_START_RECTANGLE_WORK)
            .is_none()
    );
    assert_eq!(
        used,
        17 + SPLIT_START_RECTANGLE_WORK,
        "failed attempted proof is charged"
    );
    used = usize::MAX;
    assert!(
        path.prepaid_split_start_rectangle(&mut used, usize::MAX)
            .is_none()
    );
    assert_eq!(used, usize::MAX);
}
