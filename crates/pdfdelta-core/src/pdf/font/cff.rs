//! Minimal bounded reader for CFF built-in encodings.
//!
//! `Type1C` (`FontFile3`/`Type1C`) programs carry an optional built-in encoding
//! and a charset that maps every glyph id to a glyph name. A PDF that does not
//! give the font an `/Encoding` and has no `ToUnicode` entry for a code can
//! still recover that code's text from the program's own encoding: the code
//! selects a glyph id, the charset names the glyph, and the Adobe Glyph List
//! resolves the name. A CID-keyed program instead stores CIDs in the charset;
//! its `CharStrings` entries can identify a glyph by outline when no name exists.
//! Only the header, INDEXes, Top DICT, charset, Encoding, String INDEX, and
//! `CharStrings` slices are read; charstring operators are never interpreted.
//! Every offset and count is bounds-checked because the program is untrusted
//! input.

use std::collections::BTreeMap;

/// The 391 CFF standard strings (Adobe CFF specification, Appendix A).
const STANDARD_STRINGS: [&str; 391] = [
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "aacute",
    "acircumflex",
    "adieresis",
    "agrave",
    "aring",
    "atilde",
    "ccedilla",
    "eacute",
    "ecircumflex",
    "edieresis",
    "egrave",
    "iacute",
    "icircumflex",
    "idieresis",
    "igrave",
    "ntilde",
    "oacute",
    "ocircumflex",
    "odieresis",
    "ograve",
    "otilde",
    "scaron",
    "uacute",
    "ucircumflex",
    "udieresis",
    "ugrave",
    "yacute",
    "ydieresis",
    "zcaron",
    "exclamsmall",
    "Hungarumlautsmall",
    "dollaroldstyle",
    "dollarsuperior",
    "ampersandsmall",
    "Acutesmall",
    "parenleftsuperior",
    "parenrightsuperior",
    "twodotenleader",
    "onedotenleader",
    "zerooldstyle",
    "oneoldstyle",
    "twooldstyle",
    "threeoldstyle",
    "fouroldstyle",
    "fiveoldstyle",
    "sixoldstyle",
    "sevenoldstyle",
    "eightoldstyle",
    "nineoldstyle",
    "commasuperior",
    "threequartersemdash",
    "periodsuperior",
    "questionsmall",
    "asuperior",
    "bsuperior",
    "centsuperior",
    "dsuperior",
    "esuperior",
    "isuperior",
    "lsuperior",
    "msuperior",
    "nsuperior",
    "osuperior",
    "rsuperior",
    "ssuperior",
    "tsuperior",
    "ff",
    "ffi",
    "ffl",
    "parenleftinferior",
    "parenrightinferior",
    "Circumflexsmall",
    "hyphensuperior",
    "Gravesmall",
    "Asmall",
    "Bsmall",
    "Csmall",
    "Dsmall",
    "Esmall",
    "Fsmall",
    "Gsmall",
    "Hsmall",
    "Ismall",
    "Jsmall",
    "Ksmall",
    "Lsmall",
    "Msmall",
    "Nsmall",
    "Osmall",
    "Psmall",
    "Qsmall",
    "Rsmall",
    "Ssmall",
    "Tsmall",
    "Usmall",
    "Vsmall",
    "Wsmall",
    "Xsmall",
    "Ysmall",
    "Zsmall",
    "colonmonetary",
    "onefitted",
    "rupiah",
    "Tildesmall",
    "exclamdownsmall",
    "centoldstyle",
    "Lslashsmall",
    "Scaronsmall",
    "Zcaronsmall",
    "Dieresissmall",
    "Brevesmall",
    "Caronsmall",
    "Dotaccentsmall",
    "Macronsmall",
    "figuredash",
    "hypheninferior",
    "Ogoneksmall",
    "Ringsmall",
    "Cedillasmall",
    "questiondownsmall",
    "oneeighth",
    "threeeighths",
    "fiveeighths",
    "seveneighths",
    "onethird",
    "twothirds",
    "zerosuperior",
    "foursuperior",
    "fivesuperior",
    "sixsuperior",
    "sevensuperior",
    "eightsuperior",
    "ninesuperior",
    "zeroinferior",
    "oneinferior",
    "twoinferior",
    "threeinferior",
    "fourinferior",
    "fiveinferior",
    "sixinferior",
    "seveninferior",
    "eightinferior",
    "nineinferior",
    "centinferior",
    "dollarinferior",
    "periodinferior",
    "commainferior",
    "Agravesmall",
    "Aacutesmall",
    "Acircumflexsmall",
    "Atildesmall",
    "Adieresissmall",
    "Aringsmall",
    "AEsmall",
    "Ccedillasmall",
    "Egravesmall",
    "Eacutesmall",
    "Ecircumflexsmall",
    "Edieresissmall",
    "Igravesmall",
    "Iacutesmall",
    "Icircumflexsmall",
    "Idieresissmall",
    "Ethsmall",
    "Ntildesmall",
    "Ogravesmall",
    "Oacutesmall",
    "Ocircumflexsmall",
    "Otildesmall",
    "Odieresissmall",
    "OEsmall",
    "Oslashsmall",
    "Ugravesmall",
    "Uacutesmall",
    "Ucircumflexsmall",
    "Udieresissmall",
    "Yacutesmall",
    "Thornsmall",
    "Ydieresissmall",
    "001.000",
    "001.001",
    "001.002",
    "001.003",
    "Black",
    "Bold",
    "Book",
    "Light",
    "Medium",
    "Regular",
    "Roman",
    "Semibold",
];

