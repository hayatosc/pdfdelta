use hayro_cmap::CMap;

use super::*;

pub(super) const ENCODING_BYTES: &[u8] = include_bytes!("cmaps/UniJIS-UTF16-H");
pub(super) const UNICODE_BYTES: &[u8] = include_bytes!("cmaps/Adobe-Japan1-UCS2");
// Expanded CID and notdef entries plus the three codespaces in the pinned map.
pub(super) const ENCODING_ENTRIES: usize = 15_927;

#[derive(Clone, Debug)]
pub(super) enum Encoding {
    Identity(usize),
    UniJisUtf16(Box<CMap>),
}

impl Encoding {
    pub(super) fn fixed_width(&self) -> Option<usize> {
        match self {
            Self::Identity(width) => Some(*width),
            Self::UniJisUtf16(_) => None,
        }
    }

    pub(super) fn entry_count(&self) -> usize {
        match self {
            Self::Identity(_) => 0,
            Self::UniJisUtf16(_) => ENCODING_ENTRIES,
        }
    }

    pub(super) fn next_code(&self, input: &[u8]) -> Result<(usize, u16, bool)> {
        let width = match self {
            Self::Identity(width) => *width,
            Self::UniJisUtf16(_) => {
                let first = input
                    .get(..2)
                    .ok_or_else(|| Error::Unresolved("truncated UTF-16BE Type0 code".into()))?;
                let first = u16::from_be_bytes([first[0], first[1]]);
                match first {
                    0xd800..=0xdbff => {
                        let rest = input.get(2..4).ok_or_else(|| {
                            Error::Unresolved("truncated UTF-16BE surrogate pair".into())
                        })?;
                        if !(0xdc00..=0xdfff).contains(&u16::from_be_bytes([rest[0], rest[1]])) {
                            return unresolved("invalid UTF-16BE low surrogate");
                        }
                        4
                    }
                    0xdc00..=0xdfff => return unresolved("unpaired UTF-16BE low surrogate"),
                    _ => 2,
                }
            }
        };
        let source = input
            .get(..width)
            .ok_or_else(|| Error::Unresolved("truncated Type0 code".into()))?;
        let code = source
            .iter()
            .fold(0_u32, |value, byte| (value << 8) | u32::from(*byte));
        let cid = match self {
            Self::Identity(_) => code,
            Self::UniJisUtf16(cmap) => {
                cmap.lookup_cid_code(code, width as u8).ok_or_else(|| {
                    Error::Unresolved("UniJIS-UTF16-H code has no CID mapping".into())
                })?
            }
        };
        let cid = u16::try_from(cid)
            .map_err(|_| Error::Unresolved("Type0 CID exceeds the supported range".into()))?;
        // These codes select the map's notdef glyph, not textual spaces.
        let notdef = matches!(self, Self::UniJisUtf16(_)) && code < 0x20;
        Ok((width, cid, notdef))
    }
}

pub(super) fn load(limits: FontDecoderLimits) -> Result<Encoding> {
    if limits.cmap.max_code_bytes < 4 {
        return Err(Error::LimitExceeded {
            resource: "CMap source code bytes",
            limit: limits.cmap.max_code_bytes,
        });
    }
    if ENCODING_BYTES.len() > limits.max_decoded_font_bytes {
        return Err(Error::LimitExceeded {
            resource: "decoded Type0 Encoding bytes",
            limit: limits.max_decoded_font_bytes,
        });
    }
    if ENCODING_ENTRIES > limits.cmap.max_entries {
        return Err(Error::LimitExceeded {
            resource: "Type0 Encoding CMap entries",
            limit: limits.cmap.max_entries,
        });
    }
    // Only immutable, vendored bytes reach this parser; PDF-provided CMaps retain
    // the existing bounded parser. This resource has no usecmap dependency.
    let cmap = CMap::parse(ENCODING_BYTES, |_| None)
        .ok_or_else(|| Error::Unresolved("invalid bundled UniJIS-UTF16-H CMap".into()))?;
    Ok(Encoding::UniJisUtf16(Box::new(cmap)))
}

pub(super) fn collection_max_cid(
    pdf: &dyn ParsedPdf,
    descendant: &PdfDict,
    max_indirections: usize,
) -> Result<u16> {
    let info = descendant
        .get(b"CIDSystemInfo".as_slice())
        .ok_or_else(|| Error::Unresolved("UniJIS-UTF16-H requires CIDSystemInfo".into()))?;
    let PdfObject::Dictionary(info) = resolve_object(pdf, info.clone(), max_indirections)? else {
        return unresolved("CIDSystemInfo is not a dictionary");
    };
    for (key, expected) in [
        (b"Registry".as_slice(), b"Adobe".as_slice()),
        (b"Ordering".as_slice(), b"Japan1".as_slice()),
    ] {
        let value = info
            .get(key)
            .ok_or_else(|| Error::Unresolved("incomplete CIDSystemInfo".into()))?;
        if resolve_object(pdf, value.clone(), max_indirections)?
            != PdfObject::String(expected.to_vec())
        {
            return unresolved("UniJIS-UTF16-H requires the Adobe-Japan1 character collection");
        }
    }
    let supplement = info
        .get(b"Supplement".as_slice())
        .ok_or_else(|| Error::Unresolved("CIDSystemInfo has no Supplement".into()))?;
    let PdfObject::Integer(supplement) = resolve_object(pdf, supplement.clone(), max_indirections)?
    else {
        return unresolved("CIDSystemInfo Supplement is not an integer");
    };
    let index = usize::try_from(supplement).ok();
    index
        .and_then(|i| {
            [8283, 8358, 8719, 9353, 15443, 20316, 23057, 23059]
                .get(i)
                .copied()
        })
        .ok_or_else(|| {
            Error::Unsupported("Adobe-Japan1 supplement is outside the bundled collection".into())
        })
}
