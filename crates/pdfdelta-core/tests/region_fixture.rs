use pdfdelta_core::{
    Error, Result,
    layout::{
        Line, LineId, ReadingOrder, RegionOptions, RegionRelation, partition_regions,
        validate_region_options,
    },
    model::{PageId, Rect, Vec2},
};

fn make_line(id: u64, min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Line {
    Line {
        id: LineId(id),
        page: PageId(0),
        glyphs: Vec::new(),
        synthetic_spaces: Vec::new(),
        bbox: Rect {
            min: Vec2 { x: min_x, y: min_y },
            max: Vec2 { x: max_x, y: max_y },
        },
        baseline: Vec2 { x: min_x, y: min_y },
        direction: Vec2 { x: 1.0, y: 0.0 },
        text_direction: pdfdelta_core::layout::LineTextDirection::LeftToRight,
        render_order: id as u32..=id as u32,
    }
}

#[test]
fn two_column_page_partitions_into_left_and_right_regions() -> Result<()> {
    // Left column lines (x from 50 to 250)
    let l1 = make_line(1, 50.0, 700.0, 250.0, 712.0);
    let l2 = make_line(2, 50.0, 680.0, 250.0, 692.0);
    let l3 = make_line(3, 50.0, 660.0, 250.0, 672.0);

    // Right column lines (x from 350 to 550) - vertical whitespace gap x=250..350 (gap=100)
    let r1 = make_line(4, 350.0, 700.0, 550.0, 712.0);
    let r2 = make_line(5, 350.0, 680.0, 550.0, 692.0);
    let r3 = make_line(6, 350.0, 660.0, 550.0, 672.0);

    let lines = vec![l1, l2, l3, r1, r2, r3];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 2);
    let left_region = &graph.regions[0];
    let right_region = &graph.regions[1];

    assert_eq!(left_region.line_ids, [LineId(1), LineId(2), LineId(3)]);
    assert_eq!(right_region.line_ids, [LineId(4), LineId(5), LineId(6)]);
    assert_eq!(
        graph.reading_order,
        ReadingOrder::Known(vec![left_region.id, right_region.id])
    );

    // Check exact spatial relations
    let mut expected_edges = vec![
        (left_region.id, right_region.id, RegionRelation::LeftOf),
        (right_region.id, left_region.id, RegionRelation::RightOf),
        (left_region.id, right_region.id, RegionRelation::Aligned),
        (right_region.id, left_region.id, RegionRelation::Aligned),
    ];
    expected_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    let mut actual_edges = graph.edges.clone();
    actual_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    assert_eq!(
        actual_edges, expected_edges,
        "Two-column graph must contain exactly LeftOf, RightOf, and Aligned edges without SameColumn"
    );
    Ok(())
}

#[test]
fn two_column_reading_order_is_independent_of_input_order() -> Result<()> {
    let lines = vec![
        make_line(3, 350.0, 700.0, 550.0, 712.0),
        make_line(2, 50.0, 680.0, 250.0, 692.0),
        make_line(4, 350.0, 680.0, 550.0, 692.0),
        make_line(1, 50.0, 700.0, 250.0, 712.0),
    ];

    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 2);
    assert_eq!(graph.regions[0].line_ids, [LineId(1), LineId(2)]);
    assert_eq!(graph.regions[1].line_ids, [LineId(3), LineId(4)]);
    assert_eq!(
        graph.reading_order,
        ReadingOrder::Known(graph.regions.iter().map(|region| region.id).collect())
    );
    Ok(())
}

#[test]
fn row_interleaved_two_column_render_order_is_unknown() -> Result<()> {
    let mut left_bottom = make_line(2, 50.0, 680.0, 250.0, 692.0);
    left_bottom.render_order = 3..=3;
    let mut right_top = make_line(3, 350.0, 700.0, 550.0, 712.0);
    right_top.render_order = 2..=2;
    let graph = partition_regions(
        PageId(0),
        &[
            make_line(1, 50.0, 700.0, 250.0, 712.0),
            left_bottom,
            right_top,
            make_line(4, 350.0, 680.0, 550.0, 692.0),
        ],
        RegionOptions::default(),
    )?;

    assert_eq!(graph.regions.len(), 2);
    assert_eq!(graph.reading_order, ReadingOrder::Unknown);
    Ok(())
}

#[test]
fn three_column_topology_is_unknown() -> Result<()> {
    let lines = vec![
        make_line(1, 50.0, 700.0, 200.0, 712.0),
        make_line(2, 50.0, 680.0, 200.0, 692.0),
        make_line(3, 300.0, 700.0, 450.0, 712.0),
        make_line(4, 300.0, 680.0, 450.0, 692.0),
        make_line(5, 550.0, 700.0, 700.0, 712.0),
        make_line(6, 550.0, 680.0, 700.0, 692.0),
    ];

    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 3);
    assert_eq!(graph.reading_order, ReadingOrder::Unknown);
    Ok(())
}

