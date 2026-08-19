use std::{collections::BTreeMap, sync::Arc};

use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLimits {
    pub max_input_bytes: usize,
    pub max_objects: usize,
    pub max_recursion_depth: usize,
    pub max_decoded_stream_bytes: usize,
    pub max_pages: usize,
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
    pub dictionary: PdfDict,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedStream {
    pub dictionary: PdfDict,
    pub bytes: Vec<u8>,
}

pub trait ParsedPdf: Send + Sync {
    fn version(&self) -> PdfVersion;
    fn trailer(&self) -> Result<PdfDict>;
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject>;
    fn pages(&self) -> Result<Vec<PageRef>>;
    fn page_dict(&self, page: PageRef) -> Result<PdfDict>;
    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream>;
    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream>;
}

pub trait PdfParser: Send + Sync {
    fn parse(&self, pdf: Arc<[u8]>, limits: ParseLimits) -> Result<Box<dyn ParsedPdf>>;
}
