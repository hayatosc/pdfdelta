use std::{
    fs, io,
    path::{Path, PathBuf},
};

use pdfdelta_core::{
    model::Document,
    pdf::ParseLimits,
    source::{ExternalFontIdentities, ExtractionIssue, ExtractionLimits, ExtractionOutcome},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fs::{lowercase_hex, read_limited_typed};

/// Bump when anything that changes extraction results is added to the cache
/// key or the cached payload shape.
const CACHE_FORMAT_VERSION: u32 = 5;

/// Cached glyph evidence has an explicit byte ceiling. Entries above this bound
/// are treated as corrupt rather than parsed, and the bound is enforced during
/// the read itself so a planted
/// oversized entry cannot exhaust memory.
const MAX_CACHE_PAYLOAD_BYTES: usize = 256 * 1024 * 1024;

/// Suffix of the marker recording that serializing an entry exceeded
/// [`MAX_CACHE_PAYLOAD_BYTES`]. Such an entry can never be read back, so the
/// marker lets later runs skip the doomed serialization entirely instead of
/// paying it on every run.
const OVERSIZED_MARKER_SUFFIX: &str = ".oversized";

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
        // Bounded read: the size ceiling is enforced while streaming via
        // `take`, so an oversized entry is rejected without ever being held
        // in memory in full.
        let payload = read_limited_typed(
            &self.dir.join(format!("{key}.json")),
            MAX_CACHE_PAYLOAD_BYTES,
        )
        .ok()?;
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
            .into_iter()
            .map(|issue| {
                let (kind, scope, description) = issue.into_parts();
                ExtractionIssue::new(kind, scope, description)
            })
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        // Reconstruction revalidates only what extraction enforces at the
        // boundaries: the resource ceilings (glyph and vector-line counts)
        // and the issue-scope invariants checked by `ExtractionIssue::new`
        // and `ExtractionOutcome::new`. Glyph content, geometry, and ids are
        // NOT revalidated, and cache entries are not authenticated, so a
        // well-formed but different glyph stream passes and silently changes
        // comparison results; the cache directory must not be writable by
        // untrusted writers.
        ExtractionOutcome::new(cached.document, issues).ok()
    }

    pub fn store(&self, key: &str, outcome: &ExtractionOutcome) {
        self.store_with_ceiling(key, outcome, MAX_CACHE_PAYLOAD_BYTES);
    }

    /// Serializes the outcome into the entry file directly, never buffering
    /// more than `ceiling` bytes: an entry that cannot be read back within
    /// [`MAX_CACHE_PAYLOAD_BYTES`] is never written, and a marker records it
    /// so later runs skip the doomed serialization entirely.
    fn store_with_ceiling(&self, key: &str, outcome: &ExtractionOutcome, ceiling: usize) {
        let marker = self.dir.join(format!("{key}{OVERSIZED_MARKER_SUFFIX}"));
        if marker.try_exists().unwrap_or(false) {
            return;
        }
        if self.dir.try_exists().is_ok_and(|exists| !exists)
            && fs::create_dir_all(&self.dir).is_err()
        {
            return;
        }
        // Serialize from borrowed evidence: cloning the document would double
        // the store cost for large documents.
        let cached = CachedRef {
            cache_version: CACHE_FORMAT_VERSION,
            cache_key: key,
            document: outcome.document(),
            issues: outcome.issues(),
        };
        let target = self.dir.join(format!("{key}.json"));
        let temp = self.dir.join(format!("{key}.{}.tmp", unique_temp_suffix()));
        match write_entry_exclusive(&temp, &cached, ceiling) {
            Ok(false) => {
                if fs::rename(&temp, &target).is_err() {
                    // A failed publication must not leave the temporary entry
                    // behind; the cache is rebuilt on a later run.
                    let _ = fs::remove_file(&temp);
                }
            }
            Ok(true) => {
                let _ = fs::remove_file(&temp);
                let _ = fs::write(&marker, b"");
            }
            Err(_) => {
                let _ = fs::remove_file(&temp);
            }
        }
    }
}

