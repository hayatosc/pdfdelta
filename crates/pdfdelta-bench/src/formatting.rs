//! Formatting-only fixture pairs: the text is identical, one word is styled.
//!
//! Each pair renders the same words with one word's font, size, fill colour,
//! font family, underline decoration, or text render mode changed. The PDFs are
//! written directly so the fixture controls typography that the acceptance
//! renderers do not expose. Text is placed with absolute text matrices, so a
//! style change never moves the surrounding words except when its own advance
//! changes (the larger-size case).
//!
//! The point of these fixtures is contract coverage: a text-only token diff must
//! report no content change for every pair, while the native report may list a
//! formatting-only change. Whether it does is measured, not assumed: only font
//! size and positional or structural evidence feed formatting-only reasons.

use std::{fmt::Write as _, io::Write as _};

use crate::{
    BenchError, Result,
    mutation::{DEFAULT_MARGIN, PAGE_TOP},
};

const NAME: &str = "formatting-fixture";
const LINE_GAP: u16 = 28;
const PAGE_WIDTH: u16 = 612;
const PAGE_HEIGHT: u16 = 792;
const PLAIN_FONT_SIZE: u16 = 10;
const LARGE_FONT_SIZE: u16 = 14;
/// Every fixture font declares this advance per glyph in 1/1000 em.
const GLYPH_WIDTH_UNITS: u16 = 500;
const OBJECT_COUNT: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormattingChange {
    /// The target word uses a bold font program.
    Bold,
    /// The target word uses a larger font size.
    LargerSize,
    /// The target word is filled red instead of black.
    FillColour,
    /// The target word uses a serif font family.
    FontFamily,
    /// A vector line is drawn under the target word.
    Underline,
    /// The target word is painted with fill-and-stroke render mode.
    RenderMode,
}

impl FormattingChange {
    pub const ALL: [Self; 6] = [
        Self::Bold,
        Self::LargerSize,
        Self::FillColour,
        Self::FontFamily,
        Self::Underline,
        Self::RenderMode,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Bold => "formatting-bold",
            Self::LargerSize => "formatting-larger-size",
            Self::FillColour => "formatting-fill-colour",
            Self::FontFamily => "formatting-font-family",
            Self::Underline => "formatting-underline",
            Self::RenderMode => "formatting-render-mode",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Bold => "one word switches to a bold font",
            Self::LargerSize => "one word grows from 10 to 14 points",
            Self::FillColour => "one word is filled red instead of black",
            Self::FontFamily => "one word switches from sans-serif to serif",
            Self::Underline => "a vector line is drawn under one word",
            Self::RenderMode => "one word is painted with fill-and-stroke render mode",
        }
    }
}

/// One generated formatting pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FormattingCase {
    id: &'static str,
    description: &'static str,
    old_pdf: Vec<u8>,
    new_pdf: Vec<u8>,
}

impl FormattingCase {
    #[must_use]
    pub const fn id(&self) -> &'static str {
        self.id
    }

    #[must_use]
    pub const fn description(&self) -> &'static str {
        self.description
    }

    /// Unstyled revision bytes.
    #[must_use]
    pub fn old_pdf(&self) -> &[u8] {
        &self.old_pdf
    }

    /// Styled revision bytes.
    #[must_use]
    pub fn new_pdf(&self) -> &[u8] {
        &self.new_pdf
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Font {
    Regular,
    Bold,
    Serif,
}

impl Font {
    const fn resource(self) -> &'static str {
        match self {
            Self::Regular => "F1",
            Self::Bold => "F2",
            Self::Serif => "F3",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Style {
    font: Font,
    size: u16,
    fill: (u8, u8, u8),
    render_mode: u8,
    underline: bool,
}

impl Style {
    const PLAIN: Self = Self {
        font: Font::Regular,
        size: PLAIN_FONT_SIZE,
        fill: (0, 0, 0),
        render_mode: 0,
        underline: false,
    };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Segment {
    text: &'static str,
    style: Style,
}

fn default_lines() -> [&'static str; 3] {
    [
        "Release notes describe the default policy for archived records.",
        "Operators should confirm the effective rule before migration.",
        "The change applies to every repository in the organisation.",
    ]
}

fn plain_page() -> Vec<Vec<Segment>> {
    default_lines()
        .into_iter()
        .map(|text| {
            vec![Segment {
                text,
                style: Style::PLAIN,
            }]
        })
        .collect()
}

fn styled_style(change: FormattingChange) -> Style {
    match change {
        FormattingChange::Bold => Style {
            font: Font::Bold,
            ..Style::PLAIN
        },
        FormattingChange::LargerSize => Style {
            size: LARGE_FONT_SIZE,
            ..Style::PLAIN
        },
        FormattingChange::FillColour => Style {
            fill: (255, 0, 0),
            ..Style::PLAIN
        },
        FormattingChange::FontFamily => Style {
            font: Font::Serif,
            ..Style::PLAIN
        },
        FormattingChange::Underline => Style {
            underline: true,
            ..Style::PLAIN
        },
        FormattingChange::RenderMode => Style {
            render_mode: 2,
            ..Style::PLAIN
        },
    }
}

fn styled_page(change: FormattingChange) -> Vec<Vec<Segment>> {
    let mut page = plain_page();
    page[0] = vec![
        Segment {
            text: "Release notes describe the ",
            style: Style::PLAIN,
        },
        Segment {
            text: "default",
            style: styled_style(change),
        },
        Segment {
            text: " policy for archived records.",
            style: Style::PLAIN,
        },
    ];
    page
}

/// Builds every formatting-only pair.
///
/// # Errors
///
/// Returns [`BenchError::Render`] when a fixture PDF cannot be serialized, or
/// [`BenchError::InvalidInput`] when a style change produced identical bytes.
pub fn formatting_cases() -> Result<Vec<FormattingCase>> {
    let old_pdf = render_page(&plain_page())?;
    let mut cases = Vec::with_capacity(FormattingChange::ALL.len());
    for change in FormattingChange::ALL {
        let new_pdf = render_page(&styled_page(change))?;
        if old_pdf == new_pdf {
            return Err(BenchError::InvalidInput(format!(
                "{} did not change the rendered bytes",
                change.id()
            )));
        }
        cases.push(FormattingCase {
            id: change.id(),
            description: change.description(),
            old_pdf: old_pdf.clone(),
            new_pdf,
        });
    }
    Ok(cases)
}

fn render_page(page: &[Vec<Segment>]) -> Result<Vec<u8>> {
    let content = content_stream(page)?;
    let objects = object_bodies(&content);
    let mut output = Vec::with_capacity(content.len().saturating_add(4_096));
    output.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for body in &objects {
        offsets.push(output.len());
        output.extend_from_slice(body.as_bytes());
    }
    let xref_offset = output.len();
    write!(output, "xref\n0 {}\n", OBJECT_COUNT + 1)
        .map_err(|error| render_error(error.to_string()))?;
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        writeln!(output, "{offset:010} 00000 n ")
            .map_err(|error| render_error(error.to_string()))?;
    }
    write!(
        output,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        OBJECT_COUNT + 1
    )
    .map_err(|error| render_error(error.to_string()))?;
    Ok(output)
}

fn object_bodies(content: &str) -> Vec<String> {
    let bodies = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [9 0 R] /Count 1 >>".to_owned(),
        font_body("Helvetica", 4),
        descriptor_body("Helvetica"),
        font_body("Helvetica-Bold", 6),
        descriptor_body("Helvetica-Bold"),
        font_body("Times-Roman", 8),
        descriptor_body("Times-Roman"),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_WIDTH} {PAGE_HEIGHT}] /Resources << /Font << /F1 3 0 R /F2 5 0 R /F3 7 0 R >> >> /Contents 10 0 R >>"
        ),
        format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ),
    ];
    bodies
        .into_iter()
        .enumerate()
        .map(|(index, body)| format!("{} 0 obj\n{body}\nendobj\n", index + 1))
        .collect()
}

