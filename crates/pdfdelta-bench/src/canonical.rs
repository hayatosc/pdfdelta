use std::collections::HashSet;

use serde::Deserialize;

use crate::{BenchError, Result};

pub const MAX_PARAGRAPHS: usize = 32;
pub const MAX_PARAGRAPH_ID_BYTES: usize = 64;
pub const MAX_PARAGRAPH_TEXT_BYTES: usize = 512;
pub const MAX_CANONICAL_YAML_BYTES: usize = 16 * 1024;
pub const MAX_CANONICAL_SECTIONS: usize = 8;
pub const MAX_CANONICAL_RENDER_LINES: usize = 32;

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

/// A structured canonical document used to render file-backed fixtures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalRenderDocument {
    title: String,
    sections: Vec<CanonicalSection>,
}

/// A titled group of canonical paragraphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalSection {
    id: String,
    heading: String,
    paragraphs: Vec<Paragraph>,
}

impl CanonicalRenderDocument {
    /// Parses one bounded YAML document and validates its renderable content.
    ///
    /// The parser is configured with tight budgets to prevent resource amplification
    /// from aliases, anchors, or deeply nested structures. Tags, aliases, anchors,
    /// merge keys, and multiple documents are rejected explicitly because the
    /// canonical fixture format does not require them.
    ///
    /// # Errors
    ///
    /// Returns [`BenchError::InvalidInput`] when the YAML syntax, schema,
    /// content, identifiers, or configured fixture bounds are invalid.
    pub fn from_yaml(yaml: &str) -> Result<Self> {
        if yaml.len() > MAX_CANONICAL_YAML_BYTES {
            return Err(BenchError::InvalidInput(format!(
                "canonical YAML must not exceed {MAX_CANONICAL_YAML_BYTES} bytes"
            )));
        }
        reject_disallowed_yaml_syntax(yaml)?;

        let options = serde_saphyr::options! {
            budget: serde_saphyr::budget! {
                max_documents: 1,
                max_nodes: 512,
                max_events: 2048,
                max_depth: 32,
                max_aliases: 0,
                max_anchors: 0,
                max_total_scalar_bytes: MAX_CANONICAL_YAML_BYTES,
                max_total_comment_bytes: 1024,
                max_merge_keys: 0
            },
            alias_limits: serde_saphyr::alias_limits! {
                max_total_replayed_events: 0,
                max_replay_stack_depth: 0,
                max_alias_expansions_per_anchor: 0
            },
            merge_keys: serde_saphyr::options::MergeKeyPolicy::Error,
            duplicate_keys: serde_saphyr::options::DuplicateKeyPolicy::Error
        };
        let root =
            serde_saphyr::from_str_with_options::<YamlRoot>(yaml, options).map_err(|error| {
                BenchError::InvalidInput(format!("cannot parse canonical YAML: {error}"))
            })?;
        Self::from_yaml_document(root.document)
    }