/// A CFF program's built-in encoding, or the `StandardEncoding` default.
pub(crate) struct BuiltInEncoding {
    /// The Top DICT has no Encoding operator, so the built-in encoding is the
    /// PostScript `StandardEncoding` rather than a code-to-glyph table.
    pub(crate) standard: bool,
    /// Code to glyph name pairs from the custom Encoding and charset.
    pub(crate) differences: Vec<(u8, Vec<u8>)>,
}

/// Reads the built-in encoding of a `Type1C` program.
///
/// Returns `None` when the program cannot be parsed within the bounds below;
/// callers then keep the code unknown rather than guessing a mapping.
pub(crate) fn built_in_encoding(data: &[u8]) -> Option<BuiltInEncoding> {
    if data.len() < 4 {
        return None;
    }
    // Header: major, minor, header size, offset size.
    let header_size = usize::from(data[2]);
    let (_, position) = read_index(data, header_size)?; // Name INDEX
    let (top_items, position) = read_index(data, position)?; // Top DICT INDEX
    let (string_items, _) = read_index(data, position)?; // String INDEX
    let (top_start, top_end) = *top_items.first()?;
    let top = read_dict(&data[top_start..top_end])?;
    let charstrings = usize::try_from(top.charstrings?).ok()?;
    let (charstrings_items, _) = read_index(data, charstrings)?;
    let glyph_count = charstrings_items.len();
    if glyph_count == 0 || glyph_count > usize::from(u16::MAX) {
        return None;
    }
    let Some(encoding_offset) = top.encoding.filter(|offset| *offset != 0) else {
        return Some(BuiltInEncoding {
            standard: true,
            differences: Vec::new(),
        });
    };
    let charset_offset = usize::try_from(top.charset.unwrap_or(0)).ok()?;
    let charset = read_charset(data, charset_offset, glyph_count)?;
    let (code_to_glyph, supplements) =
        read_encoding(data, usize::try_from(encoding_offset).ok()?, glyph_count)?;
    let mut differences = Vec::new();
    for code in 0u16..=255 {
        let glyph = usize::from(code_to_glyph[usize::from(code)]);
        let mut sid = charset.get(glyph).copied().unwrap_or(0);
        for (supplement_code, supplement_sid) in &supplements {
            if u16::from(*supplement_code) == code {
                sid = *supplement_sid;
            }
        }
        if sid == 0 {
            continue;
        }
        let name = if usize::from(sid) < STANDARD_STRINGS.len() {
            STANDARD_STRINGS[usize::from(sid)].as_bytes().to_vec()
        } else {
            let index = usize::from(sid) - STANDARD_STRINGS.len();
            let (start, end) = *string_items.get(index)?;
            data[start..end].to_vec()
        };
        // An empty or missing name is not usable text; leave the code unknown.
        if !name.is_empty() && name != b".notdef" {
            differences.push((u8::try_from(code).ok()?, name));
        }
    }
    Some(BuiltInEncoding {
        standard: false,
        differences,
    })
}

