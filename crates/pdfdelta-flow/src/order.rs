//! Reading-order hypotheses for one document side.
//!
//! Layout reconstruction can merge several text rows into one line on dense
//! forms. The resulting token order verifies but no longer reads like prose.
//! This module builds three hypotheses from the same visible glyphs: layout
//! order, content-stream paint order, and baseline-row order. It chooses one
//! per page from counterpart evidence in the other document only. The caller
//! must supply both documents so the decision is symmetric and never consults
//! an external reader.
//!
//! Each page is judged only against the pages of the other document that it
//! actually corresponds to. Counterparts are found by an order-free weighted
//! bag overlap, so editing an unrelated page cannot change a page's evidence,
//! and a duplicated page is judged against the same counterpart as the
//! original. A page without a similar counterpart (inserted or deleted) is
//! ordered from spatial coherence alone, never from unrelated pages.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::{Read, Write},
    ops::{Range, RangeInclusive},
};

use pdfdelta_core::{
    Result,
    layout::{
        Block, BlockId, BlockOptions, BlockRole, Line, LineId, LineOptions, LineTextDirection,
        SyntheticSpace, reconstruct_blocks, reconstruct_lines,
    },
    model::{DecodedText, Document, Glyph, GlyphId, NonTextPaintKind, PageId, Rect, Vec2},
    normalize::PositionSignature,
};

use crate::{
    Conservation, FlowStream, FlowToken, PageConservation, align, furniture, intern_tokens,
    stream_from_layout, visible_document, visible_document_owned,
};

/// Minimum order-preserving overlap for the layout and paint-order candidates
/// to count as the same reading order.
pub const ORDER_AGREEMENT_MIN: f64 = 0.9;

/// Minimum fraction of spatially adjacent consecutive tokens for an order to
/// be considered coherent.
///
/// Generated fixtures keep every page at or above 0.83, vendored external
/// pairs at or above 0.90, and the scrambled IRS layout below 0.4. The
/// threshold sits below the known-good observations and above the scrambled
/// page so it reports genuine discontinuities rather than ordinary paragraph
/// and table transitions.
pub const ORDER_COHERENCE_MIN: f64 = 0.5;

/// A counterpart must retain this fraction of the page's best match overlap.
const COUNTERPART_RETAIN: f64 = 0.5;

/// Minimum share of the document pair's typical page correspondence that the
/// best match must explain for a page to have a counterpart.
///
/// Two documents that differ by one page have a median best overlap near 1.0,
/// so a page deleted from the other side (which can still share boilerplate
/// with a neighbouring page) falls back to the intrinsic rule. Revisions whose
/// pages genuinely share little text keep a low floor, because the median shows
/// that weak overlap is normal for this pair. A page that shares no
/// information-weighted token with any page has no counterpart regardless.
const COUNTERPART_DOCUMENT_RETAIN: f64 = 0.3;

/// Maximum number of counterpart pages whose order evidence is combined.
/// The two pages adjacent to the best match are added on top of this bound.
const COUNTERPART_TOP_K: usize = 8;

/// A page is split into stacked bands only when a vertical gap between text
/// rows exceeds this multiple of the page's median row pitch.
///
/// Ordinary paragraph spacing, headings, and figure captions stay far below
/// this ratio. Two pages stacked by a merge leave their combined top and
/// bottom margins between the rows, which is many line pitches.
const STACKED_GAP_PITCH_RATIO: f64 = 4.0;

/// Minimum text rows in each stacked band. A single stray row does not define
/// an independent reading region.
const STACKED_MIN_BAND_ROWS: usize = 2;

/// Minimum text rows on a page before stacked bands are considered.
const STACKED_MIN_PAGE_ROWS: usize = 4;

/// Each stacked band must carry at least this fraction of the page's tokens.
///
/// A running header a few lines above the body leaves a large gap, but it is
/// not an independent region: the body is almost the whole page. Two pages
/// stacked by a merge contribute about half the tokens each.
const STACKED_MIN_BAND_TOKEN_FRACTION: f64 = 0.2;

/// Minimum information-weighted bag overlap between a stacked band and a whole
/// page for the band to adopt that page's chosen reading order.
///
/// A band that is exactly one page of a merged pair has overlap 1.0. A band
/// that is a part of a normal page shares only a fraction of the page's
/// weighted tokens, well below this threshold.
const REGION_ADOPT_MIN: f64 = 0.9;

/// Which reading-order hypothesis produced a page's tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    /// Reconstructed layout order.
    Layout,
    /// Content-stream paint order.
    Stream,
    /// Baseline-row order.
    Baseline,
}

impl OrderKind {
    /// Stable lowercase name for reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Stream => "stream",
            Self::Baseline => "baseline",
        }
    }
}

/// Chosen order for one stacked band of a page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandOrder {
    /// Lower edge of the band in page coordinates (inclusive).
    pub bottom: f64,
    /// Upper edge of the band in page coordinates (exclusive).
    pub top: f64,
    /// Chosen hypothesis for the band.
    pub order: OrderKind,
}

/// Evidence for one page's chosen reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct PageOrderEvidence {
    /// Page the evidence applies to.
    pub page: PageId,
    /// Chosen hypothesis.
    pub order: OrderKind,
    /// The layout and paint-order hypotheses disagreed on order-preserving overlap.
    pub uncertain: bool,
    /// Order-preserving overlap between layout and paint order, in `[0, 1]`.
    pub agreement: f64,
    /// Information-weighted n-grams shared with the other document's three
    /// order candidates.
    pub layout_score: u64,
    /// Same score for the paint-order candidate.
    pub stream_score: u64,
    /// Same score for the baseline-row candidate.
    pub baseline_score: u64,
    /// Fraction of consecutive tokens that are spatially adjacent in the
    /// chosen order.
    pub coherence: f64,
    /// Per-band orders for a page split into stacked regions whose bands match
    /// separate whole pages. Empty when one order applies to the whole page.
    pub bands: Vec<BandOrder>,
}

/// Old and new flows after per-page order selection.
#[derive(Clone, Debug)]
pub struct FlowOrderPair {
    /// Old-side stream with the chosen page orders and furniture removed.
    pub old: FlowStream,
    /// New-side stream with the chosen page orders and furniture removed.
    pub new: FlowStream,
    /// Old-side page evidence, ascending by page.
    pub old_pages: Vec<PageOrderEvidence>,
    /// New-side page evidence, ascending by page.
    pub new_pages: Vec<PageOrderEvidence>,
}

struct Candidates {
    layout: FlowStream,
    stream: FlowStream,
    baseline: FlowStream,
}

/// Builds all order hypotheses for each document, chooses one per page from
/// cross-document n-gram evidence, and returns the flows with furniture
/// removed.
///
/// # Errors
///
/// Returns an error when line or block reconstruction fails, when the
/// paint-order hypothesis cannot be normalized, or when resource limits are
/// exceeded.
pub fn flow_tokens_pair(
    old_document: &Document<Glyph>,
    new_document: &Document<Glyph>,
) -> Result<FlowOrderPair> {
    let old = flow_candidates(old_document)?;
    let new = flow_candidates(new_document)?;
    flow_order_from_candidates(old, new)
}

/// One non-text paint item retained for the flow text-completeness check.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintIdentity {
    /// Page the paint appears on. It is excluded from `identity` equality.
    pub page: PageId,
    /// Paint kind, retained for diagnostics.
    pub kind: NonTextPaintKind,
    /// Page-relative geometry or image-data identity. `None` means exact
    /// identity was unavailable and paint equality cannot be claimed.
    pub identity: Option<String>,
    /// Translation-invariant shape or image-data identity.
    pub shape: Option<String>,
    /// Page-space bounds, when the extraction established them.
    pub bounds: Option<Rect>,
}

/// One side's three order hypotheses, with its extracted document no longer
/// needed.
pub struct FlowCandidates {
    candidates: Candidates,
    paint_inventory: Option<Vec<PaintIdentity>>,
}

impl FlowCandidates {
    /// Returns the extracted non-text paint inventory.
    ///
    /// `None` means the side was built without a paint census. The text engine
    /// must not claim completeness in that case.
    #[must_use]
    pub fn paint_inventory(&self) -> Option<&[PaintIdentity]> {
        self.paint_inventory.as_deref()
    }

    /// Number of tokens in the layout-order hypothesis.
    ///
    /// Trace metrics use this bounded count; the other hypotheses have the
    /// same token multiset in a different order.
    #[must_use]
    pub fn token_count(&self) -> usize {
        self.candidates.layout.tokens.len()
    }

    /// Borrows the three order hypotheses in layout, stream, and baseline
    /// order. Their tokens may differ in order but not in multiset.
    #[must_use]
    pub fn hypotheses(&self) -> [&FlowStream; 3] {
        [
            &self.candidates.layout,
            &self.candidates.stream,
            &self.candidates.baseline,
        ]
    }

    /// Attaches retained per-token source glyph ids to the three order
    /// hypotheses.
    ///
    /// This is used when the hypotheses were built in a child process and the
    /// compact cache carried only tokens. Each list must be aligned with its
    /// hypothesis' token count, and sources must not already be attached.
    ///
    /// # Errors
    ///
    /// Returns an error when a list is misaligned or sources were already
    /// attached.
    pub fn attach_sources(
        &mut self,
        layout: Vec<Vec<GlyphId>>,
        stream: Vec<Vec<GlyphId>>,
        baseline: Vec<Vec<GlyphId>>,
    ) -> Result<()> {
        for (name, hypothesis, sources) in [
            ("layout", &mut self.candidates.layout, layout),
            ("stream", &mut self.candidates.stream, stream),
            ("baseline", &mut self.candidates.baseline, baseline),
        ] {
            if !hypothesis.source_glyphs.is_empty() {
                return Err(pdfdelta_core::Error::Unresolved(format!(
                    "{name} order hypothesis already carries source glyph ids"
                )));
            }
            if sources.len() != hypothesis.tokens.len() {
                return Err(pdfdelta_core::Error::Unresolved(format!(
                    "{name} order hypothesis has {} tokens but {} source lists",
                    hypothesis.tokens.len(),
                    sources.len()
                )));
            }
            hypothesis.source_glyphs = sources;
        }
        Ok(())
    }

    /// Whether every visible glyph is accounted for on this side.
    #[must_use]
    pub fn conservation_complete(&self) -> bool {
        self.candidates.layout.conservation.is_complete()
    }

    /// Writes the three reading-order hypotheses for another process.
    ///
    /// A flow comparison of a multi-million-glyph document keeps both sides'
    /// hypotheses alive while the second side is extracted. Serializing one
    /// side lets the caller build the other side in a process whose
    /// normalization buffers are not shared, then reload both compact token
    /// streams for order selection. The format is a private, versioned binary
    /// cache: token texts, page ids, bounding boxes, and glyph conservation are
    /// written exactly, so a reloaded comparison produces the same script.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the writer fails.
    pub fn write_cache(&self, writer: impl std::io::Write) -> std::io::Result<()> {
        let mut writer = CachedWriter {
            inner: writer,
            checksum: CHECKSUM_OFFSET,
        };
        writer.write_all(CACHE_MAGIC)?;
        writer.write_all(&CACHE_VERSION.to_le_bytes())?;
        for stream in [
            &self.candidates.layout,
            &self.candidates.stream,
            &self.candidates.baseline,
        ] {
            write_cached_stream(&mut writer, stream)?;
        }
        write_cached_paint(&mut writer, self.paint_inventory.as_deref())?;
        let checksum = writer.checksum;
        writer.inner.write_all(&checksum.to_le_bytes())?;
        writer.inner.flush()
    }

