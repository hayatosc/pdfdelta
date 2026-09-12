use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{
    Error, Result,
    model::FontProgramHash,
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
};
use sha2::{Digest, Sha256};

use super::{
    cmap::{ToUnicodeCMap, parse_to_unicode_for_width},
    decoder::FontDecoderLimits,
};

pub(super) fn load_to_unicode(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
    source_width: usize,
) -> Result<(Option<ToUnicodeCMap>, usize)> {
    let Some(to_unicode) = dictionary.get(b"ToUnicode".as_slice()) else {
        return Ok((None, 0));
    };
    // A null entry is equivalent to an absent one, and the font remains usable
    // without a Unicode map.
    if matches!(to_unicode, PdfObject::Null) {
        return Ok((None, 0));
    }
    let reference =
        resolve_stream_reference(pdf, to_unicode, limits.max_indirections, "ToUnicode")?;
    let stream = pdf.decoded_stream(reference)?;
    if stream.bytes.len() > limits.max_decoded_font_bytes {
        return Err(Error::LimitExceeded {
            resource: "decoded ToUnicode bytes",
            limit: limits.max_decoded_font_bytes,
        });
    }
    let byte_count = stream.bytes.len();
    let cmap = parse_to_unicode_for_width(&stream.bytes, limits.cmap, source_width)?;
    Ok((Some(cmap), byte_count))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FontIdentityDomain {
    SimpleType1BuiltIn,
    SimpleType1C,
    SimpleTrueTypeSymbolic,
    SimpleTrueTypeNonsymbolic,
    CidFontType0,
    CidFontType2Identity,
}

impl FontIdentityDomain {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::SimpleType1BuiltIn => b"simple-type1-built-in",
            Self::SimpleType1C => b"simple-type1c",
            Self::SimpleTrueTypeSymbolic => b"simple-truetype-symbolic",
            Self::SimpleTrueTypeNonsymbolic => b"simple-truetype-nonsymbolic",
            Self::CidFontType0 => b"cidfont-type0-cid",
            Self::CidFontType2Identity => b"cidfont-type2-identity",
        }
    }

    fn program_key(self) -> &'static [u8] {
        match self {
            Self::SimpleType1BuiltIn => b"FontFile",
            Self::SimpleType1C => b"FontFile3",
            Self::SimpleTrueTypeSymbolic | Self::SimpleTrueTypeNonsymbolic => b"FontFile2",
            Self::CidFontType0 => b"FontFile3",
            Self::CidFontType2Identity => b"FontFile2",
        }
    }

    fn font_file3_subtype(self) -> Option<&'static [u8]> {
        match self {
            Self::SimpleType1C => Some(b"Type1C"),
            Self::CidFontType0 => Some(b"CIDFontType0C"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FontIdentitySource {
    kind: FontIdentitySourceKind,
}

#[derive(Clone, Debug)]
enum FontIdentitySourceKind {
    Standard14 {
        name: &'static [u8],
        encoding: &'static [u8],
    },
    Embedded {
        reference: ObjectRef,
        domain: FontIdentityDomain,
    },
    Type3 {
        char_procs: Vec<Type3CharProcIdentitySource>,
        resources: Option<PdfObject>,
        max_graph_depth: usize,
    },
}

impl FontIdentitySource {
    pub(super) fn standard14(name: &'static [u8], encoding: &'static [u8]) -> Self {
        Self {
            kind: FontIdentitySourceKind::Standard14 { name, encoding },
        }
    }
}

#[derive(Clone, Debug)]
struct Type3CharProcIdentitySource {
    name: Vec<u8>,
    reference: ObjectRef,
}

pub(super) struct Type3FontIdentityBinding {
    pub(super) source: FontIdentitySource,
    pub(super) glyph_ids_by_name: BTreeMap<Vec<u8>, u16>,
}

pub(crate) struct LoadedFontIdentity {
    pub(crate) hash: FontProgramHash,
    pub(crate) decoded_bytes: usize,
}

pub(super) fn resolve_font_identity_source(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    max_indirections: usize,
    domain: FontIdentityDomain,
) -> Result<Option<FontIdentitySource>> {
    let Some(descriptor) = dictionary.get(b"FontDescriptor".as_slice()) else {
        return Ok(None);
    };
    let descriptor = resolve_object(pdf, descriptor.clone(), max_indirections)?;
    let descriptor = match descriptor {
        PdfObject::Dictionary(descriptor) => descriptor,
        PdfObject::Null => return Ok(None),
        _ => return unresolved("FontDescriptor is not a dictionary"),
    };
    let programs = [b"FontFile".as_slice(), b"FontFile2", b"FontFile3"]
        .into_iter()
        .filter(|key| descriptor.contains_key(*key))
        .collect::<Vec<_>>();
    if programs.as_slice() != [domain.program_key()] {
        return Ok(None);
    }
    let program = descriptor
        .get(domain.program_key())
        .ok_or_else(|| Error::Unresolved("validated embedded font program disappeared".into()))?;
    let Some((reference, stream_dictionary)) =
        resolve_identity_stream_reference(pdf, program, max_indirections)?
    else {
        return Ok(None);
    };
    if let Some(required_subtype) = domain.font_file3_subtype()
        && !matches!(
            stream_dictionary.get(b"Subtype".as_slice()),
            Some(PdfObject::Name(subtype)) if subtype.as_slice() == required_subtype
        )
    {
        return Ok(None);
    }
    Ok(Some(FontIdentitySource {
        kind: FontIdentitySourceKind::Embedded { reference, domain },
    }))
}

pub(super) fn type3_font_identity_source(
    pdf: &dyn ParsedPdf,
    char_procs: &PdfDict,
    resources: Option<&PdfObject>,
    max_indirections: usize,
) -> Result<Option<Type3FontIdentityBinding>> {
    if char_procs.is_empty() {
        return Ok(None);
    }
    let mut sources = Vec::with_capacity(char_procs.len());
    let mut glyph_ids = BTreeMap::new();
    // PdfDict's bytewise name order gives each CharProc a stable glyph ID. Encoding codes only
    // select that ID, so equivalent fonts remain comparable when codes are reassigned.
    for (index, (name, object)) in char_procs.iter().enumerate() {
        let Some((reference, _)) =
            resolve_identity_stream_reference(pdf, object, max_indirections)?
        else {
            return Ok(None);
        };
        let glyph_id = u16::try_from(index).map_err(|_| Error::LimitExceeded {
            resource: "Type 3 CharProcs entries",
            limit: usize::from(u16::MAX) + 1,
        })?;
        sources.push(Type3CharProcIdentitySource {
            name: name.clone(),
            reference,
        });
        glyph_ids.insert(name.clone(), glyph_id);
    }
    Ok(Some(Type3FontIdentityBinding {
        source: FontIdentitySource {
            kind: FontIdentitySourceKind::Type3 {
                char_procs: sources,
                resources: resources.cloned(),
                max_graph_depth: max_indirections,
            },
        },
        glyph_ids_by_name: glyph_ids,
    }))
}

pub(crate) fn load_font_identity(
    pdf: &dyn ParsedPdf,
    source: &FontIdentitySource,
    max_decoded_bytes: usize,
) -> Result<LoadedFontIdentity> {
    let mut digest = identity_digest();
    let decoded_bytes = match &source.kind {
        FontIdentitySourceKind::Standard14 { name, encoding } => {
            digest.update(b"standard-14\0");
            digest.update(name);
            digest.update(b"\0");
            digest.update(encoding);
            0
        }
        FontIdentitySourceKind::Embedded { reference, domain } => {
            let stream = pdf.decoded_stream(*reference)?;
            if stream.bytes.len() > max_decoded_bytes {
                return Err(Error::LimitExceeded {
                    resource: "decoded embedded font bytes",
                    limit: max_decoded_bytes,
                });
            }
            digest.update(domain.tag());
            digest.update(b"\0");
            digest.update(&stream.bytes);
            stream.bytes.len()
        }
        FontIdentitySourceKind::Type3 {
            char_procs,
            resources,
            max_graph_depth,
        } => {
            // FontMatrix and Widths are geometry evidence, not glyph-token identity. CharProc
            // names and decoded programs are framed and hashed without interpreting operators.
            digest.update(b"simple-type3-charprocs\0");
            let mut decoded_bytes = 0usize;
            for char_proc in char_procs {
                decoded_bytes = decoded_bytes.checked_add(char_proc.name.len()).ok_or(
                    Error::LimitExceeded {
                        resource: "decoded Type 3 font identity bytes",
                        limit: max_decoded_bytes,
                    },
                )?;
                ensure_identity_bytes(decoded_bytes, max_decoded_bytes)?;
                let stream =
                    pdf.decoded_stream(char_proc.reference)
                        .map_err(|error| match error {
                            Error::Unsupported(message) => Error::Unresolved(format!(
                                "Type 3 CharProc cannot be decoded for stable identity: {message}"
                            )),
                            error => error,
                        })?;
                decoded_bytes =
                    decoded_bytes
                        .checked_add(stream.bytes.len())
                        .ok_or(Error::LimitExceeded {
                            resource: "decoded Type 3 font identity bytes",
                            limit: max_decoded_bytes,
                        })?;
                ensure_identity_bytes(decoded_bytes, max_decoded_bytes)?;
                digest.update((char_proc.name.len() as u64).to_be_bytes());
                digest.update(&char_proc.name);
                digest.update((stream.bytes.len() as u64).to_be_bytes());
                digest.update(&stream.bytes);
            }
            if let Some(resources) = resources {
                let mut hasher = Type3ResourceHasher::new(
                    pdf,
                    *max_graph_depth,
                    max_decoded_bytes,
                    decoded_bytes,
                );
                let resource_hash = hasher.hash(resources, 0)?;
                decoded_bytes = hasher.identity_bytes;
                digest.update(b"type3-resources\0");
                digest.update(resource_hash);
            }
            decoded_bytes
        }
    };
    let hash = FontProgramHash(digest.finalize().to_vec());
    Ok(LoadedFontIdentity {
        hash,
        decoded_bytes,
    })
}

struct Type3ResourceHasher<'a> {
    pdf: &'a dyn ParsedPdf,
    max_graph_depth: usize,
    max_identity_bytes: usize,
    identity_bytes: usize,
    cached: HashMap<ObjectRef, [u8; 32]>,
    resolving: HashSet<ObjectRef>,
}

impl<'a> Type3ResourceHasher<'a> {
    fn new(
        pdf: &'a dyn ParsedPdf,
        max_graph_depth: usize,
        max_identity_bytes: usize,
        identity_bytes: usize,
    ) -> Self {
        Self {
            pdf,
            max_graph_depth,
            max_identity_bytes,
            identity_bytes,
            cached: HashMap::new(),
            resolving: HashSet::new(),
        }
    }

    fn hash(&mut self, object: &PdfObject, depth: usize) -> Result<[u8; 32]> {
        if depth > self.max_graph_depth {
            return Err(Error::LimitExceeded {
                resource: "Type 3 resource identity graph depth",
                limit: self.max_graph_depth,
            });
        }
        self.charge(1)?;
        match object {
            PdfObject::Reference(reference) => self.hash_reference(*reference, depth),
            PdfObject::Stream(_) => {
                unresolved("direct Type 3 resource streams are unavailable through the PDF facade")
            }
            PdfObject::Null => Ok(tagged_hash(b"null", &[])),
            PdfObject::Boolean(value) => Ok(tagged_hash(b"boolean", &[u8::from(*value)])),
            PdfObject::Integer(value) => Ok(tagged_hash(b"integer", &value.to_be_bytes())),
            PdfObject::Real(value) if value.is_finite() => {
                let normalized = if *value == 0.0 { 0.0 } else { *value };
                Ok(tagged_hash(b"real", &normalized.to_bits().to_be_bytes()))
            }
            PdfObject::Real(_) => unresolved("Type 3 resource contains a non-finite number"),
            PdfObject::Name(value) => self.hash_bytes(b"name", value),
            PdfObject::String(value) => self.hash_bytes(b"string", value),
            PdfObject::Array(values) => {
                let mut digest = Sha256::new();
                digest.update(b"array\0");
                digest.update((values.len() as u64).to_be_bytes());
                for value in values {
                    digest.update(self.hash(value, depth + 1)?);
                }
                Ok(digest.finalize().into())
            }
            PdfObject::Dictionary(dictionary) => self.hash_dictionary(dictionary, depth + 1, false),
        }
    }

    fn hash_reference(&mut self, reference: ObjectRef, depth: usize) -> Result<[u8; 32]> {
        if let Some(hash) = self.cached.get(&reference) {
            return Ok(*hash);
        }
        if !self.resolving.insert(reference) {
            return unresolved("Type 3 resource identity graph contains a reference cycle");
        }
        let object = self.pdf.resolve(reference)?;
        let result = match object {
            PdfObject::Stream(dictionary) => {
                let stream = self.pdf.decoded_stream(reference).map_err(|error| match error {
                    Error::Unsupported(message) => Error::Unresolved(format!(
                        "Type 3 resource stream cannot be decoded for stable identity: {message}"
                    )),
                    error => error,
                })?;
                self.charge(stream.bytes.len())?;
                let mut digest = Sha256::new();
                digest.update(b"stream\0");
                digest.update(self.hash_dictionary(&dictionary, depth + 1, true)?);
                digest.update((stream.bytes.len() as u64).to_be_bytes());
                digest.update(&stream.bytes);
                Ok(digest.finalize().into())
            }
            object => self.hash(&object, depth + 1),
        };
        self.resolving.remove(&reference);
        if let Ok(hash) = result {
            self.cached.insert(reference, hash);
        }
        result
    }

    fn hash_dictionary(
        &mut self,
        dictionary: &PdfDict,
        depth: usize,
        decoded_stream: bool,
    ) -> Result<[u8; 32]> {
        validate_type3_resource_font(dictionary)?;
        let mut digest = Sha256::new();
        digest.update(b"dictionary\0");
        for (key, value) in dictionary {
            if decoded_stream && matches!(key.as_slice(), b"Length" | b"Filter" | b"DecodeParms") {
                continue;
            }
            self.charge(key.len())?;
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(key);
            digest.update(self.hash(value, depth + 1)?);
        }
        Ok(digest.finalize().into())
    }

    fn hash_bytes(&mut self, tag: &[u8], value: &[u8]) -> Result<[u8; 32]> {
        self.charge(value.len())?;
        Ok(tagged_hash(tag, value))
    }

    fn charge(&mut self, bytes: usize) -> Result<()> {
        self.identity_bytes =
            self.identity_bytes
                .checked_add(bytes)
                .ok_or(Error::LimitExceeded {
                    resource: "decoded Type 3 font identity bytes",
                    limit: self.max_identity_bytes,
                })?;
        ensure_identity_bytes(self.identity_bytes, self.max_identity_bytes)
    }
}

fn tagged_hash(tag: &[u8], value: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(tag);
    digest.update(b"\0");
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
    digest.finalize().into()
}

fn validate_type3_resource_font(dictionary: &PdfDict) -> Result<()> {
    let declared_font = matches!(
        dictionary.get(b"Type".as_slice()),
        Some(PdfObject::Name(name)) if name.as_slice() == b"Font"
    );
    let font_shaped = dictionary.contains_key(b"BaseFont".as_slice())
        || (dictionary.contains_key(b"CharProcs".as_slice())
            && dictionary.contains_key(b"FontMatrix".as_slice()));
    if !declared_font && !font_shaped {
        return Ok(());
    }
    match dictionary.get(b"Subtype".as_slice()) {
        Some(PdfObject::Name(subtype)) if subtype.as_slice() == b"Type3" => Ok(()),
        Some(PdfObject::Name(subtype))
            if subtype.as_slice() == b"Type1"
                && !dictionary.contains_key(b"FontDescriptor".as_slice())
                && matches!(
                    dictionary.get(b"BaseFont".as_slice()),
                    Some(PdfObject::Name(name)) if is_standard_14_name(name)
                ) =>
        {
            Ok(())
        }
        _ => {
            unresolved("Type 3 resource graph contains a font without canonical embedded identity")
        }
    }
}

fn is_standard_14_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"Courier"
            | b"Courier-Bold"
            | b"Courier-Oblique"
            | b"Courier-BoldOblique"
            | b"Helvetica"
            | b"Helvetica-Bold"
            | b"Helvetica-Oblique"
            | b"Helvetica-BoldOblique"
            | b"Times-Roman"
            | b"Times-Bold"
            | b"Times-Italic"
            | b"Times-BoldItalic"
            | b"Symbol"
            | b"ZapfDingbats"
    )
}