/// Top DICT values this reader needs.
#[derive(Default)]
struct TopDict {
    charset: Option<i32>,
    encoding: Option<i32>,
    charstrings: Option<i32>,
    /// The `ROS` operator marks a CID-keyed program whose charset values are
    /// CIDs rather than glyph-name SIDs.
    is_cid: bool,
}

/// A CID-keyed CFF program's `CharStrings`, addressable by CID.
///
/// The program bytes stay alive while the caller computes one outline
/// identity per CID, then the caller drops this map and keeps only the compact
/// identities. A CID that occurs twice in the charset is ambiguous and rejects
/// the whole map instead of choosing one.
pub(crate) struct CidCharStrings {
    data: Vec<u8>,
    charstrings: Vec<(usize, usize)>,
    cid_to_gid: BTreeMap<u16, u16>,
}

impl CidCharStrings {
    /// Iterates every CID with its exact `CharStrings` INDEX entry.
    ///
    /// An empty entry means the glyph has no drawing; callers must not use it
    /// as outline evidence.
    pub(crate) fn iter_outlines(&self) -> impl Iterator<Item = (u16, &[u8])> {
        self.cid_to_gid.iter().filter_map(|(cid, glyph)| {
            let (start, end) = *self.charstrings.get(usize::from(*glyph))?;
            Some((*cid, self.data.get(start..end)?))
        })
    }

    /// Number of CIDs with a charset entry.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.cid_to_gid.len()
    }
}

/// Reads a CID-keyed CFF program's CID-to-CharStrings mapping.
///
/// Returns `None` for a non-CID program, a missing or default charset, a
/// malformed index, or a duplicate CID. The caller keeps the program-level
/// identity in those cases rather than guessing an outline.
pub(crate) fn cid_charstrings(data: &[u8]) -> Option<CidCharStrings> {
    if data.len() < 4 {
        return None;
    }
    let header_size = usize::from(data[2]);
    let (_, position) = read_index(data, header_size)?; // Name INDEX
    let (top_items, position) = read_index(data, position)?; // Top DICT INDEX
    let (_, _) = read_index(data, position)?; // String INDEX
    let (top_start, top_end) = *top_items.first()?;
    let top = read_dict(&data[top_start..top_end])?;
    if !top.is_cid {
        return None;
    }
    // CID-keyed charsets are explicit; the ISOAdobe default cannot name CIDs.
    let charset_offset = top.charset.filter(|offset| *offset != 0)?;
    let charstrings_offset = usize::try_from(top.charstrings?).ok()?;
    let (charstrings, _) = read_index(data, charstrings_offset)?;
    let glyph_count = charstrings.len();
    if glyph_count == 0 || glyph_count > usize::from(u16::MAX) {
        return None;
    }
    let charset = read_charset(data, usize::try_from(charset_offset).ok()?, glyph_count)?;
    let mut cid_to_gid = BTreeMap::new();
    for (glyph, cid) in charset.into_iter().enumerate() {
        let glyph = u16::try_from(glyph).ok()?;
        if cid_to_gid.insert(cid, glyph).is_some() {
            // Two glyphs claim one CID, so no unique outline exists for it.
            return None;
        }
    }
    Some(CidCharStrings {
        data: data.to_vec(),
        charstrings,
        cid_to_gid,
    })
}

/// Reads the charset SID list, indexed by glyph id.
fn read_charset(data: &[u8], offset: usize, glyph_count: usize) -> Option<Vec<u16>> {
    if offset == 0 {
        // The default ISOAdobe charset assigns SIDs to glyph ids in order and
        // defines at most 229 glyphs.
        if glyph_count > 229 {
            return None;
        }
        return (0..glyph_count)
            .map(|glyph| u16::try_from(glyph).ok())
            .collect();
    }
    let format = *data.get(offset)?;
    let mut sids = Vec::with_capacity(glyph_count);
    sids.push(0); // glyph 0 is .notdef
    let mut position = offset + 1;
    match format {
        0 => {
            while sids.len() < glyph_count {
                sids.push(read_u16(data, position)?);
                position += 2;
            }
        }
        1 => {
            while sids.len() < glyph_count {
                let first = read_u16(data, position)?;
                position += 2;
                let left = u16::from(*data.get(position)?);
                position += 1;
                for offset in 0..=left {
                    if sids.len() < glyph_count {
                        sids.push(first.wrapping_add(offset));
                    }
                }
            }
        }
        2 => {
            while sids.len() < glyph_count {
                let first = read_u16(data, position)?;
                position += 2;
                let left = read_u16(data, position)?;
                position += 2;
                for offset in 0..=left {
                    if sids.len() < glyph_count {
                        sids.push(first.wrapping_add(offset));
                    }
                }
            }
        }
        _ => return None,
    }
    Some(sids)
}