    /// Reads hypotheses written by [`Self::write_cache`].
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the reader fails or the data is not a cache
    /// written by this version.
    pub fn read_cache(reader: impl std::io::Read) -> std::io::Result<Self> {
        let mut reader = CachedReader {
            inner: reader,
            checksum: CHECKSUM_OFFSET,
        };
        let mut magic = [0u8; CACHE_MAGIC.len()];
        reader.read_exact(&mut magic)?;
        if magic != *CACHE_MAGIC {
            return Err(cache_invalid("flow candidate cache has a wrong magic"));
        }
        if read_u32(&mut reader)? != CACHE_VERSION {
            return Err(cache_invalid("flow candidate cache has a wrong version"));
        }
        let layout = read_cached_stream(&mut reader)?;
        let stream = read_cached_stream(&mut reader)?;
        let baseline = read_cached_stream(&mut reader)?;
        let paint_inventory = read_cached_paint(&mut reader)?;
        let checksum = reader.checksum;
        let mut stored = [0u8; 8];
        reader.inner.read_exact(&mut stored)?;
        if u64::from_le_bytes(stored) != checksum {
            return Err(cache_invalid("flow candidate cache checksum mismatch"));
        }
        Ok(Self {
            candidates: Candidates {
                layout,
                stream,
                baseline,
            },
            paint_inventory,
        })
    }
}

/// Largest accepted paint item count in one cached side.
const CACHE_MAX_PAINT_ITEMS: u64 = 1_000_000;
/// Largest accepted single paint identity string.
const CACHE_MAX_PAINT_IDENTITY: usize = 1_024;

/// Writes the optional paint inventory after the three order streams.
fn write_cached_paint(
    writer: &mut impl std::io::Write,
    paints: Option<&[PaintIdentity]>,
) -> std::io::Result<()> {
    match paints {
        None => writer.write_all(&[0]),
        Some(paints) => {
            writer.write_all(&[1])?;
            writer.write_all(&(paints.len() as u64).to_le_bytes())?;
            for paint in paints {
                writer.write_all(&paint.page.0.to_le_bytes())?;
                writer.write_all(&[paint_kind_tag(paint.kind)])?;
                match &paint.identity {
                    None => writer.write_all(&[0])?,
                    Some(identity) => {
                        if identity.len() > CACHE_MAX_PAINT_IDENTITY {
                            return Err(cache_invalid("paint identity exceeds its cache bound"));
                        }
                        writer.write_all(&[1])?;
                        writer.write_all(&(identity.len() as u32).to_le_bytes())?;
                        writer.write_all(identity.as_bytes())?;
                    }
                }
                write_optional_identity(&mut *writer, paint.shape.as_deref())?;
                match paint.bounds {
                    None => writer.write_all(&[0])?,
                    Some(bounds) => {
                        writer.write_all(&[1])?;
                        for value in [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y] {
                            writer.write_all(&value.to_bits().to_le_bytes())?;
                        }
                    }
                }
            }
            Ok(())
        }
    }
}

fn write_optional_identity(
    writer: &mut impl std::io::Write,
    identity: Option<&str>,
) -> std::io::Result<()> {
    match identity {
        None => writer.write_all(&[0]),
        Some(identity) => {
            if identity.len() > CACHE_MAX_PAINT_IDENTITY {
                return Err(cache_invalid("paint identity exceeds its cache bound"));
            }
            writer.write_all(&[1])?;
            writer.write_all(&(identity.len() as u32).to_le_bytes())?;
            writer.write_all(identity.as_bytes())
        }
    }
}

/// Reads the paint inventory written by [`write_cached_paint`].
fn read_cached_paint(
    reader: &mut impl std::io::Read,
) -> std::io::Result<Option<Vec<PaintIdentity>>> {
    let mut present = [0u8; 1];
    reader.read_exact(&mut present)?;
    match present[0] {
        0 => Ok(None),
        1 => {
            let count = read_u64(reader)?;
            if count > CACHE_MAX_PAINT_ITEMS {
                return Err(cache_invalid(
                    "flow candidate cache has too many paint items",
                ));
            }
            let mut paints = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let page = PageId(read_u32(reader)?);
                let mut kind = [0u8; 1];
                reader.read_exact(&mut kind)?;
                let kind = paint_kind(kind[0]).ok_or_else(|| {
                    cache_invalid("flow candidate cache has an invalid paint kind")
                })?;
                let mut identity_present = [0u8; 1];
                reader.read_exact(&mut identity_present)?;
                let identity = match identity_present[0] {
                    0 => None,
                    1 => {
                        let length = read_u32(reader)? as usize;
                        if length > CACHE_MAX_PAINT_IDENTITY {
                            return Err(cache_invalid(
                                "flow candidate cache has an oversized paint identity",
                            ));
                        }
                        let mut bytes = vec![0u8; length];
                        reader.read_exact(&mut bytes)?;
                        Some(String::from_utf8(bytes).map_err(|_| {
                            cache_invalid("flow candidate cache has invalid paint identity text")
                        })?)
                    }
                    _ => {
                        return Err(cache_invalid(
                            "flow candidate cache has an invalid paint flag",
                        ));
                    }
                };
                let shape = read_optional_identity(reader)?;
                let mut bounds_present = [0u8; 1];
                reader.read_exact(&mut bounds_present)?;
                let bounds = match bounds_present[0] {
                    0 => None,
                    1 => Some(Rect {
                        min: Vec2 {
                            x: read_f64(reader)?,
                            y: read_f64(reader)?,
                        },
                        max: Vec2 {
                            x: read_f64(reader)?,
                            y: read_f64(reader)?,
                        },
                    }),
                    _ => {
                        return Err(cache_invalid(
                            "flow candidate cache has an invalid bounds flag",
                        ));
                    }
                };
                paints.push(PaintIdentity {
                    page,
                    kind,
                    identity,
                    shape,
                    bounds,
                });
            }
            Ok(Some(paints))
        }
        _ => Err(cache_invalid(
            "flow candidate cache has an invalid paint census flag",
        )),
    }
}

fn read_optional_identity(reader: &mut impl std::io::Read) -> std::io::Result<Option<String>> {
    let mut present = [0u8; 1];
    reader.read_exact(&mut present)?;
    match present[0] {
        0 => Ok(None),
        1 => {
            let length = read_u32(reader)? as usize;
            if length > CACHE_MAX_PAINT_IDENTITY {
                return Err(cache_invalid(
                    "flow candidate cache has an oversized paint identity",
                ));
            }
            let mut bytes = vec![0u8; length];
            reader.read_exact(&mut bytes)?;
            Ok(Some(String::from_utf8(bytes).map_err(|_| {
                cache_invalid("flow candidate cache has invalid paint identity text")
            })?))
        }
        _ => Err(cache_invalid(
            "flow candidate cache has an invalid identity flag",
        )),
    }
}

/// Stable tag for one paint kind in the cache.
fn paint_kind_tag(kind: NonTextPaintKind) -> u8 {
    match kind {
        NonTextPaintKind::Path => 0,
        NonTextPaintKind::Image => 1,
        NonTextPaintKind::InlineImage => 2,
        NonTextPaintKind::Shading => 3,
        NonTextPaintKind::Opaque => 4,
    }
}

fn paint_kind(tag: u8) -> Option<NonTextPaintKind> {
    match tag {
        0 => Some(NonTextPaintKind::Path),
        1 => Some(NonTextPaintKind::Image),
        2 => Some(NonTextPaintKind::InlineImage),
        3 => Some(NonTextPaintKind::Shading),
        4 => Some(NonTextPaintKind::Opaque),
        _ => None,
    }
}

/// Magic bytes of the flow candidate cache.
const CACHE_MAGIC: &[u8; 8] = b"PDFDFLOW";
/// Version of the flow candidate cache layout.
const CACHE_VERSION: u32 = 8;
/// Largest accepted token count of one cached hypothesis.
const CACHE_MAX_TOKENS: u64 = 100_000_000;
/// Largest accepted token text in a cached hypothesis.
const CACHE_MAX_TEXT_BYTES: usize = 1 << 20;

/// FNV-1a 64-bit offset basis for the cache checksum.
const CHECKSUM_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
/// FNV-1a 64-bit prime for the cache checksum.
const CHECKSUM_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Updates a FNV-1a checksum with `bytes`.
fn checksum_update(mut checksum: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        checksum ^= u64::from(*byte);
        checksum = checksum.wrapping_mul(CHECKSUM_PRIME);
    }
    checksum
}

/// Writer that hashes every byte written to the cache payload.
struct CachedWriter<W> {
    inner: W,
    checksum: u64,
}

impl<W: std::io::Write> std::io::Write for CachedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.checksum = checksum_update(self.checksum, &bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reader that hashes every payload byte read from the cache.
struct CachedReader<R> {
    inner: R,
    checksum: u64,
}

impl<R: std::io::Read> std::io::Read for CachedReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(bytes)?;
        self.checksum = checksum_update(self.checksum, &bytes[..read]);
        Ok(read)
    }
}

/// Builds an invalid-data error for a malformed cache.
fn cache_invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.to_owned())
}

/// Reads one little-endian `u32`.
fn read_u32(reader: &mut impl std::io::Read) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Reads one little-endian `u64`.
fn read_u64(reader: &mut impl std::io::Read) -> std::io::Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Reads one little-endian `f64` bit pattern.
fn read_f64(reader: &mut impl std::io::Read) -> std::io::Result<f64> {
    Ok(f64::from_bits(read_u64(reader)?))
}

/// Writes one cache stream.
fn write_cached_stream(
    writer: &mut impl std::io::Write,
    stream: &FlowStream,
) -> std::io::Result<()> {
    writer.write_all(&(stream.tokens.len() as u64).to_le_bytes())?;
    for token in &stream.tokens {
        writer.write_all(&token.page.0.to_le_bytes())?;
        writer.write_all(&(token.block as u64).to_le_bytes())?;
        let flags = u8::from(token.bounds.is_some())
            | (u8::from(token.uncertain) << 1)
            | (u8::from(token.line_break_after) << 2)
            | (u8::from(token.font_size.is_some()) << 3)
            | (u8::from(token.raw.is_some()) << 4)
            | (u8::from(token.position.is_some()) << 5)
            | (u8::from(token.page_break_after) << 6);
        writer.write_all(&[flags])?;
        if let Some(bounds) = token.bounds {
            writer.write_all(&bounds.min.x.to_bits().to_le_bytes())?;
            writer.write_all(&bounds.min.y.to_bits().to_le_bytes())?;
            writer.write_all(&bounds.max.x.to_bits().to_le_bytes())?;
            writer.write_all(&bounds.max.y.to_bits().to_le_bytes())?;
        }
        if let Some(font_size) = token.font_size {
            writer.write_all(&font_size.to_bits().to_le_bytes())?;
        }
        if let Some(position) = token.position {
            let baseline = position.baseline();
            let direction = position.direction();
            writer.write_all(&baseline.x.to_bits().to_le_bytes())?;
            writer.write_all(&baseline.y.to_bits().to_le_bytes())?;
            writer.write_all(&direction.x.to_bits().to_le_bytes())?;
            writer.write_all(&direction.y.to_bits().to_le_bytes())?;
        }
        if let Some(raw) = &token.raw {
            if raw.len() > CACHE_MAX_TEXT_BYTES {
                return Err(cache_invalid("flow candidate cache has oversized raw text"));
            }
            writer.write_all(&(raw.len() as u32).to_le_bytes())?;
            writer.write_all(raw.as_bytes())?;
        }
        writer.write_all(&[token.normalization])?;
        writer.write_all(&(token.text.len() as u32).to_le_bytes())?;
        writer.write_all(token.text.as_bytes())?;
    }
    writer.write_all(&(stream.conservation.visible as u64).to_le_bytes())?;
    writer.write_all(&(stream.conservation.used as u64).to_le_bytes())?;
    writer.write_all(&(stream.conservation.pages.len() as u64).to_le_bytes())?;
    for page in &stream.conservation.pages {
        writer.write_all(&page.page.0.to_le_bytes())?;
        writer.write_all(&(page.visible as u64).to_le_bytes())?;
        writer.write_all(&(page.used as u64).to_le_bytes())?;
    }
    // Candidates keep no furniture; the count keeps the format explicit.
    writer.write_all(&0u64.to_le_bytes())?;
    Ok(())
}

