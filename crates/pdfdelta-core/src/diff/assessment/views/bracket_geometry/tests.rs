fn bounds_bits(value: Option<Option<BaselineBounds>>) -> Option<Option<[u64; 4]>> {
    value.map(|bounds| {
        bounds.map(|(a, b, c, d)| [a.to_bits(), b.to_bits(), c.to_bits(), d.to_bits()])
    })
}

#[test]
fn bracket_geometry_repeated_bounds_pay_for_setup_and_lookups() -> Result<()> {
    let blocks = [sourced_block(1, "abcdefghijklmnopqrst")];
    let source = side(&blocks);
    let views = build_views(&source, &[None], None, &[], &mut 100_000)?.expect("view");
    let ranges = [Some(0..20)];
    let mut geometry = SideGeometry::new(&source, &views, &[0], &ranges);
    let mut remaining = 1_000;
    for _ in 0..10 {
        assert_eq!(
            geometry.get(0, &mut remaining),
            Some(Some((0.0, 0.0, 0.0, 0.0)))
        );
    }
    // One slot, ten six-unit accesses, and the original two 21-unit scans.
    assert_eq!(1_000 - remaining, 103);
    let mut old_remaining = 1_000;
    for _ in 0..10 {
        assert!(reference_bounds(&source, &views[0], &(0..20), 0, &mut old_remaining).is_some());
    }
    assert_eq!(1_000 - old_remaining, 420);
    assert!(geometry.get(0, &mut 5).is_none());
    assert!(geometry.get(0, &mut 6).is_some());
    Ok(())
}

#[test]
fn bracket_geometry_unknown_is_retained_but_exhaustion_is_not() -> Result<()> {
    for unknown in [false, true] {
        let mut block = sourced_block(1, "abcdefghijklmnopqrst");
        if unknown {
            block.position_signatures = None;
        }
        let blocks = [block];
        let source = side(&blocks);
        let views = build_views(&source, &[None], None, &[], &mut 100_000)?.expect("view");
        let ranges = [Some(0..20)];
        let expected = reference_bounds(&source, &views[0], &(0..20), 0, &mut 100_000);
        for budget in 0..55 {
            let mut geometry = SideGeometry::new(&source, &views, &[0], &ranges);
            let actual = geometry.get(0, &mut { budget });
            if actual.is_some() {
                assert_eq!(actual, expected);
            }
            assert_eq!(geometry.get(0, &mut 100_000), expected);
            // A setup refused before any slot was funded stays on the scan path.
            let repeated = geometry.get(0, &mut 6);
            assert!(repeated.is_none() || repeated == expected);
        }
        let mut geometry = SideGeometry::new(&source, &views, &[0], &ranges);
        assert_eq!(geometry.get(0, &mut 100_000), expected);
        assert_eq!(geometry.get(0, &mut 6), expected);
    }
    Ok(())
}

#[test]
fn bracket_geometry_unfunded_slots_keep_the_original_scan() -> Result<()> {
    let blocks: Vec<_> = (1..=100)
        .map(|id| sourced_block(id, "abcdefghijklmnopqrst"))
        .collect();
    let source = side(&blocks);
    let views = build_views(&source, &[None; 100], None, &[], &mut 100_000)?.expect("views");
    let view_map: Vec<_> = (0..100).collect();
    let ranges = vec![Some(0..20); 100];
    let mut geometry = SideGeometry::new(&source, &views, &view_map, &ranges);
    let expected = reference_bounds(&source, &views[0], &(0..20), 0, &mut 100_000);
    let mut remaining = 48;
    assert_eq!(geometry.get(0, &mut remaining), expected);
    assert_eq!(remaining, 0);
    assert_eq!(geometry.get(0, &mut 6), None);
    assert_eq!(geometry.get(0, &mut 48), expected);
    Ok(())
}

#[test]
fn bracket_geometry_fixed_side_view_and_range_never_share_slots() -> Result<()> {
    let old_blocks = [spread_block(1, "ABCD", 1.0, 3.0, 0, 2.0)];
    let new_blocks = [spread_block(1, "ABCD", 20.0, 30.0, 0, 2.0)];
    let old = side(&old_blocks);
    let new = side(&new_blocks);
    let old_views = build_views(&old, &[None], None, &[], &mut 100_000)?.expect("old");
    let new_views = build_views(&new, &[None], None, &[], &mut 100_000)?.expect("new");
    let first = [Some(0..2)];
    let second = [Some(2..4)];
    let mut caches = [
        SideGeometry::new(&old, &old_views, &[0], &first),
        SideGeometry::new(&old, &old_views, &[0], &second),
        SideGeometry::new(&new, &new_views, &[0], &first),
    ];
    let expected = [
        Some(Some((1.0, 3.0, 3.0, 3.0))),
        Some(Some((5.0, 3.0, 7.0, 3.0))),
        Some(Some((20.0, 30.0, 22.0, 30.0))),
    ];
    for (cache, value) in caches.iter_mut().zip(expected) {
        assert_eq!(cache.get(0, &mut 100_000), value);
    }
    for (cache, value) in caches.iter_mut().zip(expected) {
        assert_eq!(cache.get(0, &mut 6), value);
    }
    Ok(())
}

