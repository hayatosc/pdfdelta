use pdfdelta_core::{
    document::image_diff::{
        ImageChangeKind, ImageInventory, ImageOccurrence, compare_images, rgba_hash,
    },
    model::PageId,
};

fn image(color: u8, page: u32, x: f64, occurrence: u32) -> ImageOccurrence {
    ImageOccurrence {
        page: PageId(page),
        occurrence,
        object: None,
        transform: [72.0, 0.0, 0.0, 72.0, x, 0.0],
        width: 1,
        height: 1,
        sha256: Some(rgba_hash(1, 1, &[color, 0, 0, 255]).expect("hash")),
        unresolved: None,
    }
}
fn inventory(images: Vec<ImageOccurrence>) -> ImageInventory {
    ImageInventory {
        images,
        complete: true,
        issues: Vec::new(),
    }
}

#[test]
fn equal_hashes_ignore_page_moves_drawing_order_and_duplicate_locations() {
    let old = inventory(vec![
        image(1, 0, 0.0, 0),
        image(2, 0, 90.0, 1),
        image(1, 1, 0.0, 0),
    ]);
    let new = inventory(vec![
        image(1, 4, 32.0, 0),
        image(1, 2, 0.0, 0),
        image(2, 0, 0.0, 0),
    ]);
    let diff = compare_images(&old, &new).expect("diff");
    assert_eq!(diff.unchanged, 3);
    assert!(diff.changes.is_empty());
    assert!(diff.complete);
}

#[test]
fn unique_slot_replacement_and_one_sided_presence_are_coarse_changes() {
    let old = inventory(vec![image(1, 0, 0.0, 0)]);
    let new = inventory(vec![image(2, 0, 0.0, 0), image(3, 1, 0.0, 0)]);
    let diff = compare_images(&old, &new).expect("diff");
    assert_eq!(
        diff.changes.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ImageChangeKind::Changed, ImageChangeKind::Added]
    );
    let reverse = compare_images(&new, &old).expect("diff");
    assert_eq!(
        reverse.changes.iter().map(|c| c.kind).collect::<Vec<_>>(),
        [ImageChangeKind::Changed, ImageChangeKind::Removed]
    );
}

#[test]
fn different_hashes_without_unique_placement_remain_unresolved() {
    let old = inventory(vec![image(1, 0, 0.0, 0), image(2, 0, 0.0, 1)]);
    let new = inventory(vec![image(3, 0, 0.0, 0)]);
    let diff = compare_images(&old, &new).expect("diff");
    assert!(diff.changes.is_empty());
    assert_eq!(diff.unresolved_old, [0, 1]);
    assert_eq!(diff.unresolved_new, [0]);
    assert!(!diff.complete);
    let moved = inventory(vec![image(3, 1, 50.0, 0)]);
    assert!(
        compare_images(&old, &moved)
            .expect("diff")
            .changes
            .is_empty()
    );
}

#[test]
fn failed_decode_does_not_become_an_addition_or_removal() {
    let mut old = inventory(vec![image(1, 0, 0.0, 0)]);
    old.complete = false;
    old.images[0].sha256 = None;
    old.images[0].unresolved = Some("unsupported filter".into());
    let new = inventory(vec![image(3, 0, 0.0, 0)]);
    let diff = compare_images(&old, &new).expect("diff");
    assert!(diff.changes.is_empty());
    assert_eq!(diff.unresolved_old, [0]);
    assert_eq!(diff.unresolved_new, [0]);
    let failed = ImageInventory {
        issues: vec!["worker deadline".into()],
        ..Default::default()
    };
    assert!(
        compare_images(&failed, &new)
            .expect("diff")
            .changes
            .is_empty()
    );
}

#[test]
fn hash_includes_dimensions_and_alpha_and_rejects_bad_sizes() {
    let rgba = [0, 0, 0, 255, 0, 0, 0, 255];
    assert_ne!(
        rgba_hash(1, 2, &rgba).expect("hash"),
        rgba_hash(2, 1, &rgba).expect("hash")
    );
    assert_ne!(
        rgba_hash(1, 1, &[0, 0, 0, 255]).expect("hash"),
        rgba_hash(1, 1, &[0, 0, 0, 0]).expect("hash")
    );
    assert!(rgba_hash(u32::MAX, u32::MAX, &[]).is_err());
    assert!(rgba_hash(0, 0, &[]).is_err());
    assert!(rgba_hash(1, 1, &[1]).is_err());
}

#[test]
fn malformed_worker_evidence_is_rejected() {
    let mut input = inventory(vec![image(1, 0, 0.0, 0)]);
    input.images[0].sha256 = Some("not a hash".into());
    assert!(compare_images(&input, &inventory(Vec::new())).is_err());
    input.images[0] = image(1, 0, f64::NAN, 0);
    assert!(input.validate().is_err());
    input.images = vec![image(1, 0, 0.0, 0), image(1, 0, 0.0, 0)];
    assert!(input.validate().is_err());
}