fn identity_digest() -> Sha256 {
    let mut digest = Sha256::new();
    digest.update(b"pdfdelta-font-glyph-identity\0v1\0");
    digest
}

fn ensure_identity_bytes(decoded_bytes: usize, limit: usize) -> Result<()> {
    if decoded_bytes > limit {
        return Err(Error::LimitExceeded {
            resource: "decoded Type 3 font identity bytes",
            limit,
        });
    }
    Ok(())
}

pub(super) fn resolve_stream_reference(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    max_indirections: usize,
    context: &str,
) -> Result<ObjectRef> {
    let mut current = object.clone();
    for depth in 0..=max_indirections {
        let PdfObject::Reference(reference) = current else {
            return match current {
                PdfObject::Stream(_) => Err(Error::Unsupported(format!(
                    "direct {context} streams are unavailable through the PDF facade"
                ))),
                _ => unresolved(&format!("{context} is not a stream reference")),
            };
        };
        if depth == max_indirections {
            return limit_indirections(max_indirections);
        }
        current = pdf.resolve(reference)?;
        if matches!(current, PdfObject::Stream(_)) {
            return Ok(reference);
        }
    }
    limit_indirections(max_indirections)
}

fn resolve_identity_stream_reference(
    pdf: &dyn ParsedPdf,
    object: &PdfObject,
    max_indirections: usize,
) -> Result<Option<(ObjectRef, PdfDict)>> {
    let mut current = object.clone();
    for depth in 0..=max_indirections {
        let PdfObject::Reference(reference) = current else {
            return Ok(None);
        };
        if depth == max_indirections {
            return limit_indirections(max_indirections);
        }
        current = match pdf.resolve(reference)? {
            PdfObject::Stream(dictionary) => return Ok(Some((reference, dictionary))),
            current => current,
        };
    }
    limit_indirections(max_indirections)
}