/// Reads one cache stream.
fn read_cached_stream(reader: &mut impl std::io::Read) -> std::io::Result<FlowStream> {
    let count = read_u64(reader)?;
    if count > CACHE_MAX_TOKENS {
        return Err(cache_invalid("flow candidate cache has too many tokens"));
    }
    let count = count as usize;
    let mut tokens = Vec::with_capacity(count);
    for _ in 0..count {
        let page = PageId(read_u32(reader)?);
        let block = read_u64(reader)? as usize;
        let mut flags = [0u8; 1];
        reader.read_exact(&mut flags)?;
        let bounds = if flags[0] & 1 != 0 {
            Some(Rect {
                min: Vec2 {
                    x: read_f64(reader)?,
                    y: read_f64(reader)?,
                },
                max: Vec2 {
                    x: read_f64(reader)?,
                    y: read_f64(reader)?,
                },
            })
        } else {
            None
        };
        let font_size = if flags[0] & 8 != 0 {
            Some(read_f64(reader)?)
        } else {
            None
        };
        let position = if flags[0] & 32 != 0 {
            let baseline = Vec2 {
                x: read_f64(reader)?,
                y: read_f64(reader)?,
            };
            let direction = Vec2 {
                x: read_f64(reader)?,
                y: read_f64(reader)?,
            };
            Some(PositionSignature::new(baseline, direction).ok_or_else(|| {
                cache_invalid("flow candidate cache has invalid position evidence")
            })?)
        } else {
            None
        };
        let raw = if flags[0] & 16 != 0 {
            let raw_len = read_u32(reader)? as usize;
            if raw_len > CACHE_MAX_TEXT_BYTES {
                return Err(cache_invalid("flow candidate cache has oversized raw text"));
            }
            let mut raw = vec![0u8; raw_len];
            reader.read_exact(&mut raw)?;
            Some(
                String::from_utf8(raw)
                    .map_err(|_| cache_invalid("flow candidate cache has invalid raw text"))?
                    .into_boxed_str(),
            )
        } else {
            None
        };
        let mut normalization = [0u8; 1];
        reader.read_exact(&mut normalization)?;
        let text_len = read_u32(reader)? as usize;
        if text_len > CACHE_MAX_TEXT_BYTES {
            return Err(cache_invalid("flow candidate cache has an oversized token"));
        }
        let mut text = vec![0u8; text_len];
        reader.read_exact(&mut text)?;
        let text = String::from_utf8(text)
            .map_err(|_| cache_invalid("flow candidate cache has invalid text"))?;
        tokens.push(FlowToken {
            text,
            page,
            bounds,
            block,
            uncertain: flags[0] & 2 != 0,
            line_break_after: flags[0] & 4 != 0,
            font_size,
            raw,
            position,
            page_break_after: flags[0] & 64 != 0,
            normalization: normalization[0],
        });
    }
    let visible = read_u64(reader)? as usize;
    let used = read_u64(reader)? as usize;
    let page_count = read_u64(reader)?;
    if page_count > CACHE_MAX_TOKENS {
        return Err(cache_invalid("flow candidate cache has too many pages"));
    }
    let mut pages = Vec::with_capacity(page_count as usize);
    for _ in 0..page_count {
        pages.push(PageConservation {
            page: PageId(read_u32(reader)?),
            visible: read_u64(reader)? as usize,
            used: read_u64(reader)? as usize,
        });
    }
    let furniture = read_u64(reader)?;
    if furniture != 0 {
        return Err(cache_invalid(
            "flow candidate cache has unexpected furniture",
        ));
    }
    Ok(FlowStream {
        tokens,
        source_glyphs: Vec::new(),
        furniture: Vec::new(),
        conservation: Conservation {
            visible,
            used,
            pages,
        },
    })
}

/// Builds all reading-order hypotheses for one document.
///
/// The caller can drop the extracted document immediately after this returns,
/// which keeps the second side's extraction from coexisting with the first
/// side's glyph document.
///
/// # Errors
///
/// Returns an error when layout reconstruction, paint-order layout, or
/// normalization fails.
pub fn flow_candidates(document: &Document<Glyph>) -> Result<FlowCandidates> {
    flow_candidates_with_sources(document, false)
}

/// Builds all order hypotheses and retains each token's source glyph ids.
///
/// Used by the full report. The default path calls
/// [`flow_candidates`], which leaves the source vectors empty.
///
/// # Errors
///
/// Returns the same errors as [`flow_candidates`].
pub fn flow_candidates_with_sources(
    document: &Document<Glyph>,
    retain_sources: bool,
) -> Result<FlowCandidates> {
    let paint_inventory = paint_inventory(document);
    let candidates = candidates(document, retain_sources)?;
    Ok(FlowCandidates {
        candidates,
        paint_inventory,
    })
}

/// Builds all reading-order hypotheses while consuming the document.
///
/// This avoids keeping the extracted glyph document and a filtered clone of
/// it alive at the same time.
///
/// # Errors
///
/// Returns an error when layout reconstruction, paint-order layout, or
/// normalization fails.
pub fn flow_candidates_owned(document: Document<Glyph>) -> Result<FlowCandidates> {
    flow_candidates_owned_with_sources(document, false)
}

/// Builds all order hypotheses while consuming the document and optionally
/// retaining source glyph ids for a full report.
///
/// # Errors
///
/// Returns the same errors as [`flow_candidates_owned`].
pub fn flow_candidates_owned_with_sources(
    document: Document<Glyph>,
    retain_sources: bool,
) -> Result<FlowCandidates> {
    let paint_inventory = paint_inventory(&document);
    let visible = visible_document_owned(document);
    let candidates = candidates_from_visible(visible, retain_sources)?;
    Ok(FlowCandidates {
        candidates,
        paint_inventory,
    })
}

/// Maps the extracted paint census into the compact flow inventory.
fn paint_inventory(document: &Document<Glyph>) -> Option<Vec<PaintIdentity>> {
    document.non_text_paint_bounds().map(|paints| {
        paints
            .iter()
            .map(|paint| PaintIdentity {
                page: paint.page,
                kind: paint.kind,
                identity: paint.identity.clone(),
                shape: paint.shape.clone(),
                bounds: paint.bounds,
            })
            .collect()
    })
}

/// Chooses page orders from both sides' candidates and returns the flows.
///
/// # Errors
///
/// Returns an error when either side has no candidate stream.
pub fn flow_order_from_candidates(
    old: FlowCandidates,
    new: FlowCandidates,
) -> Result<FlowOrderPair> {
    let old = old.candidates;
    let new = new.candidates;
    let old_side = SidePages::of(&old);
    let new_side = SidePages::of(&new);
    let old_evidence = EvidenceView::of(&new, &new_side);
    let new_evidence = EvidenceView::of(&old, &old_side);
    let index = CounterpartIndex::of(&old, &new, &old_side, &new_side);
    let old_initial = choose_pages(&old, &old_side, &old_evidence, index.for_old());
    let new_initial = choose_pages(&new, &new_side, &new_evidence, index.for_new());
    // A page split into stacked bands has no single page-level reading order.
    // When a band reproduces one whole page of the other document, follow that
    // page's chosen order so a merge or break does not change how the text
    // reads. Both sides start from their independent choices, so the pass is
    // symmetric and cannot oscillate.
    let old_stacked = stacked_pages(&old, &old_side);
    let new_stacked = stacked_pages(&new, &new_side);
    let mut old_pages = old_initial.clone();
    let mut new_pages = new_initial.clone();
    adopt_stacked_orders(
        &old,
        &old_side,
        &old_stacked,
        &RegionContext {
            vocabulary: &index.vocabulary,
            other_bags: &index.new_bags,
            other_choices: &new_initial
                .iter()
                .map(|page| (page.page.0, page.order))
                .collect(),
            other_stacked: &new_stacked,
        },
        &mut old_pages,
    );
    adopt_stacked_orders(
        &new,
        &new_side,
        &new_stacked,
        &RegionContext {
            vocabulary: &index.vocabulary,
            other_bags: &index.old_bags,
            other_choices: &old_initial
                .iter()
                .map(|page| (page.page.0, page.order))
                .collect(),
            other_stacked: &old_stacked,
        },
        &mut new_pages,
    );
    // The index and evidence views borrow both candidate sets; their last use
    // is above, so assembly can consume the candidates.
    drop(index);
    let (old_stream, old_pages) = assemble_stream(old, old_pages);
    let (new_stream, new_pages) = assemble_stream(new, new_pages);
    let old_bands = band_positions(&old_pages);
    let new_bands = band_positions(&new_pages);
    let (old_stream, new_stream) =
        furniture::split_pair(old_stream, new_stream, &old_bands, &new_bands);
    Ok(FlowOrderPair {
        old: old_stream,
        new: new_stream,
        old_pages,
        new_pages,
    })
}

fn candidates(document: &Document<Glyph>, retain_sources: bool) -> Result<Candidates> {
    candidates_from_visible(visible_document(document), retain_sources)
}

fn candidates_from_visible(visible: Document<Glyph>, retain_sources: bool) -> Result<Candidates> {
    let glyphs = crate::GlyphLookup::new(&visible);
    let conservation = crate::conservation_of(&visible);
    let lines = reconstruct_lines(&visible, LineOptions::default())?;
    let blocks = reconstruct_blocks(&visible, &lines, BlockOptions::default())?;
    let layout = stream_from_layout(
        &visible,
        &glyphs,
        &conservation,
        &lines,
        &blocks,
        false,
        retain_sources,
    )?;
    let (stream_lines, stream_blocks) = stream_layout(&visible);
    let stream = stream_from_layout(
        &visible,
        &glyphs,
        &conservation,
        &stream_lines,
        &stream_blocks,
        false,
        retain_sources,
    )?;
    let (baseline_lines, baseline_blocks) = baseline_layout(&visible);
    let baseline = stream_from_layout(
        &visible,
        &glyphs,
        &conservation,
        &baseline_lines,
        &baseline_blocks,
        false,
        retain_sources,
    )?;
    Ok(Candidates {
        layout,
        stream,
        baseline,
    })
}

/// Selects one order per page without modifying the candidates.
///
/// The caller must keep the candidates alive while the returned evidence is
/// in use only if it wants to assemble them; [`assemble_stream`] consumes the
/// candidates and moves the selected page tokens instead of cloning them.
fn choose_pages(
    candidates: &Candidates,
    side: &SidePages,
    evidence: &EvidenceView<'_>,
    counterparts: &Counterparts,
) -> Vec<PageOrderEvidence> {
    let mut evidence_pages = Vec::with_capacity(side.pages.len());
    for &page_id in &side.pages {
        let page = PageId(page_id);
        let layout_range = side.layout.get(&page_id).unwrap_or(&EMPTY_RANGE);
        let stream_range = side.stream.get(&page_id).unwrap_or(&EMPTY_RANGE);
        let baseline_range = side.baseline.get(&page_id).unwrap_or(&EMPTY_RANGE);
        let layout = &candidates.layout.tokens[layout_range.clone()];
        let stream = &candidates.stream.tokens[stream_range.clone()];
        let baseline = &candidates.baseline.tokens[baseline_range.clone()];
        let agreement = order_agreement(layout, stream);
        let (selected, uncertain, layout_score, stream_score, baseline_score) =
            if agreement >= ORDER_AGREEMENT_MIN {
                (OrderKind::Layout, false, 0, 0, 0)
            } else {
                let local = counterparts.get(&page_id);
                let page_evidence = local.map(|pages| evidence.evidence(pages));
                let (layout_score, stream_score, baseline_score) =
                    page_evidence.as_ref().map_or((0, 0, 0), |page_evidence| {
                        (
                            page_evidence.score(layout),
                            page_evidence.score(stream),
                            page_evidence.score(baseline),
                        )
                    });
                let layout_coherence = order_coherence(layout);
                let stream_coherence = order_coherence(stream);
                let baseline_coherence = order_coherence(baseline);
                // Layout stays only when it is at least as informative and
                // at least as spatially coherent as paint order. Baseline rows
                // replace paint order only when they are both more informative
                // and at least as coherent. Otherwise keep paint order. A page
                // without a counterpart has no shared n-gram evidence, so only
                // coherence can order it.
                let selected = if page_evidence.is_none() {
                    intrinsic_order(layout_coherence, stream_coherence, baseline_coherence)
                } else if layout_score >= stream_score && layout_coherence >= stream_coherence {
                    OrderKind::Layout
                } else if baseline_score > stream_score && baseline_coherence >= stream_coherence {
                    OrderKind::Baseline
                } else {
                    OrderKind::Stream
                };
                (selected, true, layout_score, stream_score, baseline_score)
            };
        let tokens = match selected {
            OrderKind::Layout => layout,
            OrderKind::Stream => stream,
            OrderKind::Baseline => baseline,
        };
        let coherence = order_coherence(tokens);
        evidence_pages.push(PageOrderEvidence {
            page,
            order: selected,
            uncertain,
            agreement,
            layout_score,
            stream_score,
            baseline_score,
            coherence,
            bands: Vec::new(),
        });
    }
    evidence_pages
}