    fn from_yaml_document(document: YamlDocument) -> Result<Self> {
        validate_named_text("document title", &document.title)?;
        if document.sections.is_empty() {
            return Err(BenchError::InvalidInput(
                "canonical YAML requires at least one section".to_owned(),
            ));
        }
        if document.sections.len() > MAX_CANONICAL_SECTIONS {
            return Err(BenchError::InvalidInput(format!(
                "canonical YAML supports at most {MAX_CANONICAL_SECTIONS} sections"
            )));
        }

        let mut section_ids = HashSet::with_capacity(document.sections.len());
        let mut paragraph_ids = HashSet::new();
        let mut paragraph_count = 0_usize;
        let mut sections = Vec::with_capacity(document.sections.len());
        for section in document.sections {
            validate_named_id("section ids", &section.id)?;
            if !section_ids.insert(section.id.clone()) {
                return Err(BenchError::InvalidInput(format!(
                    "duplicate section id {:?}",
                    section.id
                )));
            }
            validate_named_text("section heading", &section.heading)?;
            if section.paragraphs.is_empty() {
                return Err(BenchError::InvalidInput(format!(
                    "section {:?} requires at least one paragraph",
                    section.id
                )));
            }

            paragraph_count = paragraph_count
                .checked_add(section.paragraphs.len())
                .ok_or_else(|| {
                    BenchError::InvalidInput("canonical paragraph count overflowed".to_owned())
                })?;
            if paragraph_count > MAX_PARAGRAPHS {
                return Err(BenchError::InvalidInput(format!(
                    "canonical YAML supports at most {MAX_PARAGRAPHS} paragraphs"
                )));
            }

            let mut paragraphs = Vec::with_capacity(section.paragraphs.len());
            for paragraph in section.paragraphs {
                if !paragraph_ids.insert(paragraph.id.clone()) {
                    return Err(BenchError::InvalidInput(format!(
                        "duplicate paragraph id {:?}",
                        paragraph.id
                    )));
                }
                paragraphs.push(Paragraph::new(paragraph.id, paragraph.text)?);
            }
            sections.push(CanonicalSection {
                id: section.id,
                heading: section.heading,
                paragraphs,
            });
        }

        let render_line_count = 1_usize
            .checked_add(sections.len())
            .and_then(|count| count.checked_add(paragraph_count))
            .ok_or_else(|| {
                BenchError::InvalidInput("canonical render line count overflowed".to_owned())
            })?;
        if render_line_count > MAX_CANONICAL_RENDER_LINES {
            return Err(BenchError::InvalidInput(format!(
                "canonical YAML supports at most {MAX_CANONICAL_RENDER_LINES} rendered lines"
            )));
        }

        Ok(Self {
            title: document.title,
            sections,
        })
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn sections(&self) -> &[CanonicalSection] {
        &self.sections
    }

    /// Flattens the title, section headings, and paragraphs in source order.
    pub fn render_lines(&self) -> Vec<String> {
        let capacity = 1
            + self.sections.len()
            + self
                .sections
                .iter()
                .map(|section| section.paragraphs.len())
                .sum::<usize>();
        let mut lines = Vec::with_capacity(capacity);
        lines.push(self.title.clone());
        for section in &self.sections {
            lines.push(section.heading.clone());
            lines.extend(
                section
                    .paragraphs
                    .iter()
                    .map(|paragraph| paragraph.text.clone()),
            );
        }
        lines
    }

    pub(crate) fn mutation_document(&self) -> Result<CanonicalDocument> {
        self.build_mutation_document(None)
    }

    /// Flattens the document while keeping one future source ID available.
    pub(crate) fn mutation_document_reserving(
        &self,
        reserved_paragraph_id: &str,
    ) -> Result<CanonicalDocument> {
        self.build_mutation_document(Some(reserved_paragraph_id))
    }

    fn build_mutation_document(
        &self,
        reserved_paragraph_id: Option<&str>,
    ) -> Result<CanonicalDocument> {
        let capacity = 1
            + self.sections.len()
            + self
                .sections
                .iter()
                .map(|section| section.paragraphs.len())
                .sum::<usize>();
        let mut used_ids = self
            .sections
            .iter()
            .flat_map(|section| &section.paragraphs)
            .map(|paragraph| paragraph.id.clone())
            .collect::<HashSet<_>>();
        if let Some(reserved_paragraph_id) = reserved_paragraph_id {
            used_ids.insert(reserved_paragraph_id.to_owned());
        }
        let mut paragraphs = Vec::with_capacity(capacity);
        paragraphs.push(metadata_paragraph(
            "pdfdelta-title",
            &self.title,
            &mut used_ids,
        )?);
        for (section_index, section) in self.sections.iter().enumerate() {
            paragraphs.push(metadata_paragraph(
                &format!("pdfdelta-section-{section_index}"),
                &section.heading,
                &mut used_ids,
            )?);
            paragraphs.extend(section.paragraphs.iter().cloned());
        }
        CanonicalDocument::new(paragraphs)
    }
}

fn metadata_paragraph(
    id_prefix: &str,
    text: &str,
    used_ids: &mut HashSet<String>,
) -> Result<Paragraph> {
    for suffix in 0..=MAX_PARAGRAPHS {
        let id = format!("{id_prefix}-{suffix}");
        if used_ids.insert(id.clone()) {
            return Paragraph::new(id, text);
        }
    }
    Err(BenchError::InvalidInput(
        "canonical metadata identifier space is exhausted".to_owned(),
    ))
}

impl CanonicalSection {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn heading(&self) -> &str {
        &self.heading
    }