/// Code-to-glyph ids and the supplement code-to-SID overrides of one Encoding.
type EncodingTable = (Vec<u16>, Vec<(u8, u16)>);

/// Reads a custom Encoding: code-to-glyph ids plus supplement SIDs per code.
fn read_encoding(data: &[u8], offset: usize, glyph_count: usize) -> Option<EncodingTable> {
    let first = *data.get(offset)?;
    let format = first & 0x7f;
    let mut code_to_glyph = vec![0u16; 256];
    let mut position = offset + 1;
    match format {
        0 => {
            let count = usize::from(*data.get(position)?);
            position += 1;
            for glyph in 1..=count {
                let code = usize::from(*data.get(position)?);
                position += 1;
                if code < code_to_glyph.len() && glyph <= glyph_count {
                    code_to_glyph[code] = u16::try_from(glyph).ok()?;
                }
            }
        }
        1 => {
            let ranges = usize::from(*data.get(position)?);
            position += 1;
            let mut glyph = 1usize;
            for _ in 0..ranges {
                let first_code = usize::from(*data.get(position)?);
                position += 1;
                let left = usize::from(*data.get(position)?);
                position += 1;
                for code in first_code..=first_code.saturating_add(left) {
                    if code < code_to_glyph.len() && glyph <= glyph_count {
                        code_to_glyph[code] = u16::try_from(glyph).ok()?;
                    }
                    glyph = glyph.saturating_add(1);
                }
            }
        }
        _ => return None,
    }
    let mut supplements = Vec::new();
    if first & 0x80 != 0 {
        let count = usize::from(*data.get(position)?);
        position += 1;
        for _ in 0..count {
            let code = *data.get(position)?;
            position += 1;
            let sid = read_u16(data, position)?;
            position += 2;
            supplements.push((code, sid));
        }
    }
    Some((code_to_glyph, supplements))
}

/// Reads the operators and integer operands of a Top DICT.
fn read_dict(dict: &[u8]) -> Option<TopDict> {
    let mut top = TopDict::default();
    let mut operands: Vec<i32> = Vec::new();
    let mut position = 0;
    while position < dict.len() {
        let first = dict[position];
        match first {
            32..=246 => {
                operands.push(i32::from(first) - 139);
                position += 1;
            }
            247..=250 => {
                let second = i32::from(*dict.get(position + 1)?);
                operands.push((i32::from(first) - 247) * 256 + second + 108);
                position += 2;
            }
            251..=254 => {
                let second = i32::from(*dict.get(position + 1)?);
                operands.push(-(i32::from(first) - 251) * 256 - second - 108);
                position += 2;
            }
            28 => {
                let value =
                    i16::from_be_bytes([*dict.get(position + 1)?, *dict.get(position + 2)?]);
                operands.push(i32::from(value));
                position += 3;
            }
            29 => {
                let value = i32::from_be_bytes([
                    *dict.get(position + 1)?,
                    *dict.get(position + 2)?,
                    *dict.get(position + 3)?,
                    *dict.get(position + 4)?,
                ]);
                operands.push(value);
                position += 5;
            }
            30 => {
                // Real number: skip nibbles until the terminator nibble.
                position += 1;
                loop {
                    let byte = *dict.get(position)?;
                    position += 1;
                    if byte & 0x0f == 0x0f || byte >> 4 == 0x0f {
                        break;
                    }
                }
                operands.push(0);
            }
            0..=21 => {
                let operator = if first == 12 {
                    position += 1;
                    1200 + i32::from(*dict.get(position)?)
                } else {
                    i32::from(first)
                };
                position += 1;
                let value = operands.last().copied();
                match operator {
                    15 => top.charset = value,
                    16 => top.encoding = value,
                    17 => top.charstrings = value,
                    1230 => top.is_cid = true, // ROS
                    _ => {}
                }
                operands.clear();
            }
            _ => return None,
        }
    }
    Some(top)
}

