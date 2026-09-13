use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    Error, Result,
    model::{Document, FontProgramHash, Glyph, PageId},
    pdf::{ParseLimits, ParsedPdf, PdfIssue, PdfIssueKind, PdfIssueLocation, PdfParser},
};

mod content_stream;

pub use content_stream::{ContentStreamGlyphExtractor, PageCoordinateFrame};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_glyphs: usize,
    pub max_form_depth: usize,
    pub max_nesting_depth: usize,
    pub max_operators: usize,
    pub max_stream_invocations: usize,
    pub max_total_decoded_bytes: usize,
    pub max_operand_stack: usize,
    /// Maximum elements in one PDF array operand. Defaults to 65,536.
    pub max_array_elements: usize,
    pub max_operand_nodes: usize,
    pub max_fonts: usize,
    pub max_cmap_entries: usize,
    pub max_cid_width_entries: usize,
    pub max_string_bytes: usize,
    pub max_vector_lines: usize,
}

impl Default for ExtractionLimits {
    fn default() -> Self {
        Self {
            max_glyphs: 5_000_000,
            max_form_depth: 32,
            max_nesting_depth: 64,
            max_operators: 5_000_000,
            max_stream_invocations: 5_000_000,
            max_total_decoded_bytes: 512 * 1024 * 1024,
            max_operand_stack: 4_096,
            max_array_elements: 65_536,
            // The measured multilingual corpus peaks at 8,732,907 operand nodes.
            max_operand_nodes: 10_000_000,
            max_fonts: 100_000,
            max_cmap_entries: 1_000_000,
            // One complete u16 CID space globally. Multiplying this by max_fonts would
            // permit billions of entries by default; callers can raise it deliberately.
            max_cid_width_entries: usize::from(u16::MAX) + 1,
            max_string_bytes: 64 * 1024 * 1024,
            max_vector_lines: 5_000_000,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalFontIdentities {
    identities: BTreeMap<Vec<u8>, FontProgramHash>,
}

impl ExternalFontIdentities {
    pub const MAX_ENTRIES: usize = 1_024;
    pub const MAX_BASE_FONT_BYTES: usize = 127;
    pub const MAX_IDENTITY_BYTES: usize = 4_096;

    pub fn insert(&mut self, base_font: &[u8], identity: &[u8]) -> Result<()> {
        if base_font.is_empty() || base_font.len() > Self::MAX_BASE_FONT_BYTES {
            return Err(Error::InvalidConfiguration(format!(
                "external BaseFont names must contain 1..={} bytes",
                Self::MAX_BASE_FONT_BYTES
            )));
        }
        if identity.is_empty() || identity.len() > Self::MAX_IDENTITY_BYTES {
            return Err(Error::InvalidConfiguration(format!(
                "external font identities must contain 1..={} bytes",
                Self::MAX_IDENTITY_BYTES
            )));
        }
        if self.identities.contains_key(base_font) {
            return Err(Error::InvalidConfiguration(format!(
                "duplicate external font identity for /{}",
                String::from_utf8_lossy(base_font)
            )));
        }
        if self.identities.len() == Self::MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                resource: "external font identity entries",
                limit: Self::MAX_ENTRIES,
            });
        }
        let mut digest = Sha256::new();
        digest.update(b"pdfdelta-external-font-identity\0v1\0");
        digest.update((identity.len() as u64).to_be_bytes());
        digest.update(identity);
        self.identities.insert(
            base_font.to_vec(),
            FontProgramHash(digest.finalize().to_vec()),
        );
        Ok(())
    }