    pub fn paragraphs(&self) -> &[Paragraph] {
        &self.paragraphs
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlRoot {
    document: YamlDocument,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlDocument {
    title: String,
    sections: Vec<YamlSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlSection {
    id: String,
    heading: String,
    paragraphs: Vec<YamlParagraph>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct YamlParagraph {
    id: String,
    text: String,
}

fn reject_disallowed_yaml_syntax(yaml: &str) -> Result<()> {
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "---"
            || trimmed == "..."
            || trimmed.starts_with("--- ")
            || trimmed.starts_with("... ")
        {
            return Err(BenchError::InvalidInput(
                "canonical YAML must contain a single document".into(),
            ));
        }
        if trimmed.starts_with("<<:") || trimmed.starts_with("<< :") {
            return Err(BenchError::InvalidInput(
                "canonical YAML must not contain merge keys".into(),
            ));
        }
        if trimmed.starts_with('&')
            || trimmed.starts_with('*')
            || trimmed.starts_with('!')
            || trimmed.starts_with("<<")
        {
            return Err(BenchError::InvalidInput(
                "canonical YAML must not contain anchors, aliases, tags, or merge keys".into(),
            ));
        }
        if let Some(colon_pos) = trimmed.find(':') {
            let after = trimmed[colon_pos + 1..].trim_start();
            if after.starts_with('&')
                || after.starts_with('*')
                || after.starts_with('!')
                || after.starts_with("<<")
            {
                return Err(BenchError::InvalidInput(
                    "canonical YAML must not contain anchors, aliases, tags, or merge keys".into(),
                ));
            }
        }
        if let Some(after_dash) = trimmed.strip_prefix("- ") {
            let after_dash = after_dash.trim_start();
            if after_dash.starts_with('&')
                || after_dash.starts_with('*')
                || after_dash.starts_with('!')
                || after_dash.starts_with("<<")
            {
                return Err(BenchError::InvalidInput(
                    "canonical YAML must not contain anchors, aliases, tags, or merge keys".into(),
                ));
            }
            if let Some(colon_pos) = after_dash.find(':') {
                let after = after_dash[colon_pos + 1..].trim_start();
                if after.starts_with('&')
                    || after.starts_with('*')
                    || after.starts_with('!')
                    || after.starts_with("<<")
                {
                    return Err(BenchError::InvalidInput(
                        "canonical YAML must not contain anchors, aliases, tags, or merge keys"
                            .into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_text(text: &str) -> Result<()> {
    validate_named_text("paragraph and line text", text)
}

fn validate_named_text(name: &str, text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "{name} must not be blank"
        )));
    }
    if text.len() > MAX_PARAGRAPH_TEXT_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "{name} must not exceed {MAX_PARAGRAPH_TEXT_BYTES} bytes"
        )));
    }
    if text.trim() != text {
        return Err(BenchError::InvalidInput(format!(
            "{name} must not have leading or trailing whitespace"
        )));
    }
    if !text.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) {
        return Err(BenchError::InvalidInput(format!(
            "{name} must use printable ASCII"
        )));
    }
    if text.as_bytes().windows(2).any(|pair| pair == b"  ") {
        return Err(BenchError::InvalidInput(format!(
            "{name} must use single spaces"
        )));
    }
    Ok(())
}

pub(crate) fn validate_paragraph_id(id: &str) -> Result<()> {
    validate_named_id("paragraph ids", id)
}

fn validate_named_id(name: &str, id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(BenchError::InvalidInput(format!(
            "{name} must not be blank"
        )));
    }
    if id.len() > MAX_PARAGRAPH_ID_BYTES {
        return Err(BenchError::InvalidInput(format!(
            "{name} must not exceed {MAX_PARAGRAPH_ID_BYTES} bytes"
        )));
    }
    if !id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(BenchError::InvalidInput(format!(
            "{name} may contain only ASCII letters, digits, '-' and '_'"
        )));
    }
    Ok(())
}
