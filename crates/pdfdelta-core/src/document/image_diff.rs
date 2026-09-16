//! Coarse comparison of decoded image occurrences, independent of text recognition.
//! Hash equality describes intrinsic pixels, not clipping, placement or page appearance.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Error, Result, model::PageId, pdf::ObjectRef};

pub const MAX_IMAGES: usize = 10_000;
pub const MAX_IMAGE_PIXELS: usize = 8_000_000;
pub const HASH_PROFILE: &str = "decoded-rgba8-sha256-v1";

/// One drawing invocation. Object numbers are side-local provenance, never match keys.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageOccurrence {
    pub page: PageId,
    pub occurrence: u32,
    pub object: Option<ObjectRef>,
    /// Unit-image to page transform in points, with a top-left page origin.
    pub transform: [f64; 6],
    pub width: u32,
    pub height: u32,
    pub sha256: Option<String>,
    pub unresolved: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ImageInventory {
    pub images: Vec<ImageOccurrence>,
    /// Whether the supported image drawing inventory was traversed without gaps.
    /// This does not certify all visible content or recognition of image text.
    pub complete: bool,
    pub issues: Vec<String>,
}

impl ImageInventory {
    /// Checks neutral evidence before matching or accepting a worker response.
    ///
    /// # Errors
    /// Rejects oversized inventories, duplicate locations and malformed hashes.
    pub fn validate(&self) -> Result<()> {
        if self.images.len() > MAX_IMAGES || self.issues.len() > MAX_IMAGES {
            return Err(Error::LimitExceeded {
                resource: "image inventory entries",
                limit: MAX_IMAGES,
            });
        }
        let mut ids = std::collections::BTreeSet::new();
        for image in &self.images {
            let valid_hash = image.sha256.as_ref().is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            });
            if !ids.insert((image.page, image.occurrence))
                || image.transform.iter().any(|n| !n.is_finite())
                || image.sha256.is_some() != valid_hash
                || image.sha256.is_some() == image.unresolved.is_some()
                || (valid_hash
                    && (image.width == 0
                        || image.height == 0
                        || u64::from(image.width) * u64::from(image.height)
                            > MAX_IMAGE_PIXELS as u64))
            {
                return Err(Error::Unresolved("invalid image hash evidence".into()));
            }
        }
        if self.complete
            && (!self.issues.is_empty()
                || self.images.iter().any(|image| image.unresolved.is_some()))
        {
            return Err(Error::Unresolved(
                "complete image inventory contains unresolved evidence".into(),
            ));
        }
        Ok(())
    }
}

/// Hashes full-resolution, row-major RGBA8 pixels together with their dimensions.
///
/// # Errors
/// Rejects empty, oversized or incorrectly sized raster data.
pub fn rgba_hash(width: u32, height: u32, rgba: &[u8]) -> Result<String> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_IMAGE_PIXELS as u64 || pixels * 4 != rgba.len() as u64 {
        return Err(Error::Unresolved(
            "invalid or oversized image raster".into(),
        ));
    }
    let mut hash = Sha256::new();
    hash.update(HASH_PROFILE.as_bytes());
    hash.update(width.to_be_bytes());
    hash.update(height.to_be_bytes());
    hash.update(rgba);
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageChangeKind {
    Changed,
    Added,
    Removed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageChange {
    pub kind: ImageChangeKind,
    /// Indices into the respective inventories; replacement correspondence is inferred.
    pub old: Option<usize>,
    pub new: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImageDiff {
    pub profile: &'static str,
    pub unchanged: usize,
    pub changes: Vec<ImageChange>,
    pub unresolved_old: Vec<usize>,
    pub unresolved_new: Vec<usize>,
    pub complete: bool,
}

/// Cancels equal pixel hashes as a multiset, independent of page or drawing order.
/// Remaining unique equal page/transform slots propose replacements. Unmatched
/// images are additions/removals only when no rival remains and both inventories
/// are complete. Ambiguous rivals and failed decodes are never empty images.
///
/// # Errors
/// Returns invalid or resource-limited inventory errors before comparing.
pub fn compare_images(old: &ImageInventory, new: &ImageInventory) -> Result<ImageDiff> {
    old.validate()?;
    new.validate()?;
    let mut result = ImageDiff {
        profile: HASH_PROFILE,
        unchanged: 0,
        changes: Vec::new(),
        unresolved_old: Vec::new(),
        unresolved_new: Vec::new(),
        complete: false,
    };
    let mut used_old = vec![false; old.images.len()];
    let mut used_new = vec![false; new.images.len()];
    let mut hashes: BTreeMap<_, Vec<usize>> = BTreeMap::new();
    for (i, image) in new.images.iter().enumerate() {
        if let Some(hash) = &image.sha256 {
            hashes
                .entry((image.width, image.height, hash))
                .or_default()
                .push(i);
        }
    }
    for (i, image) in old.images.iter().enumerate() {
        if let Some(hash) = &image.sha256
            && let Some(j) = hashes
                .get_mut(&(image.width, image.height, hash))
                .and_then(Vec::pop)
        {
            used_old[i] = true;
            used_new[j] = true;
            result.unchanged += 1;
        }
    }
    // Exact geometry avoids arbitrary pixel tolerances. A changed location does
    // not supply enough evidence to pair two different images.
    let slots = |inventory: &ImageInventory, used: &[bool]| {
        let mut slots: BTreeMap<_, Vec<usize>> = BTreeMap::new();
        for (i, image) in inventory
            .images
            .iter()
            .enumerate()
            .filter(|(i, _)| !used[*i])
        {
            let key = (
                image.page,
                image
                    .transform
                    .map(|n| if n == 0.0 { 0 } else { n.to_bits() }),
            );
            slots.entry(key).or_default().push(i);
        }
        slots
    };
    let old_slots = slots(old, &used_old);
    let new_slots = slots(new, &used_new);
    for (key, left) in old_slots {
        if let [i] = left.as_slice()
            && let Some(right) = new_slots.get(&key)
            && let [j] = right.as_slice()
            && old.images[*i].sha256.is_some()
            && new.images[*j].sha256.is_some()
        {
            used_old[*i] = true;
            used_new[*j] = true;
            result.changes.push(ImageChange {
                kind: ImageChangeKind::Changed,
                old: Some(*i),
                new: Some(*j),
            });
        }
    }
    let left: Vec<_> = (0..used_old.len()).filter(|i| !used_old[*i]).collect();
    let right: Vec<_> = (0..used_new.len()).filter(|i| !used_new[*i]).collect();
    if old.complete && new.complete && (left.is_empty() || right.is_empty()) {
        result.changes.extend(left.into_iter().map(|i| ImageChange {
            kind: ImageChangeKind::Removed,
            old: Some(i),
            new: None,
        }));
        result
            .changes
            .extend(right.into_iter().map(|i| ImageChange {
                kind: ImageChangeKind::Added,
                old: None,
                new: Some(i),
            }));
    } else {
        result.unresolved_old = left;
        result.unresolved_new = right;
    }
    result.complete = old.complete
        && new.complete
        && result.unresolved_old.is_empty()
        && result.unresolved_new.is_empty();
    Ok(result)
}