/// Reads a CFF INDEX and returns the item bounds plus the position after it.
fn read_index(data: &[u8], position: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    let count = usize::from(read_u16(data, position)?);
    let mut position = position + 2;
    if count == 0 {
        return Some((Vec::new(), position));
    }
    let offset_size = usize::from(*data.get(position)?);
    position += 1;
    if !(1..=4).contains(&offset_size) || count > usize::from(u16::MAX) {
        return None;
    }
    let mut offsets = Vec::with_capacity(count + 1);
    for _ in 0..=count {
        offsets.push(read_offset(data, position, offset_size)?);
        position += offset_size;
    }
    // Offsets are one-based from the byte before the data area and must not
    // run backwards.
    if offsets.first() != Some(&1) || offsets.windows(2).any(|pair| pair[1] < pair[0]) {
        return None;
    }
    let base = position.checked_sub(1)?;
    let mut items = Vec::with_capacity(count);
    for index in 0..count {
        let start = base.checked_add(usize::try_from(offsets[index]).ok()?)?;
        let end = base.checked_add(usize::try_from(offsets[index + 1]).ok()?)?;
        if start > end || end > data.len() {
            return None;
        }
        items.push((start, end));
    }
    let end = base.checked_add(usize::try_from(*offsets.last()?).ok()?)?;
    if end > data.len() {
        return None;
    }
    Some((items, end))
}

fn read_u16(data: &[u8], position: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *data.get(position)?,
        *data.get(position + 1)?,
    ]))
}

