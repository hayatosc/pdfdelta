use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    model::{DecodedText, Glyph, TextRenderMode},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionLimits},
};

use crate::fs::{parse_external_font_identities, parse_lopdf, read_limited, read_password_file};

pub fn inspect_document(
    path: &Path,
    backend_info: bool,
    glyphs: bool,
    password_file: Option<&Path>,
    font_identity: &[String],
) -> Result<(), String> {
    let backend_info = backend_info || !glyphs;
    let limits = ParseLimits::default();
    let bytes = read_limited(path, limits.max_input_bytes)?;
    let password = password_file.map(read_password_file).transpose()?;
    let external_font_identities = parse_external_font_identities(font_identity)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if backend_info {
        inspect_backend(
            path,
            Arc::clone(&bytes),
            limits,
            password.as_deref(),
            &mut stdout,
        )?;
    }
    if glyphs {
        inspect_glyphs(
            path,
            bytes,
            limits,
            password.as_deref(),
            &external_font_identities,
            &mut stdout,
        )?;
    }
    stdout.flush().map_err(|error| {
        format!(
            "cannot flush inspection output for {}: {error}",
            path.display()
        )
    })
}

pub fn inspect_backend<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    password: Option<&str>,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, limits, password)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let version = pdf.version();
    let page_count = pdf
        .pages()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();

    write_inspection_line(writer, path, format_args!("backend: {}", LopdfParser::NAME))?;
    write_inspection_line(
        writer,
        path,
        format_args!("pdf-version: {}.{}", version.major, version.minor),
    )?;
    write_inspection_line(writer, path, format_args!("pages: {page_count}"))?;
    for issue in pdf.issues() {
        write_inspection_line(
            writer,
            path,
            format_args!("parser-issue: unresolved: {}", issue.description()),
        )?;
    }
    Ok(())
}

pub fn inspect_glyphs<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    password: Option<&str>,
    external_font_identities: &ExternalFontIdentities,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, parse_limits, password)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let outcome = ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(
            pdf.as_ref(),
            ExtractionLimits::default(),
            external_font_identities,
        )
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    for issue in outcome.issues() {
        write_inspection_line(
            writer,
            path,
            format_args!(
                "extraction-issue: {:?}: {}",
                issue.scope(),
                issue.description()
            ),
        )?;
    }
    let document = outcome.document();

    write_inspection_line(
        writer,
        path,
        format_args!("glyphs: {}", document.items().len()),
    )?;
    for glyph in document.items() {
        let glyph = format_glyph(glyph);
        write_inspection_line(writer, path, format_args!("{glyph}"))?;
    }
    Ok(())
}

pub fn write_inspection_line<W: Write>(
    writer: &mut W,
    path: &Path,
    line: std::fmt::Arguments<'_>,
) -> Result<(), String> {
    writeln!(writer, "{line}").map_err(|error| {
        format!(
            "cannot write inspection output for {}: {error}",
            path.display()
        )
    })
}

pub fn format_glyph(glyph: &Glyph) -> String {
    let text = match &glyph.text {
        DecodedText::Mapped(text) => format!("text={text:?}"),
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => format!(
            "unmapped-font-hash={} unmapped-glyph-id={glyph_id}",
            lowercase_hex(&font_hash.0)
        ),
    };
    format!(
        "glyph id={} page={} {} raw-hex={} bbox=({},{},{},{}) baseline=({},{}) direction=({},{}) font-id={} font-size={} render-order={} render-mode={} content-stream-object={} content-stream-generation={} operator-index={}",
        glyph.id.0,
        glyph.page.0,
        text,
        lowercase_hex(&glyph.raw_code),
        glyph.bbox.min.x,
        glyph.bbox.min.y,
        glyph.bbox.max.x,
        glyph.bbox.max.y,
        glyph.baseline.x,
        glyph.baseline.y,
        glyph.direction.x,
        glyph.direction.y,
        glyph.font_id.0,
        glyph.font_size,
        glyph.render_order,
        render_mode_name(glyph.render_mode),
        glyph.provenance.content_stream.object_number,
        glyph.provenance.content_stream.generation,
        glyph.provenance.operator_index,
    )
}

pub fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

pub fn render_mode_name(mode: TextRenderMode) -> &'static str {
    match mode {
        TextRenderMode::Fill => "fill",
        TextRenderMode::Stroke => "stroke",
        TextRenderMode::FillAndStroke => "fill-and-stroke",
        TextRenderMode::Invisible => "invisible",
        TextRenderMode::FillAndClip => "fill-and-clip",
        TextRenderMode::StrokeAndClip => "stroke-and-clip",
        TextRenderMode::FillStrokeAndClip => "fill-stroke-and-clip",
        TextRenderMode::Clip => "clip",
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::ObjectRef,
    };

    use super::{format_glyph, write_inspection_line};

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn inspection_write_failure_returns_contextual_error() {
        let error = write_inspection_line(
            &mut BrokenPipeWriter,
            std::path::Path::new("fixture.pdf"),
            format_args!("backend: test"),
        )
        .expect_err("broken inspection writer should fail");

        assert!(error.contains("cannot write inspection output for fixture.pdf"));
        assert!(error.contains("broken pipe"));
    }

    #[test]
    fn formats_glyph_evidence_stably() {
        let glyph = Glyph {
            id: GlyphId(7),
            text: DecodedText::Mapped("English\nA".to_owned()),
            raw_code: vec![0x41, 0x0a, 0xff],
            page: PageId(2),
            bbox: Rect {
                min: Vec2 { x: 10.25, y: 20.5 },
                max: Vec2 { x: 16.75, y: 30.0 },
            },
            baseline: Vec2 { x: 1.0, y: 0.0 },
            direction: Vec2 { x: 0.0, y: -1.0 },
            font_id: FontId(3),
            font_size: 11.5,
            render_order: 4,
            render_mode: TextRenderMode::FillAndStroke,
            provenance: GlyphProvenance {
                content_stream: ObjectRef {
                    object_number: 12,
                    generation: 2,
                },
                operator_index: 9,
            },
        };

        assert_eq!(
            format_glyph(&glyph),
            "glyph id=7 page=2 text=\"English\\nA\" raw-hex=410aff bbox=(10.25,20.5,16.75,30) baseline=(1,0) direction=(0,-1) font-id=3 font-size=11.5 render-order=4 render-mode=fill-and-stroke content-stream-object=12 content-stream-generation=2 operator-index=9"
        );
    }
}