#[test]
fn non_horizontal_or_mixed_direction_has_unknown_reading_order() -> Result<()> {
    let mut vertical = make_line(2, 350.0, 680.0, 362.0, 780.0);
    vertical.direction = Vec2 { x: 0.0, y: 1.0 };
    let mixed = partition_regions(
        PageId(0),
        &[make_line(1, 50.0, 700.0, 250.0, 712.0), vertical],
        RegionOptions::default(),
    )?;
    let mut rtl = make_line(3, 50.0, 700.0, 250.0, 712.0);
    rtl.direction = Vec2 { x: -1.0, y: 0.0 };
    let rtl = partition_regions(PageId(0), &[rtl], RegionOptions::default())?;

    assert_eq!(mixed.reading_order, ReadingOrder::Unknown);
    assert_eq!(rtl.reading_order, ReadingOrder::Unknown);
    assert_eq!(
        partition_regions(PageId(0), &[], RegionOptions::default())?.reading_order,
        ReadingOrder::Known(Vec::new())
    );
    Ok(())
}

#[test]
fn columns_with_small_vertical_overlap_do_not_emit_aligned_relation() -> Result<()> {
    // Left column lines (y: 650..712, x: 50..250)
    let l1 = make_line(1, 50.0, 700.0, 250.0, 712.0);
    let l2 = make_line(2, 50.0, 680.0, 250.0, 692.0);
    let l3 = make_line(3, 50.0, 650.0, 250.0, 662.0);

    // Right column lines shifted down (y: 550..660, x: 350..550) - vertical overlap is only 650..660 (10pt out of 110pt = 9% <= 50%)
    let r1 = make_line(4, 350.0, 648.0, 550.0, 660.0);
    let r2 = make_line(5, 350.0, 600.0, 550.0, 612.0);
    let r3 = make_line(6, 350.0, 550.0, 550.0, 562.0);

    let lines = vec![l1, l2, l3, r1, r2, r3];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 2);
    let left_region = &graph.regions[0];
    let right_region = &graph.regions[1];

    let mut expected_edges = vec![
        (left_region.id, right_region.id, RegionRelation::LeftOf),
        (right_region.id, left_region.id, RegionRelation::RightOf),
    ];
    expected_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    let mut actual_edges = graph.edges.clone();
    actual_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    assert_eq!(
        actual_edges, expected_edges,
        "Columns with vertical overlap <= 50% must not emit Aligned or SameColumn edges"
    );
    Ok(())
}

#[test]
fn mixed_header_and_two_column_bands_partition_hierarchically() -> Result<()> {
    // Header line spanning full width (y=750..765, x=50..550)
    let h1 = make_line(100, 50.0, 750.0, 550.0, 765.0);
    let h2 = make_line(101, 50.0, 730.0, 550.0, 745.0);

    // Two columns below header (y=600..700)
    let l1 = make_line(1, 50.0, 680.0, 250.0, 692.0);
    let l2 = make_line(2, 50.0, 660.0, 250.0, 672.0);
    let r1 = make_line(3, 350.0, 680.0, 550.0, 692.0);
    let r2 = make_line(4, 350.0, 660.0, 550.0, 672.0);

    let lines = vec![h1, h2, l1, l2, r1, r2];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert!(
        graph.regions.len() >= 3,
        "Should partition into header and two columns"
    );
    let header_region = graph
        .regions
        .iter()
        .find(|r| r.line_ids.contains(&LineId(100)))
        .expect("header region must exist");
    assert!(header_region.line_ids.contains(&LineId(101)));

    // Header is Above the column regions
    let has_above_edge = graph
        .edges
        .iter()
        .any(|&(a, _, rel)| a == header_region.id && rel == RegionRelation::Above);
    assert!(has_above_edge, "Header must be Above lower regions");
    Ok(())
}

#[test]
fn vertically_stacked_bands_in_same_column_emit_same_column_relation() -> Result<()> {
    // Top band lines (y from 750 to 780, x from 50 to 450)
    let t1 = make_line(1, 50.0, 765.0, 450.0, 777.0);
    let t2 = make_line(2, 50.0, 750.0, 450.0, 762.0);

    // Bottom band lines (y from 600 to 630, x from 50 to 450) - vertical whitespace gap y=630..750
    let b1 = make_line(3, 50.0, 615.0, 450.0, 627.0);
    let b2 = make_line(4, 50.0, 600.0, 450.0, 612.0);

    let lines = vec![t1, t2, b1, b2];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 2);
    let top_region = &graph.regions[0];
    let bot_region = &graph.regions[1];

    let mut expected_edges = vec![
        (top_region.id, bot_region.id, RegionRelation::Above),
        (bot_region.id, top_region.id, RegionRelation::Below),
        (top_region.id, bot_region.id, RegionRelation::SameColumn),
        (bot_region.id, top_region.id, RegionRelation::SameColumn),
    ];
    expected_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    let mut actual_edges = graph.edges.clone();
    actual_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    assert_eq!(
        actual_edges, expected_edges,
        "Vertically stacked bands sharing horizontal extent must emit exactly Above, Below, and SameColumn"
    );
    Ok(())
}