/// A horizontal band of a page separated from its neighbours by a vertical
/// gap much larger than the line pitch.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Band {
    /// Lower edge in page coordinates (inclusive).
    bottom: f64,
    /// Upper edge in page coordinates (exclusive).
    top: f64,
}

/// Bands of every page that is split into stacked regions.
///
/// A page with one band is absent, so ordinary pages pay only for their own
/// row sort. A merged page has one band per source page.
fn stacked_pages(candidates: &Candidates, pages: &SidePages) -> BTreeMap<u32, Vec<Band>> {
    let mut stacked = BTreeMap::new();
    for &page in &pages.pages {
        let Some(range) = pages.layout.get(&page) else {
            continue;
        };
        // Band geometry is matched against whole-page bags, which are built
        // from the layout hypothesis. Using the same hypothesis keeps token
        // boundaries identical when a band is exactly one counterpart page.
        let bands = stacked_bands(&candidates.layout.tokens[range.clone()]);
        if bands.len() > 1 {
            stacked.insert(page, bands);
        }
    }
    stacked
}

/// Splits a page's paint-order tokens at gaps much larger than the line pitch.
///
/// The result is empty when the page forms one band. Bands are contiguous and
/// ordered top to bottom; their outer edges are infinite so every token
/// belongs to exactly one band. A split needs at least [`STACKED_MIN_BAND_ROWS`]
/// text rows on both sides, and the band boundary sits in the middle of the
/// gap, where no glyph can be assigned to the wrong side.
fn stacked_bands(tokens: &[FlowToken]) -> Vec<Band> {
    if tokens.len() < STACKED_MIN_PAGE_ROWS {
        return Vec::new();
    }
    let mut points = tokens
        .iter()
        .filter_map(|token| {
            token.bounds.map(|bounds| {
                (
                    f64::midpoint(bounds.min.y, bounds.max.y),
                    bounds.min.y,
                    bounds.max.y,
                )
            })
        })
        .collect::<Vec<_>>();
    if points.len() < STACKED_MIN_PAGE_ROWS {
        return Vec::new();
    }
    points.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.total_cmp(&right.1)));
    let mut heights = points
        .iter()
        .map(|(_, minimum, maximum)| maximum - minimum)
        .filter(|height| *height > f64::EPSILON)
        .collect::<Vec<_>>();
    if heights.is_empty() {
        return Vec::new();
    }
    heights.sort_by(f64::total_cmp);
    let median_height = heights[heights.len() / 2];
    if median_height <= f64::EPSILON {
        return Vec::new();
    }
    // Rows are grouped by their vertical center. Superscripts and tall glyph
    // boxes of one row sit within half a font size of the row center.
    let mut rows: Vec<(f64, f64, f64, usize)> = Vec::new();
    for (center, minimum, maximum) in points {
        if let Some(last) = rows.last_mut()
            && (last.0 - center).abs() <= 0.5 * median_height
        {
            last.1 = last.1.min(minimum);
            last.2 = last.2.max(maximum);
            last.3 += 1;
        } else {
            rows.push((center, minimum, maximum, 1));
        }
    }
    if rows.len() < STACKED_MIN_PAGE_ROWS {
        return Vec::new();
    }
    let mut pitches = rows
        .windows(2)
        .map(|pair| (pair[0].0 - pair[1].0).abs())
        .filter(|pitch| *pitch > f64::EPSILON)
        .collect::<Vec<_>>();
    if pitches.is_empty() {
        return Vec::new();
    }
    pitches.sort_by(f64::total_cmp);
    let pitch = pitches[pitches.len() / 2];
    let total_tokens = rows.iter().map(|row| row.3).sum::<usize>();
    let minimum_tokens = (total_tokens as f64 * STACKED_MIN_BAND_TOKEN_FRACTION) as usize;
    let mut splits = Vec::new();
    let mut band_start = 0;
    let mut tokens_in_band = 0;
    for index in 0..rows.len() - 1 {
        let upper = rows[index];
        let lower = rows[index + 1];
        tokens_in_band += upper.3;
        let tokens_below = total_tokens.saturating_sub(tokens_in_band);
        let gap = upper.1 - lower.2;
        let rows_in_band = index + 1 - band_start;
        let rows_below = rows.len() - (index + 1);
        if gap > STACKED_GAP_PITCH_RATIO * pitch
            && gap > 1.5 * median_height
            && rows_in_band >= STACKED_MIN_BAND_ROWS
            && rows_below >= STACKED_MIN_BAND_ROWS
            && tokens_in_band >= minimum_tokens
            && tokens_below >= minimum_tokens
        {
            splits.push(f64::midpoint(upper.1, lower.2));
            band_start = index + 1;
            tokens_in_band = 0;
        }
    }
    if splits.is_empty() {
        return Vec::new();
    }
    let mut bands = Vec::with_capacity(splits.len() + 1);
    let mut top = f64::INFINITY;
    for split in splits {
        bands.push(Band { bottom: split, top });
        top = split;
    }
    bands.push(Band {
        bottom: f64::NEG_INFINITY,
        top,
    });
    bands
}

/// Returns the band index of one token's vertical center.
fn band_of(bands: &[Band], token: &FlowToken) -> usize {
    let y = token.bounds.map_or(f64::INFINITY, |bounds| {
        f64::midpoint(bounds.min.y, bounds.max.y)
    });
    bands
        .iter()
        .position(|band| y >= band.bottom)
        .unwrap_or(bands.len().saturating_sub(1))
}

/// Returns the band index of one token under a per-band order plan.
fn ordered_band_of(bands: &[BandOrder], token: &FlowToken) -> usize {
    let y = token.bounds.map_or(f64::INFINITY, |bounds| {
        f64::midpoint(bounds.min.y, bounds.max.y)
    });
    bands
        .iter()
        .position(|band| y >= band.bottom)
        .unwrap_or(bands.len().saturating_sub(1))
}

