use std::{collections::BTreeMap, sync::Arc};

use crate::Result;

pub mod backend;
pub(crate) mod content;
pub(crate) mod font;

pub use backend::LopdfParser;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    pub max_input_bytes: usize,
    pub max_objects: usize,
    pub max_recursion_depth: usize,
    pub max_decoded_stream_bytes: usize,
    pub max_total_object_stream_bytes: usize,
    pub max_pages: usize,
}

impl Default for ParseLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024 * 1024,
            max_objects: 1_000_000,
            max_recursion_depth: 128,
            max_decoded_stream_bytes: 64 * 1024 * 1024,
            max_total_object_stream_bytes: 256 * 1024 * 1024,
            max_pages: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PdfVersion {
    pub major: u8,
    pub minor: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ObjectRef {
    pub object_number: u32,
    pub generation: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageRef(pub ObjectRef);

pub type PdfDict = BTreeMap<Vec<u8>, PdfObject>;

#[derive(Clone, Debug, PartialEq)]
pub enum PdfObject {
    Null,
    Boolean(bool),
    Integer(i64),
    Real(f64),
    Name(Vec<u8>),
    String(Vec<u8>),
    Array(Vec<Self>),
    Dictionary(PdfDict),
    Stream(PdfDict),
    Reference(ObjectRef),
}

#[derive(Clone, Debug, PartialEq)]
pub struct RawStream {
    /// The original stream dictionary, including filter metadata.
    pub dictionary: PdfDict,
    /// Stream bytes before applying any declared filters.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedStream {
    /// The original stream dictionary retained for provenance.
    pub dictionary: PdfDict,
    /// Stream bytes after applying the declared filters.
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedObject {
    /// The final indirect object reached after following a reference chain.
    pub reference: ObjectRef,
    pub object: PdfObject,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParsedPage {
    pub dictionary: PdfDict,
    pub resources: Option<Arc<PdfObject>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PdfIssueKind {
    Unresolved,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfIssue {
    kind: PdfIssueKind,
    description: String,
}

impl PdfIssue {
    pub fn unresolved(description: impl Into<String>) -> Result<Self> {
        Self::new(PdfIssueKind::Unresolved, description)
    }

    pub fn unsupported(description: impl Into<String>) -> Result<Self> {
        Self::new(PdfIssueKind::Unsupported, description)
    }

    fn new(kind: PdfIssueKind, description: impl Into<String>) -> Result<Self> {
        let description = description.into();
        if description.trim().is_empty() {
            return Err(crate::Error::InvalidConfiguration(
                "PDF issues require a description".to_owned(),
            ));
        }
        Ok(Self { kind, description })
    }

    pub const fn kind(&self) -> PdfIssueKind {
        self.kind
    }

    pub fn description(&self) -> &str {
        &self.description
    }
}

pub trait ParsedPdf: Send + Sync {
    fn version(&self) -> PdfVersion;
    fn trailer(&self) -> Result<PdfDict>;
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject>;
    fn terminal_reference(&self, reference: ObjectRef) -> Result<ObjectRef> {
        Ok(reference)
    }
    fn resolve_with_terminal(&self, reference: ObjectRef) -> Result<ResolvedObject> {
        let terminal = self.terminal_reference(reference)?;
        Ok(ResolvedObject {
            reference: terminal,
            object: self.resolve(terminal)?,
        })
    }
    fn pages(&self) -> Result<Vec<PageRef>>;
    fn page_dict(&self, page: PageRef) -> Result<PdfDict>;
    fn page_snapshot(&self, page: PageRef) -> Result<ParsedPage> {
        let mut dictionary = self.page_dict(page)?;
        let resources = dictionary.remove(b"Resources".as_slice()).map(Arc::new);
        Ok(ParsedPage {
            dictionary,
            resources,
        })
    }
    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream>;
    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream>;
    fn issues(&self) -> &[PdfIssue] {
        &[]
    }
}

pub trait PdfParser: Send + Sync {
    fn parse(&self, pdf: Arc<[u8]>, limits: ParseLimits) -> Result<Box<dyn ParsedPdf>>;

    fn parse_with_password(
        &self,
        pdf: Arc<[u8]>,
        limits: ParseLimits,
        password: &str,
    ) -> Result<Box<dyn ParsedPdf>> {
        if password.is_empty() {
            self.parse(pdf, limits)
        } else {
            Err(crate::Error::Unsupported(
                "configured PDF passwords are not supported by this parser".into(),
            ))
        }
    }
}