fn font_body(base_font: &str, descriptor: usize) -> String {
    let widths = (0..256)
        .map(|_| GLYPH_WIDTH_UNITS.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /{base_font} /FirstChar 0 /LastChar 255 /Widths [{widths}] /FontDescriptor {descriptor} 0 R >>"
    )
}

fn descriptor_body(base_font: &str) -> String {
    format!(
        "<< /Type /FontDescriptor /FontName /{base_font} /Flags 32 /FontBBox [-166 -225 1000 931] /Ascent 800 /Descent -200 /CapHeight 700 /ItalicAngle 0 /StemV 80 /MissingWidth {GLYPH_WIDTH_UNITS} >>"
    )
}

fn content_stream(page: &[Vec<Segment>]) -> Result<String> {
    let mut content = String::new();
    for (row, segments) in page.iter().enumerate() {
        let row = i64::try_from(row)
            .map_err(|error| render_error(format!("line row exceeds i64: {error}")))?;
        let y = PAGE_TOP - row * i64::from(LINE_GAP);
        let mut x = f64::from(DEFAULT_MARGIN);
        for segment in segments {
            let (red, green, blue) = segment.style.fill;
            writeln!(
                content,
                "{:.3} {:.3} {:.3} rg",
                f64::from(red) / 255.0,
                f64::from(green) / 255.0,
                f64::from(blue) / 255.0
            )
            .map_err(|error| render_error(error.to_string()))?;
            writeln!(
                content,
                "BT /{} {} Tf {} Tr 1 0 0 1 {x:.3} {y} Tm ({}) Tj ET",
                segment.style.font.resource(),
                segment.style.size,
                segment.style.render_mode,
                escape_pdf_literal(segment.text)
            )
            .map_err(|error| render_error(error.to_string()))?;
            let advance = segment_width(segment);
            if segment.style.underline {
                writeln!(
                    content,
                    "0 g 0.7 w {:.3} {} m {:.3} {} l S",
                    x,
                    y - 2,
                    x + advance,
                    y - 2
                )
                .map_err(|error| render_error(error.to_string()))?;
            }
            x += advance;
        }
    }
    Ok(content)
}

fn segment_width(segment: &Segment) -> f64 {
    let glyphs = f64::from(u32::try_from(segment.text.chars().count()).unwrap_or(u32::MAX));
    f64::from(segment.style.size) * f64::from(GLYPH_WIDTH_UNITS) / 1_000.0 * glyphs
}

fn escape_pdf_literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(character, '(' | ')' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn render_error(message: String) -> BenchError {
    BenchError::Render {
        renderer: NAME,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_case_changes_bytes_and_keeps_the_word_count() {
        let cases = formatting_cases().expect("formatting fixtures are constructible");
        assert_eq!(cases.len(), FormattingChange::ALL.len());
        let ids = cases
            .iter()
            .map(FormattingCase::id)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), cases.len(), "case ids must be unique");
        for case in &cases {
            assert_ne!(case.old_pdf(), case.new_pdf(), "{} must differ", case.id());
        }
    }
}
