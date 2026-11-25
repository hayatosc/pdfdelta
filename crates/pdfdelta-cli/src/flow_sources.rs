//! Sidecar cache for retained per-glyph flow sources.
//!
//! The compact candidate cache carries tokens but no source glyphs. A full
//! report needs each token's original glyph records, which on a multi-million
//! glyph document would otherwise keep both extracted documents resident while
//! the second side is tokenized. This cache lets the child that built one side
//! serialize its glyph records once, and lets the reporting parent stream them
//! back by glyph id with a bounded buffer.
//!
//! The format is private and versioned. Glyph ids are dense positions in the
//! extracted document, so the fragment offset table is indexed by id.

use std::{
    cell::RefCell,
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Arc,
};

use pdfdelta_core::model::{Glyph, GlyphId};

use crate::flow_cmd::CacheDirectory;

const MAGIC: &[u8] = b"PDFDELTA-FLOW-SOURCES";
const VERSION: u32 = 1;
const HEADER_FLAG_GLYPHS: u8 = 1;
const GLYPH_METADATA_BYTES: u64 = 9;
/// Offset of the metadata-offset field: magic, version, flags, glyph count.
const HEADER_METADATA_OFFSET: u64 = 21 + 4 + 1 + 8;
/// Read window for the sidecar's per-glyph fragments.
///
/// Full reports request source records in token order, so consecutive glyph
/// ids are normally adjacent. Reading each tiny fragment with its own seek and
/// read made a multi-million-glyph report issue millions of syscalls. A bounded
/// window keeps the access pattern sequential without retaining the glyph
/// table: the window is the only extra resident state and is capped here.
const READ_WINDOW_BYTES: usize = 4 * 1024 * 1024;

struct SourceReader {
    file: File,
    window: Vec<u8>,
    start: u64,
}

impl SourceReader {
    fn new(file: File) -> Self {
        Self {
            file,
            window: Vec::new(),
            start: 0,
        }
    }

    /// Returns the requested bytes from the window, refilling it on a miss.
    fn fragment(&mut self, offset: u64, length: usize) -> std::io::Result<&[u8]> {
        let end = offset
            .checked_add(u64::try_from(length).unwrap_or(u64::MAX))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "cached source glyph record offset overflows",
                )
            })?;
        let start = self.start;
        let window_end = start.saturating_add(u64::try_from(self.window.len()).unwrap_or(u64::MAX));
        if offset < start || end > window_end {
            let read_length = READ_WINDOW_BYTES.max(length);
            self.window.resize(read_length, 0);
            self.file.seek(SeekFrom::Start(offset))?;
            let mut filled = 0usize;
            while filled < read_length {
                match self.file.read(&mut self.window[filled..])? {
                    0 => break,
                    read => filled += read,
                }
            }
            self.window.truncate(filled);
            self.start = offset;
            if filled < length {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "cached source glyph record is truncated",
                ));
            }
        }
        let relative = usize::try_from(offset - self.start).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cached source glyph record offset does not fit in memory",
            )
        })?;
        Ok(&self.window[relative..relative + length])
    }
}

/// Retained per-glyph source records for one flow side.
pub(crate) struct SourceCache {
    reader: RefCell<SourceReader>,
    offsets: Vec<u64>,
    lengths: Vec<u32>,
    pages: Vec<u32>,
    visible: Vec<bool>,
    _directory: Arc<CacheDirectory>,
}

/// Opened cache plus the three hypotheses' source glyph ids.
pub(crate) type OpenedSourceCache = (SourceCache, [Vec<Vec<GlyphId>>; 3]);

impl SourceCache {
    /// Number of extracted glyphs represented by this cache.
    pub(crate) fn glyph_count(&self) -> usize {
        self.pages.len()
    }