/// Serializes `cached` into a new exclusive file under the byte `ceiling`.
///
/// Returns `Ok(true)` when serialization was aborted because the payload
/// exceeded the ceiling (the caller should record the oversized marker);
/// `Ok(false)` after a successful durable write; `Err` for other I/O or
/// serialization failures.
fn write_entry_exclusive(
    path: &Path,
    cached: &CachedRef<'_>,
    ceiling: usize,
) -> std::io::Result<bool> {
    // create_new refuses symlinked pre-created names and 0o600 keeps the
    // entry private to the user, matching the report writers in fs.rs.
    let mut file = crate::fs::create_private_file(path)?;
    // serde_json emits many small writes; buffer them so the ceiling check
    // does not turn into one syscall per token.
    let mut writer = CeilingWriter {
        inner: io::BufWriter::new(&mut file),
        remaining: ceiling,
        oversized: false,
    };
    let serialize_result = serde_json::to_writer(&mut writer, cached);
    let oversized = writer.oversized;
    let CeilingWriter {
        inner: buffered, ..
    } = writer;
    match serialize_result {
        Ok(()) => {
            // `into_inner` flushes the buffer and returns the file reference,
            // ending the writer's borrow before the durability sync.
            let file = buffered.into_inner()?;
            file.sync_all()?;
            Ok(false)
        }
        Err(_) if oversized => Ok(true),
        Err(error) => {
            // serde_json wraps the underlying I/O error, preserving its kind.
            Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            ))
        }
    }
}

/// Write wrapper that fails the serialization once the payload would exceed
/// the ceiling, so oversized entries abort before consuming the full disk or
/// memory cost.
pub(crate) struct CeilingWriter<W> {
    inner: W,
    remaining: usize,
    oversized: bool,
}

impl<W> CeilingWriter<W> {
    pub(crate) fn new(inner: W, remaining: usize) -> Self {
        Self {
            inner,
            remaining,
            oversized: false,
        }
    }

    pub(crate) fn oversized(&self) -> bool {
        self.oversized
    }
}

impl<W: io::Write> io::Write for CeilingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.remaining {
            self.oversized = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized payload exceeds the size ceiling",
            ));
        }
        let written = self.inner.write(buf)?;
        self.remaining -= written;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Borrowing twin of the cached payload shape; serializes without cloning
/// the document.
#[derive(Serialize)]
struct CachedRef<'a> {
    cache_version: u32,
    cache_key: &'a str,
    document: &'a Document<pdfdelta_core::model::Glyph>,
    issues: &'a [ExtractionIssue],
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
    lowercase_hex(&digest)
}

