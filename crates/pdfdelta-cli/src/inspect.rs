use std::{
    io::{self, Write},
    path::Path,
    sync::Arc,
};

use pdfdelta_core::{
    model::{DecodedText, Glyph, GlyphCropStatus, GlyphPathClipStatus, TextRenderMode, VectorLine},
    pdf::{LopdfParser, ParseLimits, PdfDict, PdfIssue, PdfObject},
    source::{ContentStreamGlyphExtractor, ExternalFontIdentities, ExtractionLimits},
};

use crate::evidence_text::escape_terminal_controls;
use crate::fs::{
    parse_external_font_identities, parse_lopdf, paths_refer_to_same_file, read_limited,
    read_password_file, write_output_atomically,
};

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
    if let Some(svg_path) = svg
        && paths_refer_to_same_file(svg_path, path, "SVG output collision")?
    {
        return Err(format!(
            "refusing SVG output {} because it refers to the inspected PDF {}",
            svg_path.display(),
            path.display()
        ));
    }
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

fn inspect_error(path: &Path) -> impl Fn(pdfdelta_core::Error) -> String + '_ {
    move |error| format!("cannot inspect {}: {error}", path.display())
}

pub fn inspect_backend<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    password: Option<&str>,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, limits, password).map_err(inspect_error(path))?;
    let version = pdf.version();
    let page_count = pdf.pages().map_err(inspect_error(path))?.len();

    write_inspection_line(writer, path, format_args!("backend: {}", LopdfParser::NAME))?;
    write_inspection_line(
        writer,
        path,
        format_args!("pdf-version: {}.{}", version.major, version.minor),
    )?;
    write_inspection_line(writer, path, format_args!("pages: {page_count}"))?;
    for issue in pdf.issues() {
        write_inspection_line(writer, path, format_args!("{}", parser_issue_text(issue)))?;
    }
    Ok(())
}

/// Parser issue descriptions can quote PDF-derived bytes, so they are escaped
/// before reaching a terminal.
fn parser_issue_text(issue: &PdfIssue) -> String {
    format!(
        "parser-issue: unresolved: {}",
        escape_terminal_controls(issue.description())
    )
}

pub fn inspect_objects<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    password: Option<&str>,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = parse_lopdf(bytes, parse_limits, password).map_err(inspect_error(path))?;
    let trailer = pdf.trailer().map_err(inspect_error(path))?;
    write_inspection_line(
        writer,
        path,
        format_args!("trailer: {}", format_pdf_dict(&trailer)),
    )?;
    let pages = pdf.pages().map_err(inspect_error(path))?;
    write_inspection_line(writer, path, format_args!("pages: {}", pages.len()))?;
    for (index, page_ref) in pages.iter().enumerate() {
        let page_num = index + 1;
        let page_dict = pdf.page_dict(*page_ref).map_err(inspect_error(path))?;
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
        write_inspection_line(writer, path, format_args!("{}", parser_issue_text(issue)))?;
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
    let pdf = parse_lopdf(bytes, parse_limits, password).map_err(inspect_error(path))?;
    let outcome = ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(
            pdf.as_ref(),
            ExtractionLimits::default(),
            external_font_identities,
        )
        .map_err(inspect_error(path))?;
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
    write_inspection_line(
        writer,
        path,
        format_args!("vector-lines: {}", document.vector_lines().len()),
    )?;
    for line in document.vector_lines() {
        let line = format_vector_line(line);
        write_inspection_line(writer, path, format_args!("{line}"))?;
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
    let pdf = parse_lopdf(bytes, parse_limits, password).map_err(inspect_error(path))?;
    let outcome = ContentStreamGlyphExtractor
        .extract_outcome_with_external_font_identities(
            pdf.as_ref(),
            ExtractionLimits::default(),
            external_font_identities,
        )
        .map_err(inspect_error(path))?;
    let document = outcome.document();
    write_output_atomically(svg_path, "SVG overlay", |writer| {
        pdfdelta_core::report::write_glyph_overlay_svg(document, writer)
            .map_err(|error| format!("cannot render svg overlay for {}: {error}", path.display()))
    })
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
        PdfObject::Name(bytes) => format!(
            "/{}",
            escape_terminal_controls(&String::from_utf8_lossy(bytes))
        ),
        PdfObject::String(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => escape_terminal_controls(&format!("{text:?}")),
            Err(_) => format!("<{}>", lowercase_hex(bytes)),
        },
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
            escape_terminal_controls(&String::from_utf8_lossy(key)),
            format_pdf_object(value)
        ));
    }
    format!("<< {} >>", parts.join(" "))
}

