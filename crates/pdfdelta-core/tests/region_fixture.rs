use pdfdelta_core::{
    Result,
    layout::{
        Line, LineId, RegionOptions, RegionRelation, partition_regions, validate_region_options,
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

    // Check spatial relations
    assert!(graph.edges.iter().any(|&(a, b, rel)| a == left_region.id
        && b == right_region.id
        && rel == RegionRelation::LeftOf));
    assert!(graph.edges.iter().any(|&(a, b, rel)| a == right_region.id
        && b == left_region.id
        && rel == RegionRelation::RightOf));
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
}