/// Builds a name suffix unique across processes and within a process:
/// the high 32 bits carry the pid and the low 32 bits carry a per-process
/// counter, so `(pid, counter)` pairs never collide the way an XOR of the
/// two would.
fn unique_temp_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    (u64::from(std::process::id()) << 32) | (COUNTER.fetch_add(1, Ordering::SeqCst) & 0xffff_ffff)
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

    #[test]
    fn serialization_ceiling_charges_bytes_written_by_partial_writers() {
        struct Partial(Vec<u8>);
        impl io::Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = bytes.len().min(2);
                self.0.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let value = "a string exceeding one write";
        let expected = serde_json::to_vec(value).expect("fixture JSON");
        let mut inner = Partial(Vec::new());
        let mut writer = CeilingWriter::new(&mut inner, expected.len());
        serde_json::to_writer(&mut writer, value).expect("partial writes fit the ceiling");
        assert!(!writer.oversized());
        assert_eq!(inner.0, expected);
    }
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
        ExtractionOutcome::complete(
            Document::new(vec![glyph(1)])
                .with_last_non_text_paint(std::collections::BTreeMap::from([(PageId(0), 0)])),
        )
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
    fn cache_round_trips_float_geometry_exactly() {
        let dir = unique_temp_dir("float-roundtrip");
        let cache = ExtractionCache::new(&dir);
        let (parse_limits, extraction_limits) = fixture_limits();
        let key = cache_key(
            b"pdf",
            &parse_limits,
            &extraction_limits,
            None,
            &ExternalFontIdentities::default(),
        );
        // Values whose shortest decimal form is not reproduced by a
        // non-roundtrip float parser. The cache must preserve them exactly so
        // cached and fresh extraction produce identical comparison evidence.
        let mut sample = glyph(1);
        sample.bbox.min.x = 118.744_739_530_029_29;
        sample.bbox.max.x = 111.225_140_000_000_01;
        sample.baseline.x = 147.690_139_999_999_99;
        sample.baseline.y = 220.158_139_664_306_65;
        let outcome = ExtractionOutcome::complete(Document::new(vec![sample.clone()]));
        cache.store(&key, &outcome);

        let loaded = cache
            .load(&key, &extraction_limits)
            .expect("stored extraction should load from the cache");
        let loaded = loaded.document().items()[0].clone();
        assert_eq!(loaded.bbox.min.x.to_bits(), sample.bbox.min.x.to_bits());
        assert_eq!(loaded.bbox.max.x.to_bits(), sample.bbox.max.x.to_bits());
        assert_eq!(loaded.baseline.x.to_bits(), sample.baseline.x.to_bits());
        assert_eq!(loaded.baseline.y.to_bits(), sample.baseline.y.to_bits());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn oversized_entries_are_never_stored_and_marker_skips_later_stores() {
        // The ceiling is parameterized so the guard is testable without
        // materializing a 256 MiB payload: serialization aborts at the
        // ceiling, nothing is written, and a marker records the oversized
        // key so later stores skip the doomed serialization entirely.
        let dir = unique_temp_dir("oversized");
        let cache = ExtractionCache::new(&dir);
        let (parse_limits, extraction_limits) = fixture_limits();
        let key = cache_key(
            b"pdf",
            &parse_limits,
            &extraction_limits,
            None,
            &ExternalFontIdentities::default(),
        );
        let outcome = fixture_outcome();

        cache.store_with_ceiling(&key, &outcome, 32);

        let entries = fs::read_dir(&dir)
            .expect("cache directory should exist")
            .map(|entry| entry.expect("entry readable").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(
            entries[0].to_string_lossy().ends_with(".oversized"),
            "{entries:?}"
        );
        assert!(
            cache.load(&key, &extraction_limits).is_none(),
            "no readable entry may exist for an oversized payload"
        );

        // A later store sees the marker and degrades to a no-op.
        cache.store_with_ceiling(&key, &outcome, 32);
        let entries_after = fs::read_dir(&dir)
            .expect("cache directory should exist")
            .count();
        assert_eq!(entries_after, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_publication_removes_the_temporary_entry() {
        let dir = unique_temp_dir("failed-publish");
        let cache = ExtractionCache::new(&dir);
        let (parse_limits, extraction_limits) = fixture_limits();
        let key = cache_key(
            b"pdf",
            &parse_limits,
            &extraction_limits,
            None,
            &ExternalFontIdentities::default(),
        );
        // A directory at the entry path makes the atomic rename fail.
        let blocked = dir.join(format!("{key}.json"));
        fs::create_dir_all(&blocked).expect("blocking directory");
        fs::write(blocked.join("keep"), b"keep").expect("blocker content");

        cache.store(&key, &fixture_outcome());

        let entries = fs::read_dir(&dir)
            .expect("cache directory should exist")
            .map(|entry| {
                entry
                    .expect("entry readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![format!("{key}.json")], "{entries:?}");
        assert_eq!(
            fs::read(blocked.join("keep")).expect("blocker survives"),
            b"keep"
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