#[test]
fn stacked_bands_with_small_horizontal_overlap_do_not_emit_same_column_relation() -> Result<()> {
    // Top band lines (x from 50 to 250, y from 750 to 780)
    let t1 = make_line(1, 50.0, 765.0, 250.0, 777.0);
    let t2 = make_line(2, 50.0, 750.0, 250.0, 762.0);

    // Bottom band lines shifted right (x from 230 to 550, y from 600 to 630) - horizontal overlap is 230..250 (20pt out of 200pt = 10% <= 50%)
    let b1 = make_line(3, 230.0, 615.0, 550.0, 627.0);
    let b2 = make_line(4, 230.0, 600.0, 550.0, 612.0);

    let lines = vec![t1, t2, b1, b2];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 2);
    let top_region = &graph.regions[0];
    let bot_region = &graph.regions[1];

    let mut expected_edges = vec![
        (top_region.id, bot_region.id, RegionRelation::Above),
        (bot_region.id, top_region.id, RegionRelation::Below),
    ];
    expected_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    let mut actual_edges = graph.edges.clone();
    actual_edges.sort_by_key(|e| (e.0.0, e.1.0, e.2 as u8));
    assert_eq!(
        actual_edges, expected_edges,
        "Stacked bands with horizontal overlap <= 50% must emit Above and Below without SameColumn"
    );
    Ok(())
}

#[test]
fn degenerate_zero_height_lines_decline_cuts_without_fixed_fallback() -> Result<()> {
    // 4 lines with zero height (max_y == min_y) separated horizontally
    let l1 = make_line(1, 50.0, 700.0, 200.0, 700.0);
    let l2 = make_line(2, 50.0, 680.0, 200.0, 680.0);
    let r1 = make_line(3, 350.0, 700.0, 500.0, 700.0);
    let r2 = make_line(4, 350.0, 680.0, 500.0, 680.0);

    let lines = vec![l1, l2, r1, r2];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    // Since all line heights are degenerate (0.0), geometry cut must decline rather than using arbitrary 12.0 fallback
    assert_eq!(
        graph.regions.len(),
        1,
        "Degenerate zero-height lines must decline cut and remain in single region"
    );
    assert_eq!(
        graph.regions[0].line_ids.len(),
        4,
        "All lines must be preserved in the single region"
    );
    assert!(graph.edges.is_empty());
    Ok(())
}

#[test]
fn single_column_page_remains_single_region() -> Result<()> {
    let l1 = make_line(1, 50.0, 700.0, 500.0, 712.0);
    let l2 = make_line(2, 50.0, 680.0, 500.0, 692.0);
    let l3 = make_line(3, 50.0, 660.0, 500.0, 672.0);

    let lines = vec![l1, l2, l3];
    let graph = partition_regions(PageId(0), &lines, RegionOptions::default())?;

    assert_eq!(graph.regions.len(), 1);
    assert_eq!(graph.regions[0].line_ids, [LineId(1), LineId(2), LineId(3)]);
    assert!(graph.edges.is_empty());
    Ok(())
}

#[test]
fn region_partition_reports_recursion_depth_limit() {
    let lines = vec![
        make_line(1, 50.0, 700.0, 500.0, 712.0),
        make_line(2, 50.0, 100.0, 500.0, 112.0),
    ];
    let options = RegionOptions {
        min_partition_lines: 1,
        max_recursion_depth: 1,
        ..RegionOptions::default()
    };

    let error = partition_regions(PageId(0), &lines, options)
        .expect_err("a recursive cut beyond the configured depth must fail");

    assert!(matches!(
        error,
        Error::LimitExceeded {
            resource: "region XY-Cut recursion depth",
            limit: 1
        }
    ));
}

#[test]
fn region_options_validation_rejects_invalid_values() {
    assert!(
        validate_region_options(RegionOptions {
            min_vertical_gap_ratio: -0.1,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        validate_region_options(RegionOptions {
            min_partition_lines: 0,
            ..Default::default()
        })
        .is_err()
    );
    assert!(
        validate_region_options(RegionOptions {
            max_recursion_depth: 0,
            ..Default::default()
        })
        .is_err()
    );
}