    /// One-based page number of a glyph id.
    pub(crate) fn page(&self, id: GlyphId) -> Option<u32> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.pages.get(index).copied())
    }

    /// Whether a glyph id was comparison-visible at extraction time.
    pub(crate) fn is_comparison_visible(&self, id: GlyphId) -> Option<bool> {
        usize::try_from(id.0)
            .ok()
            .and_then(|index| self.visible.get(index).copied())
    }

    /// Copies one serialized `SourceGlyph` object to `out`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the fragment cannot be read or written, or a
    /// data error when the id is outside the cached population.
    pub(crate) fn write_fragment(&self, id: GlyphId, out: &mut dyn Write) -> std::io::Result<()> {
        let index = usize::try_from(id.0)
            .ok()
            .filter(|index| *index < self.offsets.len())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "cached source glyph id is outside the extracted population",
                )
            })?;
        let offset = self.offsets[index];
        let length = usize::try_from(self.lengths[index]).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cached source glyph record length does not fit in memory",
            )
        })?;
        let mut reader = self.reader.borrow_mut();
        out.write_all(reader.fragment(offset, length)?)
    }

    /// Opens a source cache and reads its three hypotheses' source lists.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing, truncated, or invalid cache or a
    /// fragment table that does not match the declared glyph count.
    pub(crate) fn open(
        path: &Path,
        directory: Arc<CacheDirectory>,
    ) -> Result<OpenedSourceCache, String> {
        let file =
            File::open(path).map_err(|error| format!("cannot open flow source cache: {error}"))?;
        // Metadata and source lists are long runs of small integers. Reading
        // them one field at a time from the unbuffered file made a full report
        // issue tens of millions of read syscalls before writing any output.
        let mut file = BufReader::with_capacity(1024 * 1024, file);
        let mut magic = vec![0u8; MAGIC.len()];
        file.read_exact(&mut magic)
            .map_err(|error| format!("cannot read flow source cache: {error}"))?;
        if magic != MAGIC {
            return Err("flow source cache has a wrong magic".into());
        }
        let mut version = [0u8; 4];
        file.read_exact(&mut version)
            .map_err(|error| format!("cannot read flow source cache: {error}"))?;
        if u32::from_le_bytes(version) != VERSION {
            return Err("flow source cache has a wrong version".into());
        }
        let mut flags = [0u8; 1];
        file.read_exact(&mut flags)
            .map_err(|error| format!("cannot read flow source cache: {error}"))?;
        let has_glyphs = flags[0] & HEADER_FLAG_GLYPHS != 0;
        let glyph_count = read_u64(&mut file)?;
        let glyph_count = usize::try_from(glyph_count)
            .map_err(|_| "flow source cache glyph count does not fit in memory".to_owned())?;
        let metadata_offset = read_u64(&mut file)?;
        let fragments_start = file
            .stream_position()
            .map_err(|error| format!("cannot inspect flow source cache: {error}"))?;
        let mut pages = Vec::new();
        let mut visible = Vec::new();
        let mut lengths = Vec::new();
        let mut offsets = Vec::new();
        let lists_start = if has_glyphs {
            let metadata_bytes = GLYPH_METADATA_BYTES
                .checked_mul(u64::try_from(glyph_count).unwrap_or(u64::MAX))
                .ok_or_else(|| "flow source cache metadata size overflowed".to_owned())?;
            let lists_start = metadata_offset
                .checked_add(metadata_bytes)
                .ok_or_else(|| "flow source cache offset overflowed".to_owned())?;
            file.seek(SeekFrom::Start(metadata_offset))
                .map_err(|error| format!("cannot seek in flow source cache: {error}"))?;
            pages = Vec::with_capacity(glyph_count);
            visible = Vec::with_capacity(glyph_count);
            lengths = Vec::with_capacity(glyph_count);
            for _ in 0..glyph_count {
                let page = read_u32(&mut file)?;
                let mut flag = [0u8; 1];
                file.read_exact(&mut flag)
                    .map_err(|error| format!("cannot read flow source cache: {error}"))?;
                let length = read_u32(&mut file)?;
                pages.push(page);
                visible.push(flag[0] != 0);
                lengths.push(length);
            }
            offsets = Vec::with_capacity(glyph_count);
            let mut offset = fragments_start;
            for length in &lengths {
                offsets.push(offset);
                offset = offset
                    .checked_add(u64::from(*length))
                    .ok_or_else(|| "flow source cache fragment offsets overflowed".to_owned())?;
            }
            if offset != metadata_offset {
                return Err("flow source cache fragment table does not match its metadata".into());
            }
            lists_start
        } else {
            if glyph_count != 0 {
                return Err("flow source cache declares glyphs without a glyph table".into());
            }
            fragments_start
        };
        file.seek(SeekFrom::Start(lists_start))
            .map_err(|error| format!("cannot seek in flow source cache: {error}"))?;
        let mut lists: [Vec<Vec<GlyphId>>; 3] = std::array::from_fn(|_| Vec::new());
        for list in &mut lists {
            let token_count = read_u64(&mut file)?;
            let token_count = usize::try_from(token_count)
                .map_err(|_| "flow source cache token count does not fit in memory".to_owned())?;
            list.reserve(token_count);
            for _ in 0..token_count {
                let count = read_u32(&mut file)?;
                let count = usize::try_from(count)
                    .map_err(|_| "flow source cache source count is too large".to_owned())?;
                let mut sources = Vec::with_capacity(count);
                for _ in 0..count {
                    sources.push(GlyphId(read_u64(&mut file)?));
                }
                list.push(sources);
            }
        }
        Ok((
            Self {
                reader: RefCell::new(SourceReader::new(file.into_inner())),
                offsets,
                lengths,
                pages,
                visible,
                _directory: directory,
            },
            lists,
        ))
    }
}

/// Incremental sidecar writer.
///
/// Glyph fragments are written first, while only the extracted document is
/// resident; the token source lists are appended after the side's reading
/// order hypotheses are built. This keeps the child's peak closer to the
/// compact candidate build instead of holding the document and a second full
/// copy of its glyphs at once.
pub(crate) struct SourceWriter {
    writer: BufWriter<File>,
}