/// Information-weighted bag of the tokens assigned to one band.
fn band_bag(
    tokens: &[FlowToken],
    bands: &[Band],
    index: usize,
    vocabulary: &JointVocabulary<'_>,
) -> PageBag {
    let mut counts = BTreeMap::<u32, u64>::new();
    for token in tokens {
        if band_of(bands, token) != index {
            continue;
        }
        if let Some(&id) = vocabulary.ids.get(token.text.as_str()) {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    let counts = counts.into_iter().collect::<Vec<_>>();
    let total = counts
        .iter()
        .map(|&(id, count)| count * vocabulary.weights[id as usize])
        .sum();
    PageBag { counts, total }
}

/// Counterpart information used when a stacked band adopts a whole page's
/// order.
struct RegionContext<'a> {
    vocabulary: &'a JointVocabulary<'a>,
    other_bags: &'a [(u32, PageBag)],
    other_choices: &'a BTreeMap<u32, OrderKind>,
    other_stacked: &'a BTreeMap<u32, Vec<Band>>,
}

/// Lets stacked bands follow the chosen order of the whole pages they
/// reproduce.
///
/// A page made by stacking two pages has no single page-level order: its two
/// bands can correspond to pages whose chosen orders differ. Matching each
/// band's weighted token bag to the other document's whole pages and reusing
/// that page's chosen hypothesis keeps pagination changes from reordering
/// content, while ordinary pages are untouched because a band that is only
/// part of a page cannot reach [`REGION_ADOPT_MIN`].
///
/// Counterparts that are themselves stacked are skipped, so two documents that
/// both stack the same pages keep their independent evidence-based choice.
fn adopt_stacked_orders(
    own: &Candidates,
    own_pages: &SidePages,
    own_stacked: &BTreeMap<u32, Vec<Band>>,
    context: &RegionContext<'_>,
    choices: &mut [PageOrderEvidence],
) {
    for choice in choices.iter_mut() {
        let Some(bands) = own_stacked.get(&choice.page.0) else {
            continue;
        };
        let Some(range) = own_pages.layout.get(&choice.page.0) else {
            continue;
        };
        let tokens = &own.layout.tokens[range.clone()];
        let adopted = (0..bands.len())
            .map(|index| {
                let bag = band_bag(tokens, bands, index, context.vocabulary);
                let mut best: Option<(f64, u32)> = None;
                for (page, other_bag) in context.other_bags {
                    let similarity = bag_similarity(&bag, other_bag, &context.vocabulary.weights);
                    let replace = match best {
                        None => true,
                        Some((best_similarity, best_page)) => {
                            match similarity.total_cmp(&best_similarity) {
                                Ordering::Greater => true,
                                Ordering::Equal => *page < best_page,
                                Ordering::Less => false,
                            }
                        }
                    };
                    if replace {
                        best = Some((similarity, *page));
                    }
                }
                best.and_then(|(similarity, page)| {
                    if similarity < REGION_ADOPT_MIN || context.other_stacked.contains_key(&page) {
                        return None;
                    }
                    context.other_choices.get(&page).copied()
                })
            })
            .collect::<Vec<_>>();
        if adopted.iter().all(Option::is_none) {
            continue;
        }
        let kinds = adopted
            .iter()
            .map(|kind| kind.unwrap_or(choice.order))
            .collect::<Vec<_>>();
        // Keep the band geometry even when every band chose the same order:
        // furniture positions are relative to each band, exactly like the
        // separate pages the bands came from.
        choice.order = kinds[0];
        choice.bands = bands
            .iter()
            .zip(&kinds)
            .map(|(band, kind)| BandOrder {
                bottom: band.bottom,
                top: band.top,
                order: *kind,
            })
            .collect();
    }
}

/// Vertical band extents of every page that assembled per-band orders.
///
/// Furniture positions are relative to a band, not to the whole page, so a
/// page stack keeps the same header and footer positions as the pages it was
/// made from.
fn band_positions(pages: &[PageOrderEvidence]) -> BTreeMap<u32, Vec<(f64, f64)>> {
    pages
        .iter()
        .filter(|page| !page.bands.is_empty())
        .map(|page| {
            (
                page.page.0,
                page.bands
                    .iter()
                    .map(|band| (band.bottom, band.top))
                    .collect(),
            )
        })
        .collect()
}

/// Joins the selected page token runs into one stream.
///
/// Every selected page is moved out of its candidate instead of cloned. The
/// candidate ranges are drained in descending page order, so each drain ends
/// at the current end of the token vector and no earlier token is shifted.
fn assemble_stream(
    mut candidates: Candidates,
    pages: Vec<PageOrderEvidence>,
) -> (FlowStream, Vec<PageOrderEvidence>) {
    let mut selected = [BTreeSet::new(), BTreeSet::new(), BTreeSet::new()];
    for evidence in &pages {
        let index = match evidence.order {
            OrderKind::Layout => 0,
            OrderKind::Stream => 1,
            OrderKind::Baseline => 2,
        };
        selected[index].insert(evidence.page.0);
        if !evidence.bands.is_empty() {
            // A banded page mixes hypotheses, so every hypothesis run for it
            // is needed for the per-band assembly.
            for kind in &mut selected {
                kind.insert(evidence.page.0);
            }
        }
    }
    let layout_ranges = page_ranges(&candidates.layout);
    let stream_ranges = page_ranges(&candidates.stream);
    let baseline_ranges = page_ranges(&candidates.baseline);
    let mut layout_taken =
        take_selected_pages(&mut candidates.layout, &layout_ranges, &selected[0]);
    let mut stream_taken =
        take_selected_pages(&mut candidates.stream, &stream_ranges, &selected[1]);
    let mut baseline_taken =
        take_selected_pages(&mut candidates.baseline, &baseline_ranges, &selected[2]);
    let mut chosen = Vec::new();
    let mut chosen_sources = Vec::new();
    for evidence in &pages {
        if evidence.bands.is_empty() {
            let taken = match evidence.order {
                OrderKind::Layout => &mut layout_taken,
                OrderKind::Stream => &mut stream_taken,
                OrderKind::Baseline => &mut baseline_taken,
            };
            if let Some((mut tokens, mut sources)) = taken.remove(&evidence.page.0) {
                chosen.append(&mut tokens);
                chosen_sources.append(&mut sources);
            }
            continue;
        }
        let mut runs = [
            layout_taken.remove(&evidence.page.0),
            stream_taken.remove(&evidence.page.0),
            baseline_taken.remove(&evidence.page.0),
        ];
        for (index, band) in evidence.bands.iter().enumerate() {
            let run_index = match band.order {
                OrderKind::Layout => 0,
                OrderKind::Stream => 1,
                OrderKind::Baseline => 2,
            };
            let Some((run_tokens, run_sources)) = runs[run_index].as_mut() else {
                continue;
            };
            let mut kept = Vec::new();
            let mut remaining = Vec::new();
            let mut kept_sources = Vec::new();
            let mut remaining_sources = Vec::new();
            let retained_sources = !run_sources.is_empty();
            let mut source_iter = std::mem::take(run_sources).into_iter();
            for token in run_tokens.drain(..) {
                let source = if retained_sources {
                    source_iter.next().unwrap_or_default()
                } else {
                    Vec::new()
                };
                if ordered_band_of(&evidence.bands, &token) == index {
                    kept.push(token);
                    if retained_sources {
                        kept_sources.push(source);
                    }
                } else {
                    remaining.push(token);
                    if retained_sources {
                        remaining_sources.push(source);
                    }
                }
            }
            *run_tokens = remaining;
            *run_sources = if retained_sources {
                remaining_sources
            } else {
                Vec::new()
            };
            if let Some(last) = kept.last_mut() {
                // The band ends at a line boundary; keep furniture line
                // splitting from joining text across the band seam.
                last.line_break_after = true;
            }
            chosen.append(&mut kept);
            chosen_sources.append(&mut kept_sources);
        }
    }
    let stream = FlowStream {
        tokens: chosen,
        source_glyphs: chosen_sources,
        furniture: Vec::new(),
        conservation: candidates.layout.conservation,
    };
    (stream, pages)
}

/// Removes the selected page token runs from `stream`.
fn take_selected_pages(
    stream: &mut FlowStream,
    ranges: &BTreeMap<u32, Range<usize>>,
    selected: &BTreeSet<u32>,
) -> BTreeMap<u32, (Vec<FlowToken>, Vec<Vec<GlyphId>>)> {
    let mut taken = BTreeMap::new();
    let retained_sources = !stream.source_glyphs.is_empty();
    for (&page, range) in ranges.iter().rev() {
        if selected.contains(&page) {
            let sources = if retained_sources {
                stream.source_glyphs.drain(range.clone()).collect()
            } else {
                Vec::new()
            };
            let tokens = stream.tokens.drain(range.clone()).collect();
            taken.insert(page, (tokens, sources));
        }
    }
    taken
}

fn page_ranges(stream: &FlowStream) -> BTreeMap<u32, Range<usize>> {
    let mut pages = BTreeMap::new();
    let mut start = 0usize;
    for (index, token) in stream.tokens.iter().enumerate() {
        if index == 0 {
            start = 0;
            continue;
        }
        if token.page.0 != stream.tokens[index - 1].page.0 {
            pages.insert(stream.tokens[start].page.0, start..index);
            start = index;
        }
    }
    if let Some(last) = stream.tokens.last() {
        pages.insert(last.page.0, start..stream.tokens.len());
    }
    pages
}

/// Orders a page from spatial coherence alone, for a page without a
/// counterpart. Ties keep layout, then paint order, mirroring the evidence
/// rule.
fn intrinsic_order(layout: f64, stream: f64, baseline: f64) -> OrderKind {
    if layout >= stream && layout >= baseline {
        OrderKind::Layout
    } else if baseline > stream && baseline > layout {
        OrderKind::Baseline
    } else {
        OrderKind::Stream
    }
}

/// Token ranges of one document side's three hypotheses, grouped by page.
struct SidePages {
    layout: BTreeMap<u32, Range<usize>>,
    stream: BTreeMap<u32, Range<usize>>,
    baseline: BTreeMap<u32, Range<usize>>,
    /// Pages present in at least one hypothesis, ascending.
    pages: Vec<u32>,
}

static EMPTY_RANGE: Range<usize> = 0..0;

impl SidePages {
    fn of(candidates: &Candidates) -> Self {
        let layout = page_ranges(&candidates.layout);
        let stream = page_ranges(&candidates.stream);
        let baseline = page_ranges(&candidates.baseline);
        let mut pages = layout
            .keys()
            .chain(stream.keys())
            .chain(baseline.keys())
            .copied()
            .collect::<Vec<_>>();
        pages.sort_unstable();
        pages.dedup();
        Self {
            layout,
            stream,
            baseline,
            pages,
        }
    }
}

/// The other document's candidate streams, from which one page's evidence is
/// drawn.
struct EvidenceView<'a> {
    candidates: &'a Candidates,
    pages: &'a SidePages,
}

impl<'a> EvidenceView<'a> {
    fn of(candidates: &'a Candidates, pages: &'a SidePages) -> Self {
        Self { candidates, pages }
    }

    /// Builds the n-gram evidence of the counterpart pages' three hypotheses.
    fn evidence(&self, counterparts: &[(u32, f64)]) -> PageEvidence<'a> {
        let mut runs = Vec::with_capacity(counterparts.len() * 3);
        for &(page, _similarity) in counterparts {
            for (range, stream) in [
                (self.pages.layout.get(&page), &self.candidates.layout),
                (self.pages.stream.get(&page), &self.candidates.stream),
                (self.pages.baseline.get(&page), &self.candidates.baseline),
            ] {
                if let Some(range) = range {
                    runs.push(&stream.tokens[range.clone()]);
                }
            }
        }
        PageEvidence::of(&runs)
    }
}

/// Information-weighted n-grams of the pages a page corresponds to.
///
/// The weight table is built from the counterpart pages alone, so adding or
/// removing an unrelated page cannot change a page's scores.
struct PageEvidence<'a> {
    ids: HashMap<&'a str, u32>,
    weights: Vec<u64>,
    bigrams: HashSet<(u32, u32)>,
    trigrams: HashSet<(u32, u32, u32)>,
}

impl<'a> PageEvidence<'a> {
    fn of(runs: &[&'a [FlowToken]]) -> Self {
        let mut counts: HashMap<&'a str, u64> = HashMap::new();
        let mut total = 0u64;
        for run in runs {
            for token in *run {
                *counts.entry(token.text.as_str()).or_insert(0) += 1;
                total = total.saturating_add(1);
            }
        }
        let mut entries = counts.into_iter().collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut ids = HashMap::with_capacity(entries.len());
        let mut weights = Vec::with_capacity(entries.len());
        for (text, count) in entries {
            let id = u32::try_from(weights.len()).expect("token id fits u32");
            let ratio = total as f64 / count.max(1) as f64;
            let weight = ((8.0 * ratio.log2()).round() as u64).max(1);
            ids.insert(text, id);
            weights.push(weight);
        }
        let mut bigrams = HashSet::new();
        let mut trigrams = HashSet::new();
        for run in runs {
            let mapped = run
                .iter()
                .map(|token| ids[token.text.as_str()])
                .collect::<Vec<_>>();
            for window in mapped.windows(2) {
                bigrams.insert((window[0], window[1]));
            }
            for window in mapped.windows(3) {
                trigrams.insert((window[0], window[1], window[2]));
            }
        }
        Self {
            ids,
            weights,
            bigrams,
            trigrams,
        }
    }

    /// Sums the information weight of every bigram and trigram of `tokens`
    /// that also occurs in the counterpart evidence. Tokens outside the local
    /// vocabulary cannot match, so their windows score nothing.
    fn score(&self, tokens: &[FlowToken]) -> u64 {
        let ids = tokens
            .iter()
            .map(|token| self.ids.get(token.text.as_str()).copied())
            .collect::<Vec<_>>();
        let mut score = 0u64;
        for window in ids.windows(2) {
            if let [Some(left), Some(right)] = window
                && self.bigrams.contains(&(*left, *right))
            {
                score = score
                    .saturating_add(self.weights[*left as usize])
                    .saturating_add(self.weights[*right as usize]);
            }
        }
        for window in ids.windows(3) {
            if let [Some(first), Some(second), Some(third)] = window
                && self.trigrams.contains(&(*first, *second, *third))
            {
                score = score
                    .saturating_add(self.weights[*first as usize])
                    .saturating_add(self.weights[*second as usize])
                    .saturating_add(self.weights[*third as usize]);
            }
        }
        score
    }
}

/// Counterpart pages of one side, with the weighted bag overlap that selected
/// them, ascending by page.
type Counterparts = BTreeMap<u32, Vec<(u32, f64)>>;

/// Pages of one document and the pages of the other document they correspond
/// to.
///
/// The shared vocabulary and the per-page bags are retained so a stacked band
/// can be matched to a whole page without rebuilding the page index.
struct CounterpartIndex<'a> {
    old: Counterparts,
    new: Counterparts,
    vocabulary: JointVocabulary<'a>,
    old_bags: Vec<(u32, PageBag)>,
    new_bags: Vec<(u32, PageBag)>,
}

impl<'a> CounterpartIndex<'a> {
    fn of(
        old: &'a Candidates,
        new: &'a Candidates,
        old_pages: &SidePages,
        new_pages: &SidePages,
    ) -> Self {
        let vocabulary = JointVocabulary::of(old, new);
        let old_bags = page_bags(old, old_pages, &vocabulary);
        let new_bags = page_bags(new, new_pages, &vocabulary);
        let (old, new) = counterpart_maps(&old_bags, &new_bags, &vocabulary.weights);
        Self {
            old,
            new,
            vocabulary,
            old_bags,
            new_bags,
        }
    }

    fn for_old(&self) -> &Counterparts {
        &self.old
    }

    fn for_new(&self) -> &Counterparts {
        &self.new
    }
}

/// One token vocabulary and weight table shared by both documents, so a bag
/// overlap is symmetric under swapping the sides.
struct JointVocabulary<'a> {
    ids: HashMap<&'a str, u32>,
    weights: Vec<u64>,
}

impl<'a> JointVocabulary<'a> {
    fn of(old: &'a Candidates, new: &'a Candidates) -> Self {
        let mut counts: HashMap<&'a str, u64> = HashMap::new();
        let mut total = 0u64;
        for stream in [&old.layout, &new.layout] {
            for token in &stream.tokens {
                *counts.entry(token.text.as_str()).or_insert(0) += 1;
                total = total.saturating_add(1);
            }
        }
        let mut entries = counts.into_iter().collect::<Vec<_>>();
        entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut ids = HashMap::with_capacity(entries.len());
        let mut weights = Vec::with_capacity(entries.len());
        for (text, count) in entries {
            let id = u32::try_from(weights.len()).expect("token id fits u32");
            let ratio = total as f64 / count.max(1) as f64;
            let weight = ((8.0 * ratio.log2()).round() as u64).max(1);
            ids.insert(text, id);
            weights.push(weight);
        }
        Self { ids, weights }
    }
}