    /// Inserts a precomputed identity hash for callers that only transport the
    /// digest, such as a bounded child-process request.
    ///
    /// Enforces the same `BaseFont` name and entry-count bounds as
    /// [`Self::insert`], and requires a non-empty SHA-256-sized digest.
    ///
    /// # Errors
    /// Rejects empty or oversized `BaseFont` names, empty or non-32-byte
    /// digests, duplicate names, and entries beyond [`Self::MAX_ENTRIES`].
    pub fn insert_hash(&mut self, base_font: &[u8], identity: FontProgramHash) -> Result<()> {
        if base_font.is_empty() || base_font.len() > Self::MAX_BASE_FONT_BYTES {
            return Err(Error::InvalidConfiguration(format!(
                "external BaseFont names must contain 1..={} bytes",
                Self::MAX_BASE_FONT_BYTES
            )));
        }
        if identity.0.len() != 32 {
            return Err(Error::InvalidConfiguration(
                "precomputed external font identities must be 32-byte SHA-256 digests".to_owned(),
            ));
        }
        if self.identities.contains_key(base_font) {
            return Err(Error::InvalidConfiguration(format!(
                "duplicate external font identity for /{}",
                String::from_utf8_lossy(base_font)
            )));
        }
        if self.identities.len() == Self::MAX_ENTRIES {
            return Err(Error::LimitExceeded {
                resource: "external font identity entries",
                limit: Self::MAX_ENTRIES,
            });
        }
        self.identities.insert(base_font.to_vec(), identity);
        Ok(())
    }

    pub(crate) fn get(&self, base_font: &[u8]) -> Option<&FontProgramHash> {
        self.identities.get(base_font)
    }