pub fn format_glyph(glyph: &Glyph) -> String {
    let text = match &glyph.text {
        DecodedText::Mapped(text) => {
            format!("text={}", escape_terminal_controls(&format!("{text:?}")))
        }
        DecodedText::Unmapped {
            font_hash,
            glyph_id,
        } => format!(
            "unmapped-font-hash={} unmapped-glyph-id={glyph_id}",
            lowercase_hex(&font_hash.0)
        ),
    };
    format!(
        "glyph id={} page={} {} raw-hex={} bbox=({},{},{},{}) baseline=({},{}) direction=({},{}) font-id={} font-size={} render-order={} render-mode={} crop-status={} path-clip-status={} content-stream-object={} content-stream-generation={} operator-index={}",
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
        crop_status_name(glyph.crop_status),
        path_clip_status_name(glyph.path_clip_status),
        glyph.provenance.content_stream.object_number,
        glyph.provenance.content_stream.generation,
        glyph.provenance.operator_index,
    )
}

pub fn format_vector_line(line: &VectorLine) -> String {
    format!(
        "vector-line id={} page={} from=({},{}) to=({},{}) width={} render-order={} content-stream-object={} content-stream-generation={} operator-index={}",
        line.id.0,
        line.page.0,
        line.from.x,
        line.from.y,
        line.to.x,
        line.to.y,
        line.width,
        line.render_order,
        line.provenance.content_stream.object_number,
        line.provenance.content_stream.generation,
        line.provenance.operator_index,
    )
}

pub fn crop_status_name(status: GlyphCropStatus) -> &'static str {
    match status {
        GlyphCropStatus::Inside => "inside",
        GlyphCropStatus::PartiallyOutside => "partially-outside",
        GlyphCropStatus::Outside => "outside",
    }
}

pub fn path_clip_status_name(status: GlyphPathClipStatus) -> &'static str {
    match status {
        GlyphPathClipStatus::Unclipped => "unclipped",
        GlyphPathClipStatus::Inside => "inside",
        GlyphPathClipStatus::PartiallyOutside => "partially-outside",
        GlyphPathClipStatus::Outside => "outside",
    }
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
            DecodedText, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
            GlyphProvenance, PageId, Rect, TextRenderMode, Vec2,
        },
        pdf::{ObjectRef, PdfDict, PdfIssue, PdfObject},
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
    fn pdf_names_strings_and_keys_escape_terminal_controls() {
        let mut dict: PdfDict = BTreeMap::new();
        dict.insert(b"Ty\x1bpe".to_vec(), PdfObject::Name(b"Page".to_vec()));
        dict.insert(
            b"Type".to_vec(),
            PdfObject::Name("Pa\u{202e}ge".as_bytes().to_vec()),
        );
        assert_eq!(
            format_pdf_dict(&dict),
            "<< /Ty\\u{1b}pe /Page /Type /Pa\\u{202e}ge >>"
        );
        assert_eq!(
            format_pdf_object(&PdfObject::String(b"a\x1b]0;t\x07b".to_vec())),
            "\"a\\u{1b}]0;t\\u{7}b\""
        );
        assert_eq!(
            format_pdf_object(&PdfObject::String(vec![0xff, 0xfe])),
            "<fffe>"
        );
    }

    #[test]
    fn parser_issue_lines_escape_terminal_controls() {
        let issue =
            PdfIssue::unresolved("filter /\u{1b}[31mX is unsupported").expect("valid issue");
        let line = super::parser_issue_text(&issue);
        assert!(!line.contains('\u{1b}'), "{line:?}");
        assert!(line.contains("\\u{1b}"), "{line:?}");
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
            crop_status: GlyphCropStatus::Inside,
            path_clip_status: GlyphPathClipStatus::Unclipped,
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
            "glyph id=7 page=2 text=\"English\\nA\" raw-hex=410aff bbox=(10.25,20.5,16.75,30) baseline=(1,0) direction=(0,-1) font-id=3 font-size=11.5 render-order=4 render-mode=fill-and-stroke crop-status=inside path-clip-status=unclipped content-stream-object=12 content-stream-generation=2 operator-index=9"
        );
    }
}