/// A page's token multiset in the shared vocabulary, ascending by token id.
struct PageBag {
    counts: Vec<(u32, u64)>,
    /// Sum of `count * weight`, used as the bag's weighted size.
    total: u64,
}

fn page_bags(
    candidates: &Candidates,
    pages: &SidePages,
    vocabulary: &JointVocabulary<'_>,
) -> Vec<(u32, PageBag)> {
    let mut bags = Vec::with_capacity(pages.pages.len());
    for &page in &pages.pages {
        let range = pages.layout.get(&page).unwrap_or(&EMPTY_RANGE);
        let mut counts = BTreeMap::<u32, u64>::new();
        for token in &candidates.layout.tokens[range.clone()] {
            let id = vocabulary.ids[token.text.as_str()];
            *counts.entry(id).or_insert(0) += 1;
        }
        let counts = counts.into_iter().collect::<Vec<_>>();
        let total = counts
            .iter()
            .map(|&(id, count)| count * vocabulary.weights[id as usize])
            .sum();
        bags.push((page, PageBag { counts, total }));
    }
    bags
}

/// Counterpart candidates of one page before the document floor is known.
///
/// Only a bounded number of matches is retained per page, so counterpart
/// search memory stays linear in the page count rather than quadratic.
struct CounterpartCandidates {
    page: u32,
    /// Best overlap with any page of the other document.
    best: f64,
    /// Pages at or above the retain fraction of the best overlap, best first.
    retained: Vec<(u32, f64)>,
    /// Overlaps of the pages adjacent to the best match.
    adjacent: Vec<(u32, f64)>,
}

/// Selects counterpart pages by information-weighted bag overlap.
///
/// Every page pair is compared: pages revised between versions can have
/// almost no rare token in common with their counterpart, so a shortlist must
/// not drop them. The similarity floor is relative to the document pair's own
/// typical best overlap, so a page deleted from a near-identical edit falls
/// back to the intrinsic rule while a heavily revised page can still use a
/// genuinely corresponding page. The best match's neighbouring pages are also
/// included, so a page shifted by an insertion or deletion still sees its
/// structural neighbours.
fn counterpart_maps(
    old: &[(u32, PageBag)],
    new: &[(u32, PageBag)],
    weights: &[u64],
) -> (Counterparts, Counterparts) {
    (
        apply_document_floor(&page_candidates(old, new, weights)),
        apply_document_floor(&page_candidates(new, old, weights)),
    )
}

/// Compares every page of `own` with every page of `other` and retains only
/// the bounded best matches for each page.
fn page_candidates(
    own: &[(u32, PageBag)],
    other: &[(u32, PageBag)],
    weights: &[u64],
) -> Vec<CounterpartCandidates> {
    // Inverted index from a token id to the pages that contain it. Only page
    // pairs that share a token have a nonzero information-weighted overlap, so
    // walking postings scores exactly the pairs that can become a counterpart.
    // Common tokens can keep a large posting list, but their contributions are
    // still accumulated exactly; no token is dropped.
    let token_count = own
        .iter()
        .chain(other)
        .filter_map(|(_, bag)| bag.counts.last().map(|&(id, _)| id as usize + 1))
        .max()
        .unwrap_or(0);
    let mut postings: Vec<Vec<(u32, u64)>> = vec![Vec::new(); token_count];
    for (index, (_, bag)) in other.iter().enumerate() {
        for &(id, count) in &bag.counts {
            postings[id as usize].push((index as u32, count));
        }
    }
    let other_totals = other.iter().map(|(_, bag)| bag.total).collect::<Vec<_>>();
    let mut intersections = vec![0u64; other.len()];
    let mut touched = Vec::with_capacity(other.len().min(64));
    let mut candidates = Vec::with_capacity(own.len());
    for (page, bag) in own {
        intersections.fill(0);
        touched.clear();
        for &(id, count) in &bag.counts {
            let weight = weights[id as usize];
            for &(other_index, other_count) in &postings[id as usize] {
                let index = other_index as usize;
                if intersections[index] == 0 {
                    touched.push(index);
                }
                intersections[index] += count.min(other_count) * weight;
            }
        }
        if touched.is_empty() {
            candidates.push(CounterpartCandidates {
                page: *page,
                best: 0.0,
                retained: Vec::new(),
                adjacent: Vec::new(),
            });
            continue;
        }
        let similarity = |index: usize| {
            let intersection = intersections[index];
            let union = bag.total + other_totals[index] - intersection;
            if union == 0 {
                0.0
            } else {
                intersection as f64 / union as f64
            }
        };
        let mut best_index = touched[0];
        let mut best = similarity(best_index);
        for &index in &touched[1..] {
            let candidate = similarity(index);
            let ordering = candidate.total_cmp(&best);
            if ordering == Ordering::Greater
                || (ordering == Ordering::Equal && other[index].0 < other[best_index].0)
            {
                best_index = index;
                best = candidate;
            }
        }
        let best_page = other[best_index].0;
        let mut retained = touched
            .iter()
            .map(|&index| (other[index].0, similarity(index)))
            .filter(|(_, candidate)| *candidate >= best * COUNTERPART_RETAIN)
            .collect::<Vec<_>>();
        retained.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
        retained.truncate(COUNTERPART_TOP_K);
        let adjacent = best_page
            .checked_sub(1)
            .into_iter()
            .chain(best_page.checked_add(1))
            .filter(|adjacent| {
                *adjacent != best_page
                    && other
                        .binary_search_by_key(adjacent, |(page, _)| *page)
                        .is_ok()
            })
            .map(|adjacent| {
                let index = other
                    .binary_search_by_key(&adjacent, |(page, _)| *page)
                    .expect("the adjacent page was just found");
                (adjacent, similarity(index))
            })
            .collect();
        candidates.push(CounterpartCandidates {
            page: *page,
            best,
            retained,
            adjacent,
        });
    }
    candidates
}

/// Applies the pair-relative floor to the stored candidates.
fn apply_document_floor(candidates: &[CounterpartCandidates]) -> Counterparts {
    let floor = document_floor(candidates.iter().map(|candidate| candidate.best));
    candidates
        .iter()
        .filter_map(|candidate| {
            // A structural zero means the page shares no information-weighted
            // token with any page of the other document.
            if candidate.best == 0.0 || candidate.best < floor {
                return None;
            }
            let retain = (candidate.best * COUNTERPART_RETAIN).max(floor);
            let mut pages = candidate
                .retained
                .iter()
                .filter(|(_, similarity)| *similarity >= retain)
                .copied()
                .collect::<Vec<_>>();
            for &(adjacent, similarity) in &candidate.adjacent {
                if pages.iter().all(|(page, _)| *page != adjacent) {
                    pages.push((adjacent, similarity));
                }
            }
            (!pages.is_empty()).then_some((candidate.page, pages))
        })
        .collect()
}

/// Derives one side's floor from its median best overlap.
///
/// The median is the pair's own measure of how much two corresponding pages
/// usually share, so the floor follows the documents instead of a fixed score.
fn document_floor(bests: impl Iterator<Item = f64>) -> f64 {
    let mut bests = bests.collect::<Vec<_>>();
    bests.sort_by(f64::total_cmp);
    let median = if bests.is_empty() {
        0.0
    } else if bests.len() % 2 == 1 {
        bests[bests.len() / 2]
    } else {
        f64::midpoint(bests[bests.len() / 2 - 1], bests[bests.len() / 2])
    };
    COUNTERPART_DOCUMENT_RETAIN * median
}