#[test]
fn bracket_geometry_empty_and_missing_maps_hold_without_fabricated_bounds() -> Result<()> {
    let blocks = [sourced_block(1, "AB")];
    let source = side(&blocks);
    let views = build_views(&source, &[None], None, &[], &mut 100_000)?.expect("view");
    let ranges = [Some(0..0)];
    let mut empty = SideGeometry::new(&source, &views, &[0], &ranges);
    assert_eq!(empty.get(0, &mut 100), None);
    assert_eq!(empty.get(0, &mut 100), None);
    let missing = [None];
    let mut no_range = SideGeometry::new(&source, &views, &[0], &missing);
    assert_eq!(no_range.get(0, &mut 100), Some(None));
    let ranges = [Some(0..2)];
    let mut no_view = SideGeometry::new(&source, &views, &[usize::MAX], &ranges);
    assert_eq!(no_view.get(0, &mut 100), Some(None));
    Ok(())
}

#[test]
fn bracket_geometry_preserves_signed_zero_bits() -> Result<()> {
    let blocks = [sourced_block(1, "AB")];
    let source = side(&blocks);
    let mut views = build_views(&source, &[None], None, &[], &mut 100_000)?.expect("view");
    views[0].token_positions = [-0.0, 0.0]
        .into_iter()
        .map(|x| PositionSignature::new(Vec2 { x, y: -0.0 }, Vec2 { x: 1.0, y: 0.0 }))
        .collect();
    let ranges = [Some(0..2)];
    let expected = bounds_bits(reference_bounds(
        &source,
        &views[0],
        &(0..2),
        0,
        &mut 100_000,
    ));
    let mut geometry = SideGeometry::new(&source, &views, &[0], &ranges);
    assert_eq!(bounds_bits(geometry.get(0, &mut 100_000)), expected);
    assert_eq!(bounds_bits(geometry.get(0, &mut 6)), expected);
    Ok(())
}

#[derive(serde::Deserialize)]
struct GeometryOracle {
    positions: Vec<Option<[f64; 2]>>,
    source: Option<Vec<[f64; 2]>>,
    range: [usize; 2],
    expected_state: String,
    bounds: Option<[f64; 4]>,
}

#[test]
fn bracket_geometry_matches_independent_python_oracle() -> Result<()> {
    let cases: Vec<GeometryOracle> =
        serde_json::from_str(include_str!("oracle.json")).expect("oracle");
    assert_eq!(cases.len(), 640);
    let position = |[x, y]: [f64; 2]| {
        PositionSignature::new(Vec2 { x, y }, Vec2 { x: 1.0, y: 0.0 }).expect("finite")
    };
    for (index, case) in cases.into_iter().enumerate() {
        let len = case.positions.len();
        let mut blocks = [sourced_block(1, &"A".repeat(len))];
        let mut views = {
            let source = side(&blocks);
            build_views(&source, &[None], None, &[], &mut 100_000)?.expect("view")
        };
        blocks[0].position_signatures = case
            .source
            .map(|points| points.into_iter().map(position).collect());
        views[0].token_positions = case
            .positions
            .into_iter()
            .map(|point| point.map(position))
            .collect();
        let source = Side {
            blocks: &blocks,
            index: [(BlockId(1), 0)].into_iter().collect(),
            canonical: vec![views[0].group.tokens.clone()],
            total_tokens: len,
        };
        let ranges = [Some(case.range[0]..case.range[1])];
        let expected = match case.expected_state.as_str() {
            "known" => Some(case.bounds.map(|[a, b, c, d]| (a, b, c, d))),
            "unknown" => Some(None),
            "incomplete" => None,
            _ => panic!("invalid oracle state"),
        };
        let direct = reference_bounds(
            &source,
            &views[0],
            ranges[0].as_ref().expect("range"),
            0,
            &mut 100_000,
        );
        assert_eq!(direct, expected, "direct case {index}");
        let mut geometry = SideGeometry::new(&source, &views, &[0], &ranges);
        assert_eq!(
            geometry.get(0, &mut 100_000),
            expected,
            "first case {index}"
        );
        assert_eq!(geometry.get(0, &mut 6), expected, "repeated case {index}");
    }
    Ok(())
}
