use crate::{Error, Result};

/// Decodes a PDF text string without deleting undefined bytes or padding an
/// incomplete UTF-16 code unit. Encoding markers do not become field content.
/// The input bytes remain the caller's original evidence on success or failure.
///
/// # Errors
/// Rejects malformed Unicode and undefined PDFDocEncoding entries, and enforces
/// the output byte limit before growing the decoded string.
pub fn decode_text_string(bytes: &[u8], max_output_bytes: usize) -> Result<String> {
    let mut output = String::new();
    let mut push = |character: char| -> Result<()> {
        if output.len().saturating_add(character.len_utf8()) > max_output_bytes {
            return Err(Error::LimitExceeded {
                resource: "PDF text string bytes",
                limit: max_output_bytes,
            });
        }
        output.push(character);
        Ok(())
    };
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        if !bytes.len().is_multiple_of(2) {
            return Err(Error::Unresolved(
                "PDF UTF-16BE text string has an incomplete code unit".into(),
            ));
        }
        for character in char::decode_utf16(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]])),
        ) {
            push(character.map_err(|_| {
                Error::Unresolved("PDF text string contains malformed UTF-16BE".into())
            })?)?;
        }
    } else if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        for character in std::str::from_utf8(bytes)
            .map_err(|_| Error::Unresolved("PDF text string contains malformed UTF-8".into()))?
            .chars()
        {
            push(character)?;
        }
    } else {
        for byte in bytes {
            let character = pdf_doc_encoding()[usize::from(*byte)].ok_or_else(|| {
                Error::Unresolved(format!("PDFDocEncoding byte {byte:02x} has no mapping"))
            })?;
            push(character)?;
        }
    }
    Ok(output)
}

fn pdf_doc_encoding() -> &'static [Option<char>; 256] {
    static MAPPING: std::sync::OnceLock<[Option<char>; 256]> = std::sync::OnceLock::new();
    MAPPING.get_or_init(|| {
        std::array::from_fn(|byte| {
            // Preserve undefined entries from the backend's public decoder without
            // exposing its object types or duplicating its character mapping table.
            let text = ::lopdf::decode_text_string(&::lopdf::Object::String(
                vec![byte as u8],
                ::lopdf::StringFormat::Literal,
            ))
            .ok()?;
            let mut characters = text.chars();
            let first = characters.next()?;
            characters.next().is_none().then_some(first)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_markers_are_not_content_and_malformed_units_are_not_repaired() {
        assert_eq!(
            decode_text_string(b"\xef\xbb\xbfvalue", 5).expect("UTF-8 field value"),
            "value"
        );
        assert_eq!(
            decode_text_string(&[0xfe, 0xff, 0x65, 0xe5], 3).expect("UTF-16BE Japanese value"),
            "日"
        );
        assert!(decode_text_string(&[0xfe, 0xff, 0x65], 10).is_err());
        assert!(decode_text_string(&[0xfe, 0xff, 0xd8, 0], 10).is_err());
        assert!(decode_text_string(&[0xef, 0xbb, 0xbf, 0xff], 10).is_err());
    }

    #[test]
    fn undefined_bytes_and_output_limits_are_explicit() {
        assert_eq!(
            decode_text_string(b"100", 3).expect("PDFDocEncoding number"),
            "100"
        );
        let undefined = pdf_doc_encoding()
            .iter()
            .position(Option::is_none)
            .expect("encoding includes undefined entries") as u8;
        assert!(matches!(
            decode_text_string(&[b'1', undefined, b'0'], 20),
            Err(Error::Unresolved(_))
        ));
        assert!(matches!(
            decode_text_string(b"100", 2),
            Err(Error::LimitExceeded { .. })
        ));
    }
}
