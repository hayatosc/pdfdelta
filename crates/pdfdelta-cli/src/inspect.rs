use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    model::{DecodedText, Glyph, TextRenderMode},
    pdf::{LopdfParser, ParseLimits, PdfDict, PdfObject},
    source::{ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionLimits},
};

use crate::fs::{parse_external_font_identities, parse_lopdf, read_limited, read_password_file};

pub fn inspect_document(
    path: &Path,
    backend_info: bool,
    glyphs: bool,
    objects: bool,
    svg: Option<&Path>,
    password_file: Option<&Path>,
    font_identity: &[String],
) -> Result<(), String> {
    let backend_info = backend_info || (!glyphs && !objects && svg.is_none());
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
    if objects {
        inspect_objects(
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
            Arc::clone(&bytes),
            limits,
            password.as_deref(),
            &external_font_identities,
            &mut stdout,
        )?;
    }
    if let Some(svg_path) = svg {
        inspect_svg(
            path,
            svg_path,
            Arc::clone(&bytes),
            limits,
            password.as_deref(),
            &external_font_identities,
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

pub fn inspect_objects<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    password: Option<&str>,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, parse_limits, password)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let trailer = pdf
        .trailer()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    write_inspection_line(
        writer,
        path,
        format_args!("trailer: {}", format_pdf_dict(&trailer)),
    )?;
    let pages = pdf
        .pages()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    write_inspection_line(writer, path, format_args!("pages: {}", pages.len()))?;
    for (index, page_ref) in pages.iter().enumerate() {
        let page_num = index + 1;
        let page_dict = pdf
            .page_dict(*page_ref)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        write_inspection_line(
            writer,
            path,
            format_args!(
                "page {page_num}: object {}:{} {}",
                page_ref.0.object_number,
                page_ref.0.generation,
                format_pdf_dict(&page_dict)
            ),
        )?;
    }
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

pub fn inspect_svg(
    path: &Path,
    svg_path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    password: Option<&str>,
    external_font_identities: &ExternalFontIdentities,
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
    let document = outcome.document();
    let mut file = std::fs::File::create(svg_path).map_err(|error| {
        format!(
            "cannot create svg output file {}: {error}",
            svg_path.display()
        )
    })?;
    pdfdelta_core::report::write_glyph_overlay_svg(document, &mut file)
        .map_err(|error| format!("cannot render svg overlay for {}: {error}", path.display()))?;
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

pub fn format_pdf_object(object: &PdfObject) -> String {
    match object {
        PdfObject::Null => "null".to_owned(),
        PdfObject::Boolean(val) => val.to_string(),
        PdfObject::Integer(val) => val.to_string(),
        PdfObject::Real(val) => {
            if val.fract() == 0.0 {
                format!("{val:.1}")
            } else {
                format!("{val}")
            }
        }
        PdfObject::Name(bytes) => format!("/{}", String::from_utf8_lossy(bytes)),
        PdfObject::String(bytes) => {
            if let Ok(s) = std::str::from_utf8(bytes) {
                format!("{s:?}")
            } else {
                format!("<{}>", lowercase_hex(bytes))
            }
        }
        PdfObject::Array(items) => {
            let formatted: Vec<_> = items.iter().map(format_pdf_object).collect();
            format!("[{}]", formatted.join(" "))
        }
        PdfObject::Dictionary(dict) => format_pdf_dict(dict),
        PdfObject::Stream(dict) => format!("{} stream", format_pdf_dict(dict)),
        PdfObject::Reference(reference) => {
            format!("{} {} R", reference.object_number, reference.generation)
        }
    }
}

pub fn format_pdf_dict(dict: &PdfDict) -> String {
    let mut parts = Vec::with_capacity(dict.len());
    for (key, value) in dict {
        parts.push(format!(
            "/{} {}",
            String::from_utf8_lossy(key),
            format_pdf_object(value)
        ));
    }
    format!("<< {} >>", parts.join(" "))
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
    use std::{
        collections::BTreeMap,
        io::{self, Write},
    };

    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::{ObjectRef, PdfDict, PdfObject},
    };

    use super::{format_glyph, format_pdf_dict, format_pdf_object, write_inspection_line};

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
    fn formats_pdf_objects_and_dictionaries() {
        let mut dict: PdfDict = BTreeMap::new();
        dict.insert(b"Type".to_vec(), PdfObject::Name(b"Page".to_vec()));
        dict.insert(
            b"Parent".to_vec(),
            PdfObject::Reference(ObjectRef {
                object_number: 2,
                generation: 0,
            }),
        );
        dict.insert(
            b"MediaBox".to_vec(),
            PdfObject::Array(vec![
                PdfObject::Integer(0),
                PdfObject::Integer(0),
                PdfObject::Real(612.0),
                PdfObject::Real(792.0),
            ]),
        );

        assert_eq!(
            format_pdf_dict(&dict),
            "<< /MediaBox [0 0 612.0 792.0] /Parent 2 0 R /Type /Page >>"
        );

        assert_eq!(format_pdf_object(&PdfObject::Null), "null");
        assert_eq!(format_pdf_object(&PdfObject::Boolean(true)), "true");
        assert_eq!(
            format_pdf_object(&PdfObject::String(b"Hello".to_vec())),
            "\"Hello\""
        );
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
