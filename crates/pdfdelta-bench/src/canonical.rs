use std::collections::HashSet;

use crate::{BenchError, Result};

pub const MAX_PARAGRAPHS: usize = 32;
pub const MAX_PARAGRAPH_ID_BYTES: usize = 64;
pub const MAX_PARAGRAPH_TEXT_BYTES: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    id: String,
    text: String,
}

impl Paragraph {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Result<Self> {
        let id = id.into();
        let text = text.into();
        validate_paragraph_id(&id)?;
        validate_text(&text)?;
        Ok(Self { id, text })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalDocument {
    paragraphs: Vec<Paragraph>,
}

impl CanonicalDocument {
    pub fn new(paragraphs: Vec<Paragraph>) -> Result<Self> {
        if paragraphs.is_empty() {
            return Err(BenchError::InvalidInput(
                "canonical documents require at least one paragraph".to_owned(),
            ));
        }
        if paragraphs.len() > MAX_PARAGRAPHS {
            return Err(BenchError::InvalidInput(format!(
                "canonical documents support at most {MAX_PARAGRAPHS} paragraphs"
            )));
        }

        let mut ids = HashSet::with_capacity(paragraphs.len());
        for paragraph in &paragraphs {
            if !ids.insert(paragraph.id()) {
                return Err(BenchError::InvalidInput(format!(
                    "duplicate paragraph id {:?}",
                    paragraph.id()
                )));
            }
        }
        Ok(Self { paragraphs })
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }
}

pub(crate) fn validate_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(BenchError::InvalidInput(
            "paragraph and line text must not be blank".to_owned(),
        ));
    }
    if text.len() > MAX_PARAGRAPH_TEXT_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "paragraph and line text must not exceed {MAX_PARAGRAPH_TEXT_BYTES} bytes"
        )));
    }
    if text.trim() != text {
        return Err(BenchError::InvalidInput(
            "paragraph and line text must not have leading or trailing whitespace".to_owned(),
        ));
    }
    if !text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        return Err(BenchError::InvalidInput(
            "paragraph and line text must use printable ASCII".to_owned(),
        ));
    }
    if text.as_bytes().windows(2).any(|pair| pair == b"  ") {
        return Err(BenchError::InvalidInput(
            "paragraph and line text must use single spaces".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_paragraph_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(BenchError::InvalidInput(
            "paragraph ids must not be blank".to_owned(),
        ));
    }
    if id.len() > MAX_PARAGRAPH_ID_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "paragraph ids must not exceed {MAX_PARAGRAPH_ID_BYTES} bytes"
        )));
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(BenchError::InvalidInput(
            "paragraph ids may contain only ASCII letters, digits, '-' and '_'".to_owned(),
        ));
    }
    Ok(())
}