    /// Iterates the asserted identities in deterministic `BaseFont` order for
    /// cache-key hashing and diagnostics.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &FontProgramHash)> {
        self.identities
            .iter()
            .map(|(base_font, identity)| (base_font.as_slice(), identity))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExtractionIssueKind {
    Unsupported,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum ExtractionScope {
    Document,
    Page(PageId),
    /// A skipped Page Tree branch after this many recovered pages on this
    /// document side. This is ordering evidence, not a cross-revision page
    /// identity.
    PageGap {
        retained_before: usize,
    },
    /// An extraction gap after this many retained glyphs on this document
    /// side. The boundary is a document-local count in extraction order, not
    /// a glyph identifier or a cross-revision identity.
    GlyphGap {
        retained_before: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExtractionIssue {
    kind: ExtractionIssueKind,
    scope: ExtractionScope,
    description: String,
}

impl ExtractionIssue {
    pub fn new(
        kind: ExtractionIssueKind,
        scope: ExtractionScope,
        description: impl Into<String>,
    ) -> Result<Self> {
        let description = description.into();
        if description.trim().is_empty() {
            return Err(Error::InvalidConfiguration(
                "extraction issues require a description".to_owned(),
            ));
        }
        Ok(Self {
            kind,
            scope,
            description,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> ExtractionIssueKind {
        self.kind
    }

    #[must_use]
    pub const fn scope(&self) -> ExtractionScope {
        self.scope
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn into_parts(self) -> (ExtractionIssueKind, ExtractionScope, String) {
        (self.kind, self.scope, self.description)
    }

    fn from_error(scope: ExtractionScope, error: Error) -> Result<Self> {
        match error {
            Error::Unsupported(description) => Self::new(
                ExtractionIssueKind::Unsupported,
                scope,
                nonblank_description(description, "unsupported extraction feature"),
            ),
            Error::Unresolved(description) => Self::new(
                ExtractionIssueKind::Unresolved,
                scope,
                nonblank_description(description, "unresolved extraction content"),
            ),
            error => Err(error),
        }
    }

    fn from_pdf_issue(issue: &PdfIssue) -> Result<Self> {
        Self::new(
            match issue.kind() {
                PdfIssueKind::Unresolved => ExtractionIssueKind::Unresolved,
                PdfIssueKind::Unsupported => ExtractionIssueKind::Unsupported,
            },
            match issue.location() {
                PdfIssueLocation::Document => ExtractionScope::Document,
                PdfIssueLocation::PageTreeGap { retained_before } => {
                    ExtractionScope::PageGap { retained_before }
                }
            },
            issue.description(),
        )
    }

    fn into_error(self) -> Error {
        match self.kind {
            ExtractionIssueKind::Unsupported => Error::Unsupported(self.description),
            ExtractionIssueKind::Unresolved => Error::Unresolved(self.description),
        }
    }
}

fn nonblank_description(description: String, fallback: &str) -> String {
    if description.trim().is_empty() {
        fallback.to_owned()
    } else {
        description
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExtractionOutcome {
    document: Document<Glyph>,
    issues: Vec<ExtractionIssue>,
}

impl ExtractionOutcome {
    pub fn new(document: Document<Glyph>, issues: Vec<ExtractionIssue>) -> Result<Self> {
        let mut pages = HashSet::with_capacity(issues.len());
        for issue in &issues {
            match issue.scope() {
                ExtractionScope::Document => {}
                ExtractionScope::Page(page) if !pages.insert(page) => {
                    return Err(Error::InvalidConfiguration(format!(
                        "duplicate extraction issue scope for page {}",
                        page.0
                    )));
                }
                ExtractionScope::Page(_) => {}
                ExtractionScope::PageGap { .. } => {}
                ExtractionScope::GlyphGap { retained_before }
                    if retained_before > document.items().len() =>
                {
                    return Err(Error::InvalidConfiguration(format!(
                        "extraction glyph gap boundary {retained_before} exceeds the retained glyph count {}",
                        document.items().len()
                    )));
                }
                ExtractionScope::GlyphGap { .. } => {}
            }
        }
        if let Some(glyph) = document
            .items()
            .iter()
            .find(|glyph| pages.contains(&glyph.page))
        {
            return Err(Error::InvalidConfiguration(format!(
                "a page-scoped extraction issue for page {} cannot retain glyph evidence from that page",
                glyph.page.0
            )));
        }
        Ok(Self { document, issues })
    }

    #[must_use]
    pub fn complete(document: Document<Glyph>) -> Self {
        Self {
            document,
            issues: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.issues.is_empty()
    }

    #[must_use]
    pub fn document(&self) -> &Document<Glyph> {
        &self.document
    }

    #[must_use]
    pub fn issues(&self) -> &[ExtractionIssue] {
        &self.issues
    }

    #[must_use]
    pub fn into_parts(self) -> (Document<Glyph>, Vec<ExtractionIssue>) {
        (self.document, self.issues)
    }

    pub fn into_complete(self) -> Result<Document<Glyph>> {
        match self.issues.into_iter().next() {
            Some(issue) => Err(issue.into_error()),
            None => Ok(self.document),
        }
    }

    fn from_error(scope: ExtractionScope, error: Error) -> Result<Self> {
        Self::new(
            Document::new(Vec::new()),
            vec![ExtractionIssue::from_error(scope, error)?],
        )
    }
}

pub trait GlyphExtractor: Send + Sync {
    fn extract(&self, pdf: &dyn ParsedPdf, limits: ExtractionLimits) -> Result<Document<Glyph>>;

    fn extract_outcome(
        &self,
        pdf: &dyn ParsedPdf,
        limits: ExtractionLimits,
    ) -> Result<ExtractionOutcome> {
        match self.extract(pdf, limits) {
            Ok(document) => Ok(ExtractionOutcome::complete(document)),
            Err(error) => ExtractionOutcome::from_error(ExtractionScope::Document, error),
        }
    }
}

pub struct ParserBackedGlyphSource<P, E> {
    parser: P,
    extractor: E,
}

impl<P, E> ParserBackedGlyphSource<P, E>
where
    P: PdfParser,
    E: GlyphExtractor,
{
    pub fn new(parser: P, extractor: E) -> Self {
        Self { parser, extractor }
    }

    pub fn extract(
        &self,
        pdf: Arc<[u8]>,
        parse_limits: ParseLimits,
        extraction_limits: ExtractionLimits,
    ) -> Result<Document<Glyph>> {
        self.extract_outcome(pdf, parse_limits, extraction_limits)?
            .into_complete()
    }

    pub fn extract_outcome(
        &self,
        pdf: Arc<[u8]>,
        parse_limits: ParseLimits,
        extraction_limits: ExtractionLimits,
    ) -> Result<ExtractionOutcome> {
        let parsed = match self.parser.parse(pdf, parse_limits) {
            Ok(parsed) => parsed,
            Err(error) => return ExtractionOutcome::from_error(ExtractionScope::Document, error),
        };
        self.extractor
            .extract_outcome(parsed.as_ref(), extraction_limits)
    }

    pub fn extract_outcome_with_password(
        &self,
        pdf: Arc<[u8]>,
        parse_limits: ParseLimits,
        extraction_limits: ExtractionLimits,
        password: &str,
    ) -> Result<ExtractionOutcome> {
        let parsed = match self.parser.parse_with_password(pdf, parse_limits, password) {
            Ok(parsed) => parsed,
            Err(error) => return ExtractionOutcome::from_error(ExtractionScope::Document, error),
        };
        self.extractor
            .extract_outcome(parsed.as_ref(), extraction_limits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_font_identities_accept_their_documented_bounds() {
        let mut identities = ExternalFontIdentities::default();
        let base_font = vec![b'A'; ExternalFontIdentities::MAX_BASE_FONT_BYTES];
        let identity = vec![b'i'; ExternalFontIdentities::MAX_IDENTITY_BYTES];
        identities
            .insert(&base_font, &identity)
            .expect("documented boundary lengths are accepted");
        assert!(identities.get(&base_font).is_some());
        let names = identities
            .iter()
            .map(|(name, _)| name.to_vec())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![base_font]);
    }

    #[test]
    fn external_font_identities_reject_empty_and_oversized_inputs() {
        let mut identities = ExternalFontIdentities::default();
        assert!(identities.insert(b"", b"identity").is_err());
        let oversized_base_font = vec![b'A'; ExternalFontIdentities::MAX_BASE_FONT_BYTES + 1];
        assert!(
            identities
                .insert(&oversized_base_font, b"identity")
                .is_err()
        );
        assert!(identities.insert(b"BaseFont", b"").is_err());
        let oversized_identity = vec![b'i'; ExternalFontIdentities::MAX_IDENTITY_BYTES + 1];
        assert!(identities.insert(b"BaseFont", &oversized_identity).is_err());
        assert!(identities.get(b"BaseFont").is_none());
    }

    #[test]
    fn external_font_identities_reject_duplicates_and_excess_entries() {
        let mut identities = ExternalFontIdentities::default();
        identities
            .insert(b"BaseFont", b"identity")
            .expect("the first insertion succeeds");
        assert!(identities.insert(b"BaseFont", b"other").is_err());
        assert_eq!(identities.iter().count(), 1);

        let mut identities = ExternalFontIdentities::default();
        for index in 0..ExternalFontIdentities::MAX_ENTRIES {
            let name = format!("BaseFont{index}");
            identities
                .insert(name.as_bytes(), b"identity")
                .expect("entries below the limit are accepted");
        }
        let error = identities
            .insert(b"BaseFontOverflow", b"identity")
            .expect_err("one entry beyond the limit is rejected");
        assert!(matches!(
            error,
            Error::LimitExceeded {
                resource: "external font identity entries",
                limit,
            } if limit == ExternalFontIdentities::MAX_ENTRIES
        ));
    }

    #[test]
    fn precomputed_font_identities_are_validated_and_match_inserted_digests() {
        let mut identities = ExternalFontIdentities::default();
        identities
            .insert(b"BaseFont", b"identity")
            .expect("source identity");
        let digest = identities
            .iter()
            .find(|(name, _)| *name == b"BaseFont")
            .map(|(_, hash)| hash.clone())
            .expect("inserted identity");

        let mut transported = ExternalFontIdentities::default();
        transported
            .insert_hash(b"BaseFont", digest.clone())
            .expect("precomputed digest");
        assert_eq!(transported.get(b"BaseFont"), Some(&digest));
        assert!(
            transported
                .insert_hash(b"BaseFont", digest.clone())
                .is_err()
        );
        assert!(transported.insert_hash(b"", digest.clone()).is_err());
        assert!(
            transported
                .insert_hash(b"Other", FontProgramHash(vec![0; 31]),)
                .is_err()
        );
        let oversized = vec![b'A'; ExternalFontIdentities::MAX_BASE_FONT_BYTES + 1];
        assert!(transported.insert_hash(&oversized, digest).is_err());
    }
}
