use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use pdfdelta_core::{
    model::Document,
    pdf::ParseLimits,
    source::{ExternalFontIdentities, ExtractionIssue, ExtractionLimits, ExtractionOutcome},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bump when anything that changes extraction results is added to the cache
/// key or the cached payload shape.
const CACHE_FORMAT_VERSION: u32 = 1;

/// Cached payloads are compact glyph evidence, never larger than the PDF they
/// came from; entries above this bound are treated as corrupt rather than
/// parsed, so a planted oversized entry cannot exhaust memory.
const MAX_CACHE_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct CachedExtraction {
    cache_version: u32,
    cache_key: String,
    document: Document<pdfdelta_core::model::Glyph>,
    issues: Vec<ExtractionIssue>,
}

/// Best-effort on-disk cache for extraction outcomes keyed by the complete set
/// of extraction-determining inputs. Every failure (missing entry, corrupt or
/// outdated payload, write error) degrades to a fresh extraction; the cache
/// never changes comparison results.
pub struct ExtractionCache {
    dir: PathBuf,
}

impl ExtractionCache {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
        }
    }

    pub fn load(&self, key: &str, limits: &ExtractionLimits) -> Option<ExtractionOutcome> {
        let payload = fs::read(self.dir.join(format!("{key}.json"))).ok()?;
        if payload.len() > MAX_CACHE_PAYLOAD_BYTES {
            return None;
        }
        let cached = serde_json::from_slice::<CachedExtraction>(&payload).ok()?;
        if cached.cache_version != CACHE_FORMAT_VERSION || cached.cache_key != key {
            return None;
        }
        // Enforce the same resource ceilings as a fresh extraction so a
        // planted entry cannot bypass extraction limits, and rebuild the
        // issues through their validating constructor because deserialization
        // bypasses `ExtractionIssue::new`.
        if cached.document.items().len() > limits.max_glyphs
            || cached.document.vector_lines().len() > limits.max_vector_lines
        {
            return None;
        }
        let issues = cached
            .issues
            .iter()
            .map(|issue| {
                let (kind, scope, description) = issue.clone().into_parts();
                ExtractionIssue::new(kind, scope, description)
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        // Reconstruction revalidates the outcome invariants that extraction
        // guarantees, so a stale or tampered cache cannot smuggle in invalid
        // evidence.
        ExtractionOutcome::new(cached.document, issues).ok()
    }

    pub fn store(&self, key: &str, outcome: &ExtractionOutcome) {
        if self
            .dir
            .try_exists()
            .map(|exists| !exists)
            .unwrap_or_default()
            && fs::create_dir_all(&self.dir).is_err()
        {
            return;
        }
        let cached = CachedExtraction {
            cache_version: CACHE_FORMAT_VERSION,
            cache_key: key.to_owned(),
            document: outcome.document().clone(),
            issues: outcome.issues().to_vec(),
        };
        let Ok(payload) = serde_json::to_vec(&cached) else {
            return;
        };
        let target = self.dir.join(format!("{key}.json"));
        let temp = self.dir.join(format!("{key}.{}.tmp", unique_temp_suffix()));
        if write_exclusive(&temp, &payload)
            .and_then(|()| fs::rename(&temp, &target))
            .is_err()
        {
            let _ = fs::remove_file(&temp);
        }
    }
}

fn write_exclusive(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    // create_new refuses symlinked pre-created names and 0o600 keeps the
    // entry private to the user, matching the report writers in fs.rs.
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(payload)?;
    file.sync_all()
}

/// Hashes every input that determines the extraction outcome: file bytes,
/// parser limits, extraction limits, password, and asserted font identities.
/// Passwords enter only through SHA-256, never in plaintext.
pub fn cache_key(
    bytes: &[u8],
    parse_limits: &ParseLimits,
    extraction_limits: &ExtractionLimits,
    password: Option<&str>,
    font_identities: &ExternalFontIdentities,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"pdfdelta-extraction-cache\0");
    hasher.update(CACHE_FORMAT_VERSION.to_be_bytes());
    hasher.update(bytes);

    fn hash_limits(hasher: &mut Sha256, values: &[usize]) {
        for value in values {
            hasher.update(b"L");
            hasher.update((*value as u64).to_be_bytes());
        }
    }
    let parse_values = [
        parse_limits.max_input_bytes,
        parse_limits.max_objects,
        parse_limits.max_recursion_depth,
        parse_limits.max_decoded_stream_bytes,
        parse_limits.max_total_object_stream_bytes,
        parse_limits.max_pages,
    ];
    hash_limits(&mut hasher, &parse_values);
    let extraction_values = [
        extraction_limits.max_glyphs,
        extraction_limits.max_form_depth,
        extraction_limits.max_nesting_depth,
        extraction_limits.max_operators,
        extraction_limits.max_stream_invocations,
        extraction_limits.max_total_decoded_bytes,
        extraction_limits.max_operand_stack,
        extraction_limits.max_array_elements,
        extraction_limits.max_operand_nodes,
        extraction_limits.max_fonts,
        extraction_limits.max_cmap_entries,
        extraction_limits.max_cid_width_entries,
        extraction_limits.max_string_bytes,
        extraction_limits.max_vector_lines,
    ];
    hash_limits(&mut hasher, &extraction_values);

    hasher.update(b"P");
    hasher.update(Sha256::digest(password.unwrap_or_default()));

    for (base_font, identity) in font_identities.iter() {
        hasher.update(b"F");
        hasher.update((base_font.len() as u64).to_be_bytes());
        hasher.update(base_font);
        hasher.update((identity.0.len() as u64).to_be_bytes());
        hasher.update(&identity.0);
    }

    let digest = hasher.finalize();
    let mut key = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn unique_temp_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    (std::process::id() as u64) ^ COUNTER.fetch_add(1, Ordering::SeqCst)
}