pub(super) fn resolve_object(
    pdf: &dyn ParsedPdf,
    mut object: PdfObject,
    max_indirections: usize,
) -> Result<PdfObject> {
    for depth in 0..=max_indirections {
        let PdfObject::Reference(reference) = object else {
            return Ok(object);
        };
        if depth == max_indirections {
            return limit_indirections(max_indirections);
        }
        object = pdf.resolve(reference)?;
    }
    limit_indirections(max_indirections)
}

fn limit_indirections<T>(limit: usize) -> Result<T> {
    Err(Error::LimitExceeded {
        resource: "font object indirections",
        limit,
    })
}

pub(super) fn optional_number(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    key: &[u8],
    max_indirections: usize,
) -> Result<Option<f64>> {
    dictionary
        .get(key)
        .map(|value| {
            let value = resolve_object(pdf, value.clone(), max_indirections)?;
            finite_number(&value, "font metric")
        })
        .transpose()
}

pub(super) fn load_descriptor_bbox(
    pdf: &dyn ParsedPdf,
    descriptor: &PdfDict,
    max_indirections: usize,
) -> Result<Option<(f64, f64)>> {
    let Some(font_bbox) = descriptor.get(b"FontBBox".as_slice()) else {
        return Ok(None);
    };
    let font_bbox = resolve_object(pdf, font_bbox.clone(), max_indirections)?;
    let PdfObject::Array(font_bbox) = font_bbox else {
        return unresolved("FontDescriptor FontBBox is not an array");
    };
    if font_bbox.len() != 4 {
        return unresolved("FontDescriptor FontBBox does not contain four numbers");
    }
    let font_bbox = font_bbox
        .into_iter()
        .map(|value| {
            let value = resolve_object(pdf, value, max_indirections)?;
            finite_number(&value, "FontDescriptor FontBBox value")
        })
        .collect::<Result<Vec<_>>>()?;
    if font_bbox[0] > font_bbox[2] || font_bbox[1] >= font_bbox[3] {
        return unresolved("FontDescriptor FontBBox has invalid bounds");
    }
    Ok(Some((font_bbox[3], font_bbox[1])))
}