impl SourceWriter {
    /// Creates the cache and writes its glyph section, if requested.
    ///
    /// # Errors
    ///
    /// Returns an error when a glyph id is not its dense position, a record
    /// cannot be serialized, or the file cannot be written.
    pub(crate) fn create(path: &Path, glyphs: Option<&[Glyph]>) -> Result<Self, String> {
        let file = File::create(path)
            .map_err(|error| format!("cannot create flow source cache: {error}"))?;
        let mut writer = BufWriter::with_capacity(1024 * 1024, file);
        write_header(&mut writer, glyphs)?;
        if let Some(glyphs) = glyphs {
            let mut metadata = Vec::with_capacity(glyphs.len() * 9);
            for (index, glyph) in glyphs.iter().enumerate() {
                if glyph.id.0 != u64::try_from(index).unwrap_or(u64::MAX) {
                    return Err("flow source cache glyph ids are not dense positions".into());
                }
                let length = crate::flow_full::write_source_fragment(glyph, &mut writer)?;
                metadata.extend_from_slice(&(glyph.page.0 + 1).to_le_bytes());
                metadata.push(u8::from(glyph.is_comparison_visible()));
                metadata.extend_from_slice(
                    &u32::try_from(length)
                        .map_err(|_| "flow source glyph record does not fit in u32".to_owned())?
                        .to_le_bytes(),
                );
            }
            patch_metadata_offset(&mut writer)?;
            writer
                .write_all(&metadata)
                .map_err(|error| format!("cannot write flow source cache: {error}"))?;
        }
        Ok(Self { writer })
    }

    /// Appends the three reading-order hypotheses' source glyph ids.
    ///
    /// # Errors
    ///
    /// Returns an error when a source list is misaligned or the file cannot be
    /// written.
    pub(crate) fn write_lists(
        &mut self,
        candidates: &pdfdelta_flow::FlowCandidates,
    ) -> Result<(), String> {
        for hypothesis in candidates.hypotheses() {
            if !hypothesis.source_glyphs.is_empty()
                && hypothesis.source_glyphs.len() != hypothesis.tokens.len()
            {
                return Err(format!(
                    "flow source cache source lists are not aligned: {} lists, {} tokens",
                    hypothesis.source_glyphs.len(),
                    hypothesis.tokens.len()
                ));
            }
            let token_count = u64::try_from(hypothesis.tokens.len())
                .map_err(|_| "flow source cache token count does not fit in u64".to_owned())?;
            self.writer
                .write_all(&token_count.to_le_bytes())
                .map_err(|error| format!("cannot write flow source cache: {error}"))?;
            for index in 0..hypothesis.tokens.len() {
                let sources = hypothesis
                    .source_glyphs
                    .get(index)
                    .map_or(&[][..], Vec::as_slice);
                let count = u32::try_from(sources.len())
                    .map_err(|_| "flow source cache source count does not fit in u32".to_owned())?;
                self.writer
                    .write_all(&count.to_le_bytes())
                    .map_err(|error| format!("cannot write flow source cache: {error}"))?;
                for source in sources {
                    self.writer
                        .write_all(&source.0.to_le_bytes())
                        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
                }
            }
        }
        Ok(())
    }

    /// Flushes the completed cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be flushed.
    pub(crate) fn finish(self) -> Result<(), String> {
        let mut writer = self.writer;
        writer
            .flush()
            .map_err(|error| format!("cannot flush flow source cache: {error}"))
    }
}

fn write_header(writer: &mut BufWriter<File>, glyphs: Option<&[Glyph]>) -> Result<(), String> {
    writer
        .write_all(MAGIC)
        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
    writer
        .write_all(&VERSION.to_le_bytes())
        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
    writer
        .write_all(&[if glyphs.is_some() {
            HEADER_FLAG_GLYPHS
        } else {
            0
        }])
        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
    let glyph_count = glyphs.map_or(0u64, |glyphs| {
        u64::try_from(glyphs.len()).unwrap_or(u64::MAX)
    });
    writer
        .write_all(&glyph_count.to_le_bytes())
        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
    writer
        .write_all(&0u64.to_le_bytes())
        .map_err(|error| format!("cannot write flow source cache: {error}"))
}

fn patch_metadata_offset(writer: &mut BufWriter<File>) -> Result<(), String> {
    let position = writer
        .stream_position()
        .map_err(|error| format!("cannot inspect flow source cache: {error}"))?;
    writer
        .seek(SeekFrom::Start(HEADER_METADATA_OFFSET))
        .map_err(|error| format!("cannot seek in flow source cache: {error}"))?;
    writer
        .write_all(&position.to_le_bytes())
        .map_err(|error| format!("cannot write flow source cache: {error}"))?;
    writer
        .seek(SeekFrom::End(0))
        .map_err(|error| format!("cannot seek in flow source cache: {error}"))?;
    let actual = writer
        .stream_position()
        .map_err(|error| format!("cannot inspect flow source cache: {error}"))?;
    if actual != position {
        return Err("flow source cache metadata offset was not restored".into());
    }
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("cannot read flow source cache: {error}"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| format!("cannot read flow source cache: {error}"))?;
    Ok(u64::from_le_bytes(bytes))
}