#[cfg(test)]
fn unique_temp_dir(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "pdfdelta-cache-test-{tag}-{}-{}",
        std::process::id(),
        unique_temp_suffix()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::parse_external_font_identities;
    use pdfdelta_core::model::{DecodedText, Glyph, GlyphId, PageId, Rect, TextRenderMode, Vec2};

    fn glyph(id: u64) -> Glyph {
        Glyph {
            id: GlyphId(id),
            text: DecodedText::Mapped("A".to_owned()),
            raw_code: vec![b'A'],
            page: PageId(0),
            bbox: Rect {
                min: Vec2 { x: 1.0, y: 2.0 },
                max: Vec2 { x: 3.0, y: 4.0 },
            },
            baseline: Vec2 { x: 1.0, y: 2.0 },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: pdfdelta_core::model::FontId(0),
            font_size: 10.0,
            render_order: 0,
            render_mode: TextRenderMode::Fill,
            crop_status: pdfdelta_core::model::GlyphCropStatus::Inside,
            path_clip_status: pdfdelta_core::model::GlyphPathClipStatus::Unclipped,
            provenance: pdfdelta_core::model::GlyphProvenance {
                content_stream: pdfdelta_core::pdf::ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: 0,
            },
        }
    }

    fn fixture_outcome() -> ExtractionOutcome {
        ExtractionOutcome::complete(Document::new(vec![glyph(1)]))
    }

    fn fixture_limits() -> (ParseLimits, ExtractionLimits) {
        (ParseLimits::default(), ExtractionLimits::default())
    }

    #[test]
    fn store_then_load_round_trips_the_outcome() {
        let dir = unique_temp_dir("roundtrip");
        let cache = ExtractionCache::new(&dir);
        let (parse_limits, extraction_limits) = fixture_limits();
        let key = cache_key(
            b"pdf",
            &parse_limits,
            &extraction_limits,
            None,
            &ExternalFontIdentities::default(),
        );
        cache.store(&key, &fixture_outcome());

        let loaded = cache
            .load(&key, &extraction_limits)
            .expect("stored extraction should load from the cache");
        assert_eq!(
            loaded.document().items(),
            fixture_outcome().document().items()
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_entries_and_corrupt_payloads_degrade_to_none() {
        let dir = unique_temp_dir("roundtrip");
        let cache = ExtractionCache::new(&dir);
        let (parse_limits, extraction_limits) = fixture_limits();
        let key = cache_key(
            b"pdf",
            &parse_limits,
            &extraction_limits,
            None,
            &ExternalFontIdentities::default(),
        );

        assert!(cache.load(&key, &extraction_limits).is_none());

        fs::create_dir_all(&dir).expect("cache directory should be creatable");
        fs::write(dir.join(format!("{key}.json")), b"not json")
            .expect("corrupt cache payload should be writable");

        assert!(cache.load(&key, &extraction_limits).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn key_covers_all_extraction_determining_inputs() {
        let (parse_limits, extraction_limits) = fixture_limits();
        let base = |password: Option<&str>, identities: &[String]| {
            let parsed = parse_external_font_identities(identities)
                .expect("fixture identities should parse");
            cache_key(b"pdf", &parse_limits, &extraction_limits, password, &parsed)
        };

        let no_password = base(None, &[]);
        assert_ne!(no_password, base(Some("secret"), &[]));
        assert_ne!(no_password, base(None, &["Base=abc".to_owned()]));
        assert_ne!(
            cache_key(
                b"pdf-a",
                &parse_limits,
                &extraction_limits,
                None,
                &ExternalFontIdentities::default()
            ),
            cache_key(
                b"pdf-b",
                &parse_limits,
                &extraction_limits,
                None,
                &ExternalFontIdentities::default()
            ),
        );
    }
}