pub(super) fn non_negative_number(object: &PdfObject, context: &str) -> Result<f64> {
    let value = finite_number(object, context)?;
    if value < 0.0 {
        return unresolved(&format!("{context} is negative"));
    }
    Ok(value)
}

pub(super) fn finite_number(object: &PdfObject, context: &str) -> Result<f64> {
    let value = match object {
        PdfObject::Integer(value) => *value as f64,
        PdfObject::Real(value) => *value,
        _ => return unresolved(&format!("{context} is not numeric")),
    };
    if !value.is_finite() {
        return unresolved(&format!("{context} is not finite"));
    }
    Ok(value)
}

pub(super) fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
}

/// Replaces ascent/descent with the descriptor `FontBBox` extent whenever the
/// declared metrics cannot form a positive vertical extent.
pub(super) fn apply_bbox_vertical_fallback(
    (ascent, descent): (Option<f64>, Option<f64>),
    load_bbox: impl FnOnce() -> Result<Option<(f64, f64)>>,
) -> Result<(Option<f64>, Option<f64>)> {
    if ascent
        .zip(descent)
        .is_none_or(|(ascent, descent)| ascent <= descent)
        && let Some((bbox_ascent, bbox_descent)) = load_bbox()?
    {
        return Ok((Some(bbox_ascent), Some(bbox_descent)));
    }
    Ok((ascent, descent))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::pdf::{DecodedStream, PageRef, PdfVersion, RawStream};

    use super::*;

    #[test]
    fn resolves_then_lazily_hashes_decoded_font_bytes_with_a_mapping_domain() -> Result<()> {
        let program_bytes = b"decoded font program".to_vec();
        let pdf = MockPdf {
            objects: HashMap::from([
                (
                    object_ref(1),
                    PdfObject::Dictionary(PdfDict::from([(
                        b"FontFile2".to_vec(),
                        PdfObject::Reference(object_ref(2)),
                    )])),
                ),
                (object_ref(2), PdfObject::Reference(object_ref(3))),
                (object_ref(3), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([(object_ref(3), program_bytes.clone())]),
            unsupported_decoding: false,
            decoded_calls: AtomicUsize::new(0),
        };
        let font = PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Reference(object_ref(1)),
        )]);

        let source = resolve_font_identity_source(
            &pdf,
            &font,
            4,
            FontIdentityDomain::SimpleTrueTypeNonsymbolic,
        )?
        .ok_or_else(|| Error::Unresolved("fixture should contain a font program".into()))?;
        assert_eq!(pdf.decoded_calls.load(Ordering::Relaxed), 0);
        let loaded = load_font_identity(&pdf, &source, usize::MAX)?;

        assert_eq!(loaded.decoded_bytes, program_bytes.len());
        assert_eq!(loaded.hash.0.len(), 32);
        assert_eq!(pdf.decoded_calls.load(Ordering::Relaxed), 1);
        Ok(())
    }

    #[test]
    fn separates_identical_programs_across_mapping_domains() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([
                (
                    object_ref(1),
                    PdfObject::Stream(PdfDict::from([(
                        b"Subtype".to_vec(),
                        PdfObject::Name(b"CIDFontType0C".to_vec()),
                    )])),
                ),
                (
                    object_ref(2),
                    PdfObject::Stream(PdfDict::from([(
                        b"Subtype".to_vec(),
                        PdfObject::Name(b"Type1C".to_vec()),
                    )])),
                ),
            ]),
            streams: HashMap::from([
                (object_ref(1), b"same program".to_vec()),
                (object_ref(2), b"same program".to_vec()),
            ]),
            ..MockPdf::default()
        };
        let mut hashes = Vec::new();
        for domain in [
            FontIdentityDomain::SimpleType1BuiltIn,
            FontIdentityDomain::SimpleType1C,
            FontIdentityDomain::SimpleTrueTypeSymbolic,
            FontIdentityDomain::SimpleTrueTypeNonsymbolic,
            FontIdentityDomain::CidFontType0,
            FontIdentityDomain::CidFontType2Identity,
        ] {
            let reference = if matches!(domain, FontIdentityDomain::SimpleType1C) {
                object_ref(2)
            } else {
                object_ref(1)
            };
            let font = font_with_program_reference(domain, reference);
            let source = resolve_font_identity_source(&pdf, &font, 4, domain)?
                .ok_or_else(|| Error::Unresolved("fixture should contain a font program".into()))?;
            hashes.push(load_font_identity(&pdf, &source, usize::MAX)?.hash);
        }
        hashes.sort();
        hashes.dedup();

        assert_eq!(hashes.len(), 6);
        Ok(())
    }

    #[test]
    fn type3_identity_is_deterministic_bounded_and_domain_separated() -> Result<()> {
        let first_pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(11), PdfObject::Stream(PdfDict::new())),
                (object_ref(12), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([
                (object_ref(11), b"first proc".to_vec()),
                (object_ref(12), b"second proc".to_vec()),
            ]),
            ..MockPdf::default()
        };
        let first_char_procs = PdfDict::from([
            (b"c3".to_vec(), PdfObject::Reference(object_ref(11))),
            (b"c8".to_vec(), PdfObject::Reference(object_ref(12))),
        ]);
        let first_binding = type3_font_identity_source(&first_pdf, &first_char_procs, None, 4)?
            .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
        let first = load_font_identity(&first_pdf, &first_binding.source, usize::MAX)?;

        let second_pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(91), PdfObject::Stream(PdfDict::new())),
                (object_ref(92), PdfObject::Stream(PdfDict::new())),
            ]),
            streams: HashMap::from([
                (object_ref(91), b"second proc".to_vec()),
                (object_ref(92), b"first proc".to_vec()),
            ]),
            ..MockPdf::default()
        };
        let second_char_procs = PdfDict::from([
            (b"c8".to_vec(), PdfObject::Reference(object_ref(91))),
            (b"c3".to_vec(), PdfObject::Reference(object_ref(92))),
        ]);
        let second_binding = type3_font_identity_source(&second_pdf, &second_char_procs, None, 4)?
            .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
        let second = load_font_identity(&second_pdf, &second_binding.source, usize::MAX)?;

        assert_eq!(first.hash, second.hash);
        assert_eq!(
            first_binding.glyph_ids_by_name,
            second_binding.glyph_ids_by_name
        );
        assert_eq!(
            first.decoded_bytes,
            4 + b"first proc".len() + b"second proc".len()
        );
        assert!(matches!(
            load_font_identity(&first_pdf, &first_binding.source, first.decoded_bytes - 1,),
            Err(Error::LimitExceeded {
                resource: "decoded Type 3 font identity bytes",
                ..
            })
        ));

        let embedded_pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Stream(PdfDict::new()))]),
            streams: HashMap::from([(
                object_ref(1),
                [b"first proc".as_slice(), b"second proc"].concat(),
            )]),
            ..MockPdf::default()
        };
        let embedded_font =
            font_with_program_reference(FontIdentityDomain::SimpleType1BuiltIn, object_ref(1));
        let embedded_source = resolve_font_identity_source(
            &embedded_pdf,
            &embedded_font,
            4,
            FontIdentityDomain::SimpleType1BuiltIn,
        )?
        .ok_or_else(|| Error::Unresolved("fixture should have embedded identity".into()))?;
        assert_ne!(
            first.hash,
            load_font_identity(&embedded_pdf, &embedded_source, usize::MAX)?.hash
        );
        Ok(())
    }

    #[test]
    fn type3_identity_hashes_bounded_resource_graphs_without_object_ids() -> Result<()> {
        let first_pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(1), PdfObject::Stream(PdfDict::new())),
                (
                    object_ref(2),
                    PdfObject::Dictionary(PdfDict::from([
                        (b"Type".to_vec(), PdfObject::Name(b"Font".to_vec())),
                        (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
                        (b"BaseFont".to_vec(), PdfObject::Name(b"Helvetica".to_vec())),
                    ])),
                ),
                (
                    object_ref(3),
                    PdfObject::Stream(PdfDict::from([(
                        b"Resources".to_vec(),
                        PdfObject::Dictionary(PdfDict::from([(
                            b"Font".to_vec(),
                            PdfObject::Dictionary(PdfDict::from([(
                                b"F1".to_vec(),
                                PdfObject::Reference(object_ref(2)),
                            )])),
                        )])),
                    )])),
                ),
            ]),
            streams: HashMap::from([
                (object_ref(1), b"char proc".to_vec()),
                (object_ref(3), b"pattern program".to_vec()),
            ]),
            ..MockPdf::default()
        };
        let char_procs = PdfDict::from([(b"glyph".to_vec(), PdfObject::Reference(object_ref(1)))]);
        let resources = PdfObject::Dictionary(PdfDict::from([(
            b"Pattern".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"P1".to_vec(),
                PdfObject::Reference(object_ref(3)),
            )])),
        )]));
        let binding = type3_font_identity_source(&first_pdf, &char_procs, Some(&resources), 64)?
            .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
        let first = load_font_identity(&first_pdf, &binding.source, usize::MAX)?;

        let second_pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(11), PdfObject::Stream(PdfDict::new())),
                (
                    object_ref(12),
                    first_pdf
                        .objects
                        .get(&object_ref(2))
                        .cloned()
                        .ok_or_else(|| Error::Backend("missing fixture font".into()))?,
                ),
                (
                    object_ref(13),
                    PdfObject::Stream(PdfDict::from([(
                        b"Resources".to_vec(),
                        PdfObject::Dictionary(PdfDict::from([(
                            b"Font".to_vec(),
                            PdfObject::Dictionary(PdfDict::from([(
                                b"F1".to_vec(),
                                PdfObject::Reference(object_ref(12)),
                            )])),
                        )])),
                    )])),
                ),
            ]),
            streams: HashMap::from([
                (object_ref(11), b"char proc".to_vec()),
                (object_ref(13), b"pattern program".to_vec()),
            ]),
            ..MockPdf::default()
        };
        let second_char_procs =
            PdfDict::from([(b"glyph".to_vec(), PdfObject::Reference(object_ref(11)))]);
        let second_resources = PdfObject::Dictionary(PdfDict::from([(
            b"Pattern".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"P1".to_vec(),
                PdfObject::Reference(object_ref(13)),
            )])),
        )]));
        let second_binding = type3_font_identity_source(
            &second_pdf,
            &second_char_procs,
            Some(&second_resources),
            64,
        )?
        .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
        let second = load_font_identity(&second_pdf, &second_binding.source, usize::MAX)?;

        assert_eq!(first.hash, second.hash);
        assert!(first.decoded_bytes > b"char proc".len() + b"pattern program".len());
        assert!(matches!(
            load_font_identity(&first_pdf, &binding.source, first.decoded_bytes - 1),
            Err(Error::LimitExceeded {
                resource: "decoded Type 3 font identity bytes",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn type3_identity_rejects_resource_cycles_and_unstable_fonts() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(1), PdfObject::Stream(PdfDict::new())),
                (
                    object_ref(2),
                    PdfObject::Dictionary(PdfDict::from([(
                        b"Next".to_vec(),
                        PdfObject::Reference(object_ref(2)),
                    )])),
                ),
                (
                    object_ref(3),
                    PdfObject::Dictionary(PdfDict::from([
                        (b"Type".to_vec(), PdfObject::Name(b"Font".to_vec())),
                        (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
                        (
                            b"BaseFont".to_vec(),
                            PdfObject::Name(b"CustomFont".to_vec()),
                        ),
                    ])),
                ),
            ]),
            streams: HashMap::from([(object_ref(1), b"char proc".to_vec())]),
            ..MockPdf::default()
        };
        let char_procs = PdfDict::from([(b"glyph".to_vec(), PdfObject::Reference(object_ref(1)))]);
        for resources in [
            PdfObject::Reference(object_ref(2)),
            PdfObject::Reference(object_ref(3)),
        ] {
            let binding = type3_font_identity_source(&pdf, &char_procs, Some(&resources), 8)?
                .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
            assert!(matches!(
                load_font_identity(&pdf, &binding.source, usize::MAX),
                Err(Error::Unresolved(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn type3_identity_requires_indirect_decodable_char_procs() -> Result<()> {
        let direct = PdfDict::from([(b"c3".to_vec(), PdfObject::Stream(PdfDict::new()))]);
        assert!(type3_font_identity_source(&MockPdf::default(), &direct, None, 4)?.is_none());

        let pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Stream(PdfDict::new()))]),
            streams: HashMap::from([(object_ref(1), b"proc".to_vec())]),
            unsupported_decoding: true,
            decoded_calls: AtomicUsize::new(0),
        };
        let char_procs = PdfDict::from([(b"c3".to_vec(), PdfObject::Reference(object_ref(1)))]);
        let binding = type3_font_identity_source(&pdf, &char_procs, None, 4)?
            .ok_or_else(|| Error::Unresolved("fixture should have Type 3 identity".into()))?;
        assert!(matches!(
            load_font_identity(&pdf, &binding.source, usize::MAX),
            Err(Error::Unresolved(message))
                if message.contains("CharProc cannot be decoded for stable identity")
        ));
        Ok(())
    }

    #[test]
    fn distinguishes_absent_and_ambiguous_embedded_font_programs() -> Result<()> {
        let pdf = MockPdf::default();
        assert!(
            resolve_font_identity_source(
                &pdf,
                &PdfDict::new(),
                4,
                FontIdentityDomain::SimpleType1BuiltIn,
            )?
            .is_none()
        );
        let without_file = PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::new()),
        )]);
        assert!(
            resolve_font_identity_source(
                &pdf,
                &without_file,
                4,
                FontIdentityDomain::SimpleType1BuiltIn,
            )?
            .is_none()
        );

        let ambiguous = PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (b"FontFile".to_vec(), PdfObject::Reference(object_ref(1))),
                (b"FontFile2".to_vec(), PdfObject::Reference(object_ref(2))),
            ])),
        )]);
        assert!(matches!(
            resolve_font_identity_source(
                &pdf,
                &ambiguous,
                4,
                FontIdentityDomain::SimpleType1BuiltIn,
            ),
            Ok(None)
        ));

        let ambiguous_type1c = PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([
                (b"FontFile".to_vec(), PdfObject::Reference(object_ref(1))),
                (b"FontFile3".to_vec(), PdfObject::Reference(object_ref(2))),
            ])),
        )]);
        assert!(matches!(
            resolve_font_identity_source(
                &pdf,
                &ambiguous_type1c,
                4,
                FontIdentityDomain::SimpleType1C,
            ),
            Ok(None)
        ));
        Ok(())
    }

    #[test]
    fn rejects_program_key_and_font_file3_subtype_mismatches() -> Result<()> {
        let pdf = MockPdf {
            objects: HashMap::from([
                (object_ref(1), PdfObject::Stream(PdfDict::new())),
                (
                    object_ref(2),
                    PdfObject::Stream(PdfDict::from([(
                        b"Subtype".to_vec(),
                        PdfObject::Name(b"OpenType".to_vec()),
                    )])),
                ),
                (
                    object_ref(3),
                    PdfObject::Stream(PdfDict::from([(
                        b"Subtype".to_vec(),
                        PdfObject::Name(b"CIDFontType0C".to_vec()),
                    )])),
                ),
                (
                    object_ref(4),
                    PdfObject::Stream(PdfDict::from([(
                        b"Subtype".to_vec(),
                        PdfObject::Name(b"Type1C".to_vec()),
                    )])),
                ),
            ]),
            ..MockPdf::default()
        };
        let type1_with_font_file2 =
            font_with_program_reference(FontIdentityDomain::SimpleTrueTypeSymbolic, object_ref(1));
        let type1c_with_font_file =
            font_with_program_reference(FontIdentityDomain::SimpleType1BuiltIn, object_ref(1));
        let type1c_without_subtype =
            font_with_program_reference(FontIdentityDomain::SimpleType1C, object_ref(1));
        let type1c_with_wrong_subtype =
            font_with_program_reference(FontIdentityDomain::SimpleType1C, object_ref(2));
        let type1c_with_valid_subtype =
            font_with_program_reference(FontIdentityDomain::SimpleType1C, object_ref(4));
        let cid_without_subtype =
            font_with_program_reference(FontIdentityDomain::CidFontType0, object_ref(1));
        let cid_with_wrong_subtype =
            font_with_program_reference(FontIdentityDomain::CidFontType0, object_ref(2));
        let cid_with_valid_subtype =
            font_with_program_reference(FontIdentityDomain::CidFontType0, object_ref(3));

        assert!(
            resolve_font_identity_source(
                &pdf,
                &type1_with_font_file2,
                4,
                FontIdentityDomain::SimpleType1BuiltIn,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &type1c_with_font_file,
                4,
                FontIdentityDomain::SimpleType1C,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &type1c_without_subtype,
                4,
                FontIdentityDomain::SimpleType1C,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &type1c_with_wrong_subtype,
                4,
                FontIdentityDomain::SimpleType1C,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &type1c_with_valid_subtype,
                4,
                FontIdentityDomain::SimpleType1C,
            )?
            .is_some()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &cid_without_subtype,
                4,
                FontIdentityDomain::CidFontType0,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &cid_with_wrong_subtype,
                4,
                FontIdentityDomain::CidFontType0,
            )?
            .is_none()
        );
        assert!(
            resolve_font_identity_source(
                &pdf,
                &cid_with_valid_subtype,
                4,
                FontIdentityDomain::CidFontType0,
            )?
            .is_some()
        );
        Ok(())
    }

    #[test]
    fn classifies_direct_cycles_filters_and_limits_for_font_programs() {
        let direct = PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                b"FontFile".to_vec(),
                PdfObject::Stream(PdfDict::new()),
            )])),
        )]);
        assert!(matches!(
            resolve_font_identity_source(
                &MockPdf::default(),
                &direct,
                4,
                FontIdentityDomain::SimpleType1BuiltIn,
            ),
            Ok(None)
        ));

        let cyclic_pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Reference(object_ref(1)))]),
            ..MockPdf::default()
        };
        let indirect =
            font_with_program_reference(FontIdentityDomain::SimpleType1BuiltIn, object_ref(1));
        assert!(matches!(
            resolve_font_identity_source(
                &cyclic_pdf,
                &indirect,
                2,
                FontIdentityDomain::SimpleType1BuiltIn,
            ),
            Err(Error::LimitExceeded {
                resource: "font object indirections",
                limit: 2,
            })
        ));

        let stream_pdf = MockPdf {
            objects: HashMap::from([(object_ref(1), PdfObject::Stream(PdfDict::new()))]),
            streams: HashMap::from([(object_ref(1), b"font bytes".to_vec())]),
            unsupported_decoding: false,
            decoded_calls: AtomicUsize::new(0),
        };
        let source = resolve_font_identity_source(
            &stream_pdf,
            &indirect,
            4,
            FontIdentityDomain::SimpleType1BuiltIn,
        )
        .expect("fixture source should resolve")
        .expect("fixture should contain a font program");
        assert!(matches!(
            load_font_identity(&stream_pdf, &source, 4),
            Err(Error::LimitExceeded {
                resource: "decoded embedded font bytes",
                limit: 4,
            })
        ));

        let unsupported_pdf = MockPdf {
            unsupported_decoding: true,
            ..stream_pdf
        };
        assert!(matches!(
            load_font_identity(&unsupported_pdf, &source, 100),
            Err(Error::Unsupported(message)) if message.contains("font filter")
        ));
    }

    fn font_with_program_reference(domain: FontIdentityDomain, reference: ObjectRef) -> PdfDict {
        PdfDict::from([(
            b"FontDescriptor".to_vec(),
            PdfObject::Dictionary(PdfDict::from([(
                domain.program_key().to_vec(),
                PdfObject::Reference(reference),
            )])),
        )])
    }

    fn object_ref(object_number: u32) -> ObjectRef {
        ObjectRef {
            object_number,
            generation: 0,
        }
    }

    #[derive(Default)]
    struct MockPdf {
        objects: HashMap<ObjectRef, PdfObject>,
        streams: HashMap<ObjectRef, Vec<u8>>,
        unsupported_decoding: bool,
        decoded_calls: AtomicUsize,
    }

    impl ParsedPdf for MockPdf {
        fn version(&self) -> PdfVersion {
            PdfVersion { major: 1, minor: 7 }
        }

        fn trailer(&self) -> Result<PdfDict> {
            Ok(PdfDict::new())
        }

        fn resolve(&self, reference: ObjectRef) -> Result<PdfObject> {
            self.objects
                .get(&reference)
                .cloned()
                .ok_or_else(|| Error::Backend("missing mock object".into()))
        }

        fn pages(&self) -> Result<Vec<PageRef>> {
            Ok(Vec::new())
        }

        fn page_dict(&self, _page: PageRef) -> Result<PdfDict> {
            Err(Error::Backend("unused mock method".into()))
        }

        fn raw_stream(&self, _reference: ObjectRef) -> Result<RawStream> {
            Err(Error::Backend("unused mock method".into()))
        }

        fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream> {
            self.decoded_calls.fetch_add(1, Ordering::Relaxed);
            if self.unsupported_decoding {
                return Err(Error::Unsupported("unsupported font filter".into()));
            }
            Ok(DecodedStream {
                dictionary: PdfDict::new(),
                bytes: self
                    .streams
                    .get(&reference)
                    .cloned()
                    .ok_or_else(|| Error::Backend("missing mock stream".into()))?,
            })
        }
    }
}