/// Information-weighted Jaccard overlap of two page token multisets.
fn bag_similarity(left: &PageBag, right: &PageBag, weights: &[u64]) -> f64 {
    let mut left_index = 0;
    let mut right_index = 0;
    let mut intersection = 0u64;
    while left_index < left.counts.len() && right_index < right.counts.len() {
        match left.counts[left_index].0.cmp(&right.counts[right_index].0) {
            Ordering::Less => left_index += 1,
            Ordering::Greater => right_index += 1,
            Ordering::Equal => {
                let count = left.counts[left_index].1.min(right.counts[right_index].1);
                intersection += count * weights[left.counts[left_index].0 as usize];
                left_index += 1;
                right_index += 1;
            }
        }
    }
    let union = left.total + right.total - intersection;
    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn order_agreement(layout: &[FlowToken], stream: &[FlowToken]) -> f64 {
    let longest = layout.len().max(stream.len());
    if longest == 0 {
        return 1.0;
    }
    let (layout_ids, stream_ids) = intern_tokens(layout, stream);
    let pairs = align::align(&layout_ids, &stream_ids);
    pairs.len() as f64 / longest as f64
}

fn order_coherence(tokens: &[FlowToken]) -> f64 {
    if tokens.len() < 2 {
        return 1.0;
    }
    let adjacent = tokens
        .windows(2)
        .filter(|window| spatially_adjacent(&window[0], &window[1]))
        .count();
    adjacent as f64 / (tokens.len() - 1) as f64
}

fn spatially_adjacent(left: &FlowToken, right: &FlowToken) -> bool {
    if left.page != right.page {
        return false;
    }
    let (Some(left_bounds), Some(right_bounds)) = (left.bounds, right.bounds) else {
        return false;
    };
    let left_width = (left_bounds.max.x - left_bounds.min.x).abs();
    let left_height = (left_bounds.max.y - left_bounds.min.y).abs();
    let right_width = (right_bounds.max.x - right_bounds.min.x).abs();
    let right_height = (right_bounds.max.y - right_bounds.min.y).abs();
    let width = left_width.max(right_width);
    let height = left_height.max(right_height);
    if width <= f64::EPSILON || height <= f64::EPSILON {
        return false;
    }
    let left_center = center(left_bounds);
    let right_center = center(right_bounds);
    let horizontal_gap = gap(
        (left_bounds.min.x, left_bounds.max.x),
        (right_bounds.min.x, right_bounds.max.x),
    );
    let vertical_gap = gap(
        (left_bounds.min.y, left_bounds.max.y),
        (right_bounds.min.y, right_bounds.max.y),
    );
    let same_row =
        (left_center.y - right_center.y).abs() <= 0.5 * height && horizontal_gap <= 1.5 * height;
    let same_column =
        (left_center.x - right_center.x).abs() <= 0.5 * width && vertical_gap <= 1.5 * width;
    let next_row = right_center.y > left_center.y
        && (right_center.y - left_center.y) <= 2.5 * height
        && (right_bounds.min.x - left_bounds.min.x).abs() <= 2.0 * height;
    let next_column = right_center.x < left_center.x
        && (left_center.x - right_center.x) <= 2.5 * width
        && (right_bounds.min.y - left_bounds.min.y).abs() <= 2.0 * width;
    same_row || same_column || next_row || next_column
}

fn center(rect: Rect) -> Vec2 {
    Vec2 {
        x: f64::midpoint(rect.min.x, rect.max.x),
        y: f64::midpoint(rect.min.y, rect.max.y),
    }
}

fn gap(left: (f64, f64), right: (f64, f64)) -> f64 {
    if left.1 < right.0 {
        right.0 - left.1
    } else if right.1 < left.0 {
        left.0 - right.1
    } else {
        0.0
    }
}

/// Builds one line per content-stream row and one block per line.
fn stream_layout(document: &Document<Glyph>) -> (Vec<Line>, Vec<Block>) {
    let mut glyphs = document.items().iter().collect::<Vec<_>>();
    glyphs.sort_by(|left, right| {
        left.page
            .0
            .cmp(&right.page.0)
            .then(left.render_order.cmp(&right.render_order))
            .then(left.id.0.cmp(&right.id.0))
    });
    let options = LineOptions::default();
    let mut lines = Vec::new();
    let mut blocks = Vec::new();
    let mut current: Option<StreamLine> = None;
    for glyph in glyphs {
        let continues = current
            .as_ref()
            .is_some_and(|line| line.continues(glyph, options));
        if !continues {
            if let Some(line) = current.take() {
                push_stream_line(&mut lines, &mut blocks, line);
            }
            current = Some(StreamLine::new(glyph));
            continue;
        }
        let line = current.as_mut().expect("continuation requires a line");
        line.push(glyph, options);
    }
    if let Some(line) = current {
        push_stream_line(&mut lines, &mut blocks, line);
    }
    (lines, blocks)
}

/// Builds one line per baseline row, ignoring bounding-box height.
///
/// Row membership uses baseline projections and a gap relative to the smaller
/// of the adjacent font sizes, so tall overlapping glyph boxes on tight rows
/// do not merge. Rows run top-to-bottom for horizontal text and
/// right-to-left for vertical text; glyphs run in the direction of their
/// baseline vector.
fn baseline_layout(document: &Document<Glyph>) -> (Vec<Line>, Vec<Block>) {
    let mut glyphs = document.items().iter().collect::<Vec<_>>();
    glyphs.sort_by(|left, right| {
        left.page
            .0
            .cmp(&right.page.0)
            .then(left.render_order.cmp(&right.render_order))
            .then(left.id.0.cmp(&right.id.0))
    });
    let options = LineOptions::default();
    let mut lines = Vec::new();
    let mut blocks = Vec::new();
    let mut page_start = 0usize;
    while page_start < glyphs.len() {
        let page = glyphs[page_start].page;
        let mut page_end = page_start + 1;
        while page_end < glyphs.len() && glyphs[page_end].page == page {
            page_end += 1;
        }
        let page_glyphs = &glyphs[page_start..page_end];
        let reference = normalize(page_glyphs[0].direction);
        let horizontal = reference.x.abs() >= reference.y.abs();
        let key = |glyph: &Glyph| {
            if horizontal {
                glyph.baseline.y
            } else {
                glyph.baseline.x
            }
        };
        let mut sorted = page_glyphs.to_vec();
        sorted.sort_by(|left, right| {
            key(right)
                .total_cmp(&key(left))
                .then(left.id.0.cmp(&right.id.0))
        });
        let mut rows: Vec<Vec<&Glyph>> = Vec::new();
        let mut row_key = f64::NAN;
        let mut row_font = 0.0f64;
        for glyph in sorted {
            let glyph_key = key(glyph);
            let joins = !row_key.is_nan()
                && (row_key - glyph_key).abs()
                    <= 0.5 * row_font.min(glyph.font_size).max(f64::EPSILON);
            if !joins {
                rows.push(Vec::new());
                row_key = glyph_key;
                row_font = glyph.font_size;
            } else {
                row_font = row_font.max(glyph.font_size);
            }
            rows.last_mut()
                .expect("a row exists before pushing a glyph")
                .push(glyph);
        }
        for mut row in rows {
            row.sort_by(|left, right| {
                dot(left.baseline, reference)
                    .total_cmp(&dot(right.baseline, reference))
                    .then(left.id.0.cmp(&right.id.0))
            });
            let mut line = StreamLine::new(row[0]);
            for glyph in row.into_iter().skip(1) {
                line.push(glyph, options);
            }
            push_stream_line(&mut lines, &mut blocks, line);
        }
        page_start = page_end;
    }
    (lines, blocks)
}

fn push_stream_line(lines: &mut Vec<Line>, blocks: &mut Vec<Block>, line: StreamLine) {
    let id = LineId(u64::try_from(lines.len()).expect("line id fits u64"));
    let block_id = BlockId(u64::try_from(lines.len()).expect("block id fits u64"));
    let bbox = line.bbox;
    let baseline = line.baseline;
    let direction = line.direction;
    let page = line.page;
    lines.push(Line {
        id,
        page,
        glyphs: line.glyphs,
        synthetic_spaces: line.synthetic_spaces,
        bbox,
        baseline,
        direction,
        text_direction: LineTextDirection::Unknown,
        render_order: line.render_order,
    });
    blocks.push(Block {
        id: block_id,
        lines: vec![id],
        role: BlockRole::Body,
    });
}

struct StreamLine {
    page: PageId,
    glyphs: Vec<GlyphId>,
    synthetic_spaces: Vec<SyntheticSpace>,
    bbox: Rect,
    baseline: Vec2,
    direction: Vec2,
    render_order: RangeInclusive<u32>,
    last: LastGlyph,
}

/// The previous glyph fields needed to decide whether the next glyph continues
/// the line and where a synthetic space goes.
///
/// Keeping only these values avoids cloning every glyph's raw codes and
/// decoded text once per line hypothesis.
struct LastGlyph {
    id: GlyphId,
    bbox: Rect,
    baseline: Vec2,
    direction: Vec2,
    font_size: f64,
    whitespace: bool,
}

impl LastGlyph {
    fn of(glyph: &Glyph) -> Self {
        Self {
            id: glyph.id,
            bbox: glyph.bbox,
            baseline: glyph.baseline,
            direction: glyph.direction,
            font_size: glyph.font_size,
            whitespace: is_whitespace(glyph),
        }
    }
}

impl StreamLine {
    fn new(glyph: &Glyph) -> Self {
        Self {
            page: glyph.page,
            glyphs: vec![glyph.id],
            synthetic_spaces: Vec::new(),
            bbox: glyph.bbox,
            baseline: glyph.baseline,
            direction: glyph.direction,
            render_order: glyph.render_order..=glyph.render_order,
            last: LastGlyph::of(glyph),
        }
    }

    fn continues(&self, glyph: &Glyph, options: LineOptions) -> bool {
        if self.page != glyph.page {
            return false;
        }
        let last_direction = normalize(self.last.direction);
        let direction = normalize(glyph.direction);
        if dot(last_direction, direction) < options.min_direction_similarity {
            return false;
        }
        let cross_axis = perpendicular(last_direction);
        let baseline_distance =
            (dot(glyph.baseline, cross_axis) - dot(self.last.baseline, cross_axis)).abs();
        let height = extent(self.last.bbox, cross_axis)
            .min(extent(glyph.bbox, cross_axis))
            .max(f64::EPSILON);
        if baseline_distance > options.max_baseline_distance_ratio * height {
            return false;
        }
        let inline_gap = interval_gap(
            projected_interval(self.last.bbox, last_direction),
            projected_interval(glyph.bbox, direction),
        );
        inline_gap
            <= options.max_inline_gap_font_size_ratio * self.last.font_size.max(glyph.font_size)
    }

    fn push(&mut self, glyph: &Glyph, options: LineOptions) {
        let direction = normalize(self.last.direction);
        let inline_gap = interval_gap(
            projected_interval(self.last.bbox, direction),
            projected_interval(glyph.bbox, direction),
        );
        let font_size = self.last.font_size.max(glyph.font_size);
        let advance = extent(glyph.bbox, direction);
        let word_gap = inline_gap
            > (options.space_gap_font_size_ratio * font_size)
                .max(options.space_gap_advance_ratio * advance);
        if word_gap && !self.last.whitespace && !is_whitespace(glyph) {
            self.synthetic_spaces.push(SyntheticSpace {
                preceding: self.last.id,
                following: glyph.id,
            });
        }
        self.glyphs.push(glyph.id);
        self.bbox.min.x = self.bbox.min.x.min(glyph.bbox.min.x);
        self.bbox.min.y = self.bbox.min.y.min(glyph.bbox.min.y);
        self.bbox.max.x = self.bbox.max.x.max(glyph.bbox.max.x);
        self.bbox.max.y = self.bbox.max.y.max(glyph.bbox.max.y);
        self.last = LastGlyph::of(glyph);
        let start = (*self.render_order.start()).min(glyph.render_order);
        let end = (*self.render_order.end()).max(glyph.render_order);
        self.render_order = start..=end;
    }
}

fn is_whitespace(glyph: &Glyph) -> bool {
    match &glyph.text {
        DecodedText::Mapped(text) => text.chars().all(char::is_whitespace),
        DecodedText::Unmapped { .. } => false,
    }
}

fn normalize(vector: Vec2) -> Vec2 {
    let length = (vector.x * vector.x + vector.y * vector.y).sqrt();
    if length <= f64::EPSILON {
        Vec2 { x: 1.0, y: 0.0 }
    } else {
        Vec2 {
            x: vector.x / length,
            y: vector.y / length,
        }
    }
}

fn dot(left: Vec2, right: Vec2) -> f64 {
    left.x * right.x + left.y * right.y
}

fn perpendicular(vector: Vec2) -> Vec2 {
    Vec2 {
        x: -vector.y,
        y: vector.x,
    }
}

fn projected_interval(rect: Rect, axis: Vec2) -> (f64, f64) {
    let corners = [
        dot(rect.min, axis),
        dot(
            Vec2 {
                x: rect.min.x,
                y: rect.max.y,
            },
            axis,
        ),
        dot(
            Vec2 {
                x: rect.max.x,
                y: rect.min.y,
            },
            axis,
        ),
        dot(rect.max, axis),
    ];
    let min = corners.iter().copied().fold(f64::INFINITY, f64::min);
    let max = corners.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (min, max)
}

fn extent(rect: Rect, axis: Vec2) -> f64 {
    let (min, max) = projected_interval(rect, axis);
    (max - min).abs()
}

fn interval_gap(left: (f64, f64), right: (f64, f64)) -> f64 {
    if left.1 < right.0 {
        right.0 - left.1
    } else if right.1 < left.0 {
        left.0 - right.1
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::{
        model::{
            DecodedText, FontId, GlyphCropStatus, GlyphId, GlyphPathClipStatus, GlyphProvenance,
            TextRenderMode,
        },
        pdf::ObjectRef,
    };

    fn glyph(text: &str, id: u64, x: f64, baseline_y: f64, render_order: u32) -> Glyph {
        Glyph {
            id: GlyphId(id),
            raw_code: text.as_bytes().to_vec(),
            text: DecodedText::Mapped(text.to_owned()),
            page: PageId(0),
            bbox: Rect {
                min: Vec2 {
                    x,
                    y: baseline_y - 20.0,
                },
                max: Vec2 {
                    x: x + 10.0,
                    y: baseline_y + 20.0,
                },
            },
            baseline: Vec2 { x, y: baseline_y },
            direction: Vec2 { x: 1.0, y: 0.0 },
            font_id: FontId(0),
            font_size: 9.0,
            render_order,
            render_mode: TextRenderMode::Fill,
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 1,
                    generation: 0,
                },
                operator_index: render_order,
            },
        }
    }

    #[test]
    fn baseline_rows_ignore_tall_overlapping_boxes() {
        // Row 1: A B at baseline 100; row 2: C D at baseline 89. Each box is
        // 40 units tall, so adjacent rows overlap vertically. Paint order is
        // interleaved (A, C, B, D), which previously merged rows through
        // bounding-box overlap.
        let document = Document::new(vec![
            glyph("A", 1, 0.0, 100.0, 0),
            glyph("C", 2, 0.0, 89.0, 1),
            glyph("B", 3, 10.0, 100.0, 2),
            glyph("D", 4, 10.0, 89.0, 3),
        ]);
        let (lines, _) = baseline_layout(&document);
        let text = lines
            .iter()
            .map(|line| {
                line.glyphs
                    .iter()
                    .map(|id| {
                        document
                            .items()
                            .iter()
                            .find(|glyph| glyph.id == *id)
                            .and_then(|glyph| match &glyph.text {
                                DecodedText::Mapped(text) => Some(text.as_str()),
                                DecodedText::Unmapped { .. } => None,
                            })
                            .expect("test glyph text")
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(text, ["AB", "CD"]);
    }

    fn token(page: u32) -> FlowToken {
        FlowToken {
            text: "t".to_owned(),
            page: PageId(page),
            bounds: None,
            block: 0,
            uncertain: false,
            line_break_after: false,
            font_size: None,
            raw: None,
            position: None,
            page_break_after: false,
            normalization: 0,
        }
    }

    fn row_token(y: f64) -> FlowToken {
        FlowToken {
            text: "t".to_owned(),
            page: PageId(0),
            bounds: Some(Rect {
                min: Vec2 { x: 0.0, y },
                max: Vec2 {
                    x: 10.0,
                    y: y + 10.0,
                },
            }),
            block: 0,
            uncertain: false,
            line_break_after: true,
            font_size: None,
            raw: None,
            position: None,
            page_break_after: false,
            normalization: 0,
        }
    }

    #[test]
    fn candidate_cache_round_trips_exactly() {
        let mut candidates = candidates_of(&[&["alpha", "beta"], &["gamma"]]);
        for stream in [
            &mut candidates.layout,
            &mut candidates.stream,
            &mut candidates.baseline,
        ] {
            let token = &mut stream.tokens[0];
            token.raw = Some("alpha ".into());
            token.position = Some(
                PositionSignature::new(Vec2 { x: 1.0, y: 2.0 }, Vec2 { x: 1.0, y: 0.0 })
                    .expect("position"),
            );
            token.page_break_after = true;
            token.normalization = 4;
        }
        let paint_inventory = vec![PaintIdentity {
            page: PageId(2),
            kind: NonTextPaintKind::Path,
            identity: Some("identity".to_owned()),
            shape: Some("shape".to_owned()),
            bounds: Some(Rect {
                min: Vec2 { x: 1.0, y: 2.0 },
                max: Vec2 { x: 3.0, y: 4.0 },
            }),
        }];
        let cached = FlowCandidates {
            candidates: Candidates {
                layout: candidates.layout.clone(),
                stream: candidates.stream.clone(),
                baseline: candidates.baseline.clone(),
            },
            paint_inventory: Some(paint_inventory.clone()),
        };
        let mut bytes = Vec::new();
        cached
            .write_cache(&mut bytes)
            .expect("cache write should succeed");
        let loaded =
            FlowCandidates::read_cache(bytes.as_slice()).expect("cache read should succeed");
        assert_eq!(loaded.candidates.layout, candidates.layout);
        assert_eq!(loaded.candidates.stream, candidates.stream);
        assert_eq!(loaded.candidates.baseline, candidates.baseline);
        assert_eq!(loaded.paint_inventory, Some(paint_inventory));
        assert!(FlowCandidates::read_cache(&b"notacache"[..]).is_err());
        let truncated = &bytes[..bytes.len() - 4];
        assert!(FlowCandidates::read_cache(truncated).is_err());
        let mut corrupted = bytes.clone();
        *corrupted.last_mut().expect("cache bytes are non-empty") ^= 0xff;
        assert!(FlowCandidates::read_cache(corrupted.as_slice()).is_err());
        let mut wrong_version = bytes.clone();
        wrong_version[8] ^= 0xff;
        assert!(FlowCandidates::read_cache(wrong_version.as_slice()).is_err());
        let mut oversized = CACHE_MAGIC.to_vec();
        oversized.extend_from_slice(&CACHE_VERSION.to_le_bytes());
        oversized.extend_from_slice(&(u64::MAX / 2).to_le_bytes());
        assert!(FlowCandidates::read_cache(oversized.as_slice()).is_err());
    }

    #[test]
    fn stacked_bands_need_a_gap_much_larger_than_the_line_pitch() {
        // Two four-row blocks with a 154-unit gap between the last row of the
        // lower block and the first row of the upper block (line pitch 12).
        let mut tokens = (0..4)
            .map(|row| row_token(f64::from(row) * 12.0))
            .collect::<Vec<_>>();
        tokens.extend((0..4).map(|row| row_token(200.0 + f64::from(row) * 12.0)));
        let bands = stacked_bands(&tokens);
        assert_eq!(bands.len(), 2);
        assert!(bands[0].bottom > 46.0 && bands[0].bottom < 200.0);
        assert!(bands[0].top.is_infinite());
        assert!(bands[1].bottom.is_infinite());
        assert_eq!(bands[1].top, bands[0].bottom);

        // The same eight rows on one pitch stay one band.
        let continuous = (0..8)
            .map(|row| row_token(f64::from(row) * 12.0))
            .collect::<Vec<_>>();
        assert!(stacked_bands(&continuous).is_empty());
    }

    #[test]
    fn page_ranges_keep_every_page_exactly_once() {
        let stream = FlowStream {
            tokens: vec![token(0), token(0), token(2), token(2), token(3)],
            ..FlowStream::default()
        };
        let ranges = page_ranges(&stream);
        assert_eq!(ranges.get(&0), Some(&(0..2)));
        assert_eq!(ranges.get(&2), Some(&(2..4)));
        assert_eq!(ranges.get(&3), Some(&(4..5)));
        assert_eq!(ranges.len(), 3);
    }

    /// Builds one candidate set whose three hypotheses all carry the same token
    /// order. Counterpart matching and evidence scoring are order-free in their
    /// inputs, so identical hypotheses make the expected values explicit.
    fn candidates_of(pages: &[&[&str]]) -> Candidates {
        let mut tokens = Vec::new();
        for (page, texts) in pages.iter().enumerate() {
            for text in *texts {
                tokens.push(FlowToken {
                    text: (*text).to_owned(),
                    page: PageId(u32::try_from(page).expect("test page fits u32")),
                    bounds: None,
                    block: 0,
                    uncertain: false,
                    line_break_after: false,
                    font_size: None,
                    raw: None,
                    position: None,
                    page_break_after: false,
                    normalization: 0,
                });
            }
        }
        let stream = FlowStream {
            tokens,
            ..FlowStream::default()
        };
        Candidates {
            layout: stream.clone(),
            stream: stream.clone(),
            baseline: stream,
        }
    }

    fn counterpart_index<'a>(old: &'a Candidates, new: &'a Candidates) -> CounterpartIndex<'a> {
        CounterpartIndex::of(old, new, &SidePages::of(old), &SidePages::of(new))
    }

    fn page_bag(counts: &[(u32, u64)], weights: &[u64]) -> PageBag {
        let total = counts
            .iter()
            .map(|&(id, count)| count * weights[id as usize])
            .sum();
        PageBag {
            counts: counts.to_vec(),
            total,
        }
    }

    #[test]
    fn sparse_page_candidates_match_brute_force_similarity() {
        // Token widths and page counts make both the best match and the
        // retained list non-trivial; page 20 also exercises the adjacent-page
        // evidence for a page that shares no token with its neighbour.
        let weights = [1u64, 2, 3, 4, 5, 6];
        let own = vec![
            (10, page_bag(&[(0, 3), (1, 1), (3, 2)], &weights)),
            (20, page_bag(&[(2, 1), (4, 2)], &weights)),
            (30, page_bag(&[(5, 1)], &weights)),
        ];
        let other = vec![
            (11, page_bag(&[(0, 1), (1, 1), (2, 1)], &weights)),
            (12, page_bag(&[(0, 2), (3, 1)], &weights)),
            (13, page_bag(&[(4, 1), (5, 1)], &weights)),
            (14, page_bag(&[(0, 1), (1, 1)], &weights)),
        ];
        let sparse = page_candidates(&own, &other, &weights);
        for (index, (page, bag)) in own.iter().enumerate() {
            let mut scored = other
                .iter()
                .map(|(other_page, other_bag)| {
                    (*other_page, bag_similarity(bag, other_bag, &weights))
                })
                .collect::<Vec<_>>();
            scored.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
            let (best_page, best) = scored[0];
            let retained = scored
                .iter()
                .filter(|(_, similarity)| *similarity >= best * COUNTERPART_RETAIN)
                .take(COUNTERPART_TOP_K)
                .copied()
                .collect::<Vec<_>>();
            let similarity_by_page = scored.into_iter().collect::<HashMap<_, _>>();
            let adjacent = best_page
                .checked_sub(1)
                .into_iter()
                .chain(best_page.checked_add(1))
                .filter(|adjacent| {
                    *adjacent != best_page
                        && other
                            .binary_search_by_key(adjacent, |(page, _)| *page)
                            .is_ok()
                })
                .map(|adjacent| (adjacent, similarity_by_page[&adjacent]))
                .collect::<Vec<_>>();
            assert_eq!(sparse[index].page, *page);
            assert_eq!(sparse[index].best, best);
            assert_eq!(sparse[index].retained, retained);
            assert_eq!(sparse[index].adjacent, adjacent);
        }
    }

    #[test]
    fn counterpart_pages_skip_unrelated_pages() {
        let old = candidates_of(&[&["alpha", "beta"], &["gamma", "delta"]]);
        let new = candidates_of(&[&["alpha", "beta"], &["gamma", "delta"], &["omega"]]);
        let index = counterpart_index(&old, &new);
        let pages = index.for_old().get(&0).expect("page 0 has a counterpart");
        assert_eq!(pages[0].0, 0);
        assert_eq!(pages[0].1, 1.0);
        assert!(
            pages.iter().all(|(page, _)| *page != 2),
            "an unrelated page must not contribute evidence"
        );
    }

    #[test]
    fn a_page_without_a_counterpart_has_no_evidence() {
        let old = candidates_of(&[&["alpha", "beta"], &["gamma", "delta", "epsilon"]]);
        let new = candidates_of(&[&["gamma", "delta", "epsilon"]]);
        let index = counterpart_index(&old, &new);
        assert!(
            index.for_old().get(&0).is_none(),
            "a deleted page has no counterpart above the similarity floor"
        );
        assert_eq!(index.for_old().get(&1).expect("surviving page")[0].0, 0);
    }

    #[test]
    fn a_duplicated_page_is_judged_against_the_original() {
        let old = candidates_of(&[&["alpha", "beta"], &["gamma"]]);
        let new = candidates_of(&[&["alpha", "beta"], &["alpha", "beta"], &["gamma"]]);
        let index = counterpart_index(&old, &new);
        for page in [0, 1] {
            let pages = index
                .for_new()
                .get(&page)
                .expect("the duplicate has a counterpart");
            assert_eq!(pages[0].0, 0, "duplicate page {page} must match old page 0");
            assert_eq!(pages[0].1, 1.0);
        }
    }

    #[test]
    fn local_evidence_does_not_depend_on_unrelated_pages() {
        let old = candidates_of(&[&["alpha", "beta", "gamma"], &["delta", "epsilon", "zeta"]]);
        let before = candidates_of(&[&["alpha", "beta", "gamma"], &["delta", "epsilon", "zeta"]]);
        let after = candidates_of(&[
            &["alpha", "beta", "gamma"],
            &["delta", "epsilon", "zeta"],
            &["eta", "theta", "iota"],
        ]);
        let scores = |new: &Candidates| {
            let new_pages = SidePages::of(new);
            let view = EvidenceView::of(new, &new_pages);
            let index = counterpart_index(&old, new);
            let counterparts = index.for_old().get(&0).expect("counterpart").clone();
            let evidence = view.evidence(&counterparts);
            let old_pages = SidePages::of(&old);
            let range = old_pages.layout.get(&0).expect("page range").clone();
            (
                evidence.score(&old.layout.tokens[range]),
                counterparts
                    .iter()
                    .map(|(page, _)| *page)
                    .collect::<Vec<_>>(),
            )
        };
        let before_scores = scores(&before);
        assert!(before_scores.0 > 0);
        assert_eq!(before_scores, scores(&after));
    }

    #[test]
    fn intrinsic_order_prefers_layout_then_paint_order() {
        assert_eq!(intrinsic_order(0.9, 0.9, 0.9), OrderKind::Layout);
        assert_eq!(intrinsic_order(0.9, 0.95, 0.9), OrderKind::Stream);
        assert_eq!(intrinsic_order(0.9, 0.9, 0.95), OrderKind::Baseline);
    }
}
