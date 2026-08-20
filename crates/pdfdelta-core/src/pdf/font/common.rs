use crate::{
    Error, Result,
    model::FontProgramHash,
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject},
};
use sha2::{Digest, Sha256};

use super::{
    cmap::{ToUnicodeCMap, parse_to_unicode},
    decoder::FontDecoderLimits,
};

pub(super) fn load_to_unicode(
    pdf: &dyn ParsedPdf,
    dictionary: &PdfDict,
    limits: FontDecoderLimits,
) -> Result<(Option<ToUnicodeCMap>, usize)> {
    let Some(to_unicode) = dictionary.get(b"ToUnicode".as_slice()) else {
        return Ok((None, 0));
    };
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
    let cmap = parse_to_unicode(&stream.bytes, limits.cmap)?;
    Ok((Some(cmap), byte_count))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FontIdentityDomain {
    SimpleType1BuiltIn,
    SimpleTrueTypeSymbolic,
    SimpleTrueTypeNonsymbolic,
    CidFontType0,
    CidFontType2Identity,
}

impl FontIdentityDomain {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::SimpleType1BuiltIn => b"simple-type1-built-in",
            Self::SimpleTrueTypeSymbolic => b"simple-truetype-symbolic",
            Self::SimpleTrueTypeNonsymbolic => b"simple-truetype-nonsymbolic",
            Self::CidFontType0 => b"cidfont-type0-cid",
            Self::CidFontType2Identity => b"cidfont-type2-identity",
        }
    }

    fn program_key(self) -> &'static [u8] {
        match self {
            Self::SimpleType1BuiltIn => b"FontFile",
            Self::SimpleTrueTypeSymbolic | Self::SimpleTrueTypeNonsymbolic => b"FontFile2",
            Self::CidFontType0 => b"FontFile3",
            Self::CidFontType2Identity => b"FontFile2",
        }
    }

    fn font_file3_subtype(self) -> Option<&'static [u8]> {
        match self {
            Self::CidFontType0 => Some(b"CIDFontType0C"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct FontIdentitySource {
    reference: ObjectRef,
    domain: FontIdentityDomain,
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
    let PdfObject::Dictionary(descriptor) = descriptor else {
        return unresolved("FontDescriptor is not a dictionary");
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
    Ok(Some(FontIdentitySource { reference, domain }))
}

pub(crate) fn load_font_identity(
    pdf: &dyn ParsedPdf,
    source: &FontIdentitySource,
    max_decoded_bytes: usize,
) -> Result<LoadedFontIdentity> {
    let reference = source.reference;
    let stream = pdf.decoded_stream(reference)?;
    if stream.bytes.len() > max_decoded_bytes {
        return Err(Error::LimitExceeded {
            resource: "decoded embedded font bytes",
            limit: max_decoded_bytes,
        });
    }
    let decoded_bytes = stream.bytes.len();
    let mut digest = Sha256::new();
    digest.update(b"pdfdelta-font-glyph-identity\0v1\0");
    digest.update(source.domain.tag());
    digest.update(b"\0");
    digest.update(&stream.bytes);
    let hash = FontProgramHash(digest.finalize().to_vec());
    Ok(LoadedFontIdentity {
        hash,
        decoded_bytes,
    })
}

fn resolve_stream_reference(
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

pub(super) fn optional_number(dictionary: &PdfDict, key: &[u8]) -> Result<Option<f64>> {
    dictionary
        .get(key)
        .map(|value| finite_number(value, "font metric"))
        .transpose()
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

fn unresolved<T>(message: &str) -> Result<T> {
    Err(Error::Unresolved(message.into()))
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
            objects: HashMap::from([(
                object_ref(1),
                PdfObject::Stream(PdfDict::from([(
                    b"Subtype".to_vec(),
                    PdfObject::Name(b"CIDFontType0C".to_vec()),
                )])),
            )]),
            streams: HashMap::from([(object_ref(1), b"same program".to_vec())]),
            ..MockPdf::default()
        };
        let mut hashes = Vec::new();
        for domain in [
            FontIdentityDomain::SimpleType1BuiltIn,
            FontIdentityDomain::SimpleTrueTypeSymbolic,
            FontIdentityDomain::SimpleTrueTypeNonsymbolic,
            FontIdentityDomain::CidFontType0,
            FontIdentityDomain::CidFontType2Identity,
        ] {
            let font = font_with_program_reference(domain, object_ref(1));
            let source = resolve_font_identity_source(&pdf, &font, 4, domain)?
                .ok_or_else(|| Error::Unresolved("fixture should contain a font program".into()))?;
            hashes.push(load_font_identity(&pdf, &source, usize::MAX)?.hash);
        }
        hashes.sort();
        hashes.dedup();

        assert_eq!(hashes.len(), 5);
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
            ]),
            ..MockPdf::default()
        };
        let type1_with_font_file2 =
            font_with_program_reference(FontIdentityDomain::SimpleTrueTypeSymbolic, object_ref(1));
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