fn read_offset(data: &[u8], position: usize, size: usize) -> Option<u32> {
    let mut value = 0u32;
    for byte in data.get(position..position.checked_add(size)?)? {
        value = value.checked_mul(256)?.checked_add(u32::from(*byte))?;
    }
    Some(value)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a CFF INDEX with one-byte offsets.
    fn build_index(items: &[&[u8]]) -> Vec<u8> {
        let mut data = Vec::new();
        let mut offsets = vec![1u8];
        for item in items {
            data.extend_from_slice(item);
            offsets.push(u8::try_from(data.len() + 1).expect("fixture offsets fit one byte"));
        }
        let mut index = Vec::new();
        index.extend_from_slice(
            &u16::try_from(items.len())
                .expect("fixture count fits u16")
                .to_be_bytes(),
        );
        index.push(1); // offSize
        index.extend_from_slice(&offsets);
        index.extend_from_slice(&data);
        index
    }

    /// Encodes one DICT integer operand in the 28 (shortint) form.
    fn dict_int(value: usize) -> [u8; 3] {
        let value = u16::try_from(value).expect("fixture offset fits u16");
        [28, (value >> 8) as u8, value as u8]
    }

    /// A five-glyph `Type1C` fixture: `.notdef`, `quotesingle`, `comma`,
    /// `asciigrave`, `arrowhookleft`, with a custom code-to-glyph Encoding.
    pub(crate) fn fixture_program(include_encoding: bool) -> Vec<u8> {
        let header = [1, 0, 4, 4];
        let name_index = build_index(&[b"TestFont".as_slice()]);
        let dict_len: usize = if include_encoding { 12 } else { 8 };
        let top_index_len = 2 + 1 + 2 + dict_len;
        let string_index = build_index(&[b"asciigrave".as_slice(), b"arrowhookleft"]);
        let charset = [
            0x00, 0x00, 0x68, // quotesingle, SID 104
            0x00, 0x0d, // comma, SID 13
            0x01, 0x87, // asciigrave, SID 391
            0x01, 0x88, // arrowhookleft, SID 392
        ];
        let encoding = [0x00, 4, 0x27, 0x2c, 0x60, 0x61];
        let charset_offset = header.len() + name_index.len() + top_index_len + string_index.len();
        let encoding_offset = charset_offset + charset.len();
        let charstrings_offset = encoding_offset + encoding.len();
        let glyph = [14u8];
        let charstrings = build_index(&[glyph.as_slice(); 5]);
        let mut dict = Vec::new();
        dict.extend_from_slice(&dict_int(charset_offset));
        dict.push(15); // charset
        if include_encoding {
            dict.extend_from_slice(&dict_int(encoding_offset));
            dict.push(16); // Encoding
        }
        dict.extend_from_slice(&dict_int(charstrings_offset));
        dict.push(17); // CharStrings
        assert_eq!(dict.len(), dict_len);
        let top_index = build_index(&[dict.as_slice()]);
        let mut program = Vec::new();
        program.extend_from_slice(&header);
        program.extend_from_slice(&name_index);
        program.extend_from_slice(&top_index);
        program.extend_from_slice(&string_index);
        program.extend_from_slice(&charset);
        program.extend_from_slice(&encoding);
        program.extend_from_slice(&charstrings);
        program
    }

    /// A four-glyph CID-keyed fixture (`ROS` present, charset values are CIDs).
    fn fixture_cid_program() -> Vec<u8> {
        let header = [1, 0, 4, 4];
        let name_index = build_index(&[b"TestCidFont".as_slice()]);
        let top_index_len = 2 + 1 + 2 + 13;
        let string_index = build_index(&[]);
        let charset = [0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03];
        let charset_offset = header.len() + name_index.len() + top_index_len + string_index.len();
        let charstrings_offset = charset_offset + charset.len();
        let empty = [0u8; 0];
        let first = [14u8];
        let second = [5u8, 14];
        let charstrings = build_index(&[
            empty.as_slice(),
            first.as_slice(),
            second.as_slice(),
            first.as_slice(),
        ]);
        let mut dict = Vec::new();
        dict.extend_from_slice(&dict_int(charset_offset));
        dict.push(15); // charset
        dict.extend_from_slice(&dict_int(charstrings_offset));
        dict.push(17); // CharStrings
        dict.extend_from_slice(&[139, 139, 139, 12, 30]); // ROS 0 0 0
        assert_eq!(dict.len(), 13);
        let top_index = build_index(&[dict.as_slice()]);
        let mut program = Vec::new();
        program.extend_from_slice(&header);
        program.extend_from_slice(&name_index);
        program.extend_from_slice(&top_index);
        program.extend_from_slice(&string_index);
        program.extend_from_slice(&charset);
        program.extend_from_slice(&charstrings);
        program
    }

    #[test]
    fn reads_cid_charstrings_and_rejects_non_cid_or_truncated_programs() {
        let program = fixture_cid_program();
        let charstrings = cid_charstrings(&program).expect("parsable CID fixture");
        assert_eq!(charstrings.len(), 4);
        let outlines = charstrings.iter_outlines().collect::<Vec<_>>();
        assert_eq!(outlines[0], (0, [].as_slice()));
        assert_eq!(outlines[1], (1, [14].as_slice()));
        assert_eq!(outlines[2], (2, [5, 14].as_slice()));
        assert_eq!(outlines[3], (3, [14].as_slice()));

        assert!(cid_charstrings(&fixture_program(true)).is_none());
        for length in [0, 3, program.len() / 2, program.len() - 1] {
            assert!(cid_charstrings(&program[..length]).is_none());
        }
    }

    #[test]
    fn reads_the_custom_encoding_and_charset_names() {
        let encoding = built_in_encoding(&fixture_program(true)).expect("parsable fixture");
        assert!(!encoding.standard);
        assert_eq!(
            encoding.differences,
            [
                (0x27, b"quotesingle".to_vec()),
                (0x2c, b"comma".to_vec()),
                (0x60, b"asciigrave".to_vec()),
                (0x61, b"arrowhookleft".to_vec()),
            ]
        );
    }

    #[test]
    fn a_missing_encoding_operator_selects_standard_encoding() {
        let encoding = built_in_encoding(&fixture_program(false)).expect("parsable fixture");
        assert!(encoding.standard);
        assert!(encoding.differences.is_empty());
    }

    #[test]
    fn a_truncated_program_is_rejected_without_a_guess() {
        let program = fixture_program(true);
        for length in [0, 3, 8, program.len() / 2, program.len() - 1] {
            assert!(
                built_in_encoding(&program[..length]).is_none(),
                "truncation at {length} must not produce a mapping"
            );
        }
    }
}
