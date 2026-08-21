use std::{
    collections::{BTreeMap, HashSet},
    sync::Arc,
};

use sha2::{Digest, Sha256};

use crate::{
    Error, Result,
    model::{Document, FontProgramHash, Glyph, PageId},
    pdf::{ParseLimits, ParsedPdf, PdfIssue, PdfParser},
};

mod content_stream;

pub use content_stream::ContentStreamGlyphExtractor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionLimits {
    pub max_glyphs: usize,
    pub max_form_depth: usize,
    pub max_nesting_depth: usize,
    pub max_operators: usize,
    pub max_stream_invocations: usize,
    pub max_total_decoded_bytes: usize,
    pub max_operand_stack: usize,
    pub max_operand_nodes: usize,
    pub max_fonts: usize,
    pub max_cmap_entries: usize,
    pub max_cid_width_entries: usize,
    pub max_string_bytes: usize,
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
            // The measured multilingual corpus peaks at 8,732,907 operand nodes.
            max_operand_nodes: 10_000_000,
            max_fonts: 100_000,
            max_cmap_entries: 1_000_000,
            // One complete u16 CID space globally. Multiplying this by max_fonts would
            // permit billions of entries by default; callers can raise it deliberately.
            max_cid_width_entries: usize::from(u16::MAX) + 1,
            max_string_bytes: 64 * 1024 * 1024,
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

    pub(crate) fn get(&self, base_font: &[u8]) -> Option<&FontProgramHash> {
        self.identities.get(base_font)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionIssueKind {
    Unsupported,
    Unresolved,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionScope {
    Document,
    Page(PageId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

    pub const fn kind(&self) -> ExtractionIssueKind {
        self.kind
    }

    pub const fn scope(&self) -> ExtractionScope {
        self.scope
    }

    pub fn description(&self) -> &str {
        &self.description
    }

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
            ExtractionIssueKind::Unresolved,
            ExtractionScope::Document,
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

    pub fn complete(document: Document<Glyph>) -> Self {
        Self {
            document,
            issues: Vec::new(),
        }
    }

    pub fn is_complete(&self) -> bool {
        self.issues.is_empty()
    }

    pub fn document(&self) -> &Document<Glyph> {
        &self.document
    }

    pub fn issues(&self) -> &[ExtractionIssue] {
        &self.issues
    }

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
