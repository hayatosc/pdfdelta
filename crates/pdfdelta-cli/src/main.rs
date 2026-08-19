use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use clap::{Parser, Subcommand};
use pdfdelta_core::{
    diff::Comparison,
    model::{DecodedText, Document, Glyph, TextRenderMode},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::{PipelineOptions, compare_glyph_documents},
    report::{ExtractionStatus, exit_status, render_text, write_json},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Parser)]
#[command(
    name = "pdfdelta",
    version,
    about = "Compare meaningful text changes between two PDF documents",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[arg(value_name = "OLD_PDF")]
    old: Option<PathBuf>,

    #[arg(value_name = "NEW_PDF")]
    new: Option<PathBuf>,

    #[arg(long, value_name = "PATH", requires = "new")]
    json: Option<PathBuf>,

    #[arg(long, requires = "new")]
    strict: bool,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect evidence extracted from one PDF.
    Inspect {
        document: PathBuf,

        /// Print the selected parser backend and parsed document summary.
        #[arg(long)]
        backend_info: bool,

        /// Print extracted glyph evidence.
        #[arg(long)]
        glyphs: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Inspect {
            document,
            backend_info,
            glyphs,
        }) => match inspect_document(&document, backend_info, glyphs) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(2)
            }
        },
        None => match compare_documents(
            cli.old.as_deref(),
            cli.new.as_deref(),
            cli.json.as_deref(),
            cli.strict,
        ) {
            Ok(status) => ExitCode::from(status),
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(2)
            }
        },
    }
}

fn compare_documents(
    old_path: Option<&Path>,
    new_path: Option<&Path>,
    json_path: Option<&Path>,
    strict: bool,
) -> Result<u8, String> {
    let old_path = old_path.ok_or_else(|| "cannot compare PDFs: OLD_PDF is required".to_owned())?;
    let new_path = new_path.ok_or_else(|| "cannot compare PDFs: NEW_PDF is required".to_owned())?;

    if let Some(json_path) = json_path {
        ensure_output_does_not_alias_input(json_path, old_path, new_path)?;
    }

    let parse_limits = ParseLimits::default();
    let old = extract_comparison_document("old", old_path, parse_limits)?;
    let new = extract_comparison_document("new", new_path, parse_limits)?;
    let comparison =
        compare_glyph_documents(&old, &new, PipelineOptions::default()).map_err(|error| {
            format!(
                "cannot compare old PDF {} with new PDF {}: {error}",
                old_path.display(),
                new_path.display()
            )
        })?;
    let extraction = ExtractionStatus::complete();
    let status = exit_status(&comparison, &extraction, strict).map_err(|error| {
        format!(
            "cannot determine comparison status for {} and {}: {error}",
            old_path.display(),
            new_path.display()
        )
    })?;

    if let Some(json_path) = json_path {
        write_json_atomically(json_path, &comparison, &extraction)?;
    } else {
        let report = render_text(&comparison, &extraction).map_err(|error| {
            format!(
                "cannot render comparison report for {} and {}: {error}",
                old_path.display(),
                new_path.display()
            )
        })?;
        let stdout = io::stdout();
        let mut stdout = stdout.lock();
        stdout
            .write_all(report.as_bytes())
            .map_err(|error| format!("cannot write comparison report to stdout: {error}"))?;
        stdout
            .flush()
            .map_err(|error| format!("cannot flush comparison report to stdout: {error}"))?;
    }

    Ok(status.code())
}

fn extract_comparison_document(
    side: &str,
    path: &Path,
    parse_limits: ParseLimits,
) -> Result<Document<Glyph>, String> {
    let bytes = read_limited(path, parse_limits.max_input_bytes)
        .map_err(|error| format!("cannot load {side} PDF {}: {error}", path.display()))?;
    ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor)
        .extract(bytes, parse_limits, ExtractionLimits::default())
        .map_err(|error| {
            format!(
                "cannot parse or extract {side} PDF {}: {error}",
                path.display()
            )
        })
}

fn write_json_atomically(
    output_path: &Path,
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<(), String> {
    let (temporary_path, mut temporary_file) = create_temporary_output(output_path)?;
    let prepare_result = (|| {
        write_json(&mut temporary_file, comparison, extraction).map_err(|error| {
            format!(
                "cannot render JSON comparison report for {}: {error}",
                output_path.display()
            )
        })?;
        temporary_file.flush().map_err(|error| {
            format!(
                "cannot flush temporary JSON report for {}: {error}",
                output_path.display()
            )
        })?;
        temporary_file.sync_all().map_err(|error| {
            format!(
                "cannot sync temporary JSON report for {}: {error}",
                output_path.display()
            )
        })
    })();
    drop(temporary_file);

    if let Err(error) = prepare_result {
        return Err(error_with_temporary_cleanup(&temporary_path, error));
    }

    if let Err(error) = fs::hard_link(&temporary_path, output_path) {
        let message = if error.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "refusing to overwrite existing JSON report {}: output path already exists",
                output_path.display()
            )
        } else {
            format!(
                "cannot publish JSON report {} atomically without replacing an existing file: {error}",
                output_path.display()
            )
        };
        return Err(error_with_temporary_cleanup(&temporary_path, message));
    }

    fs::remove_file(&temporary_path).map_err(|error| {
        format!(
            "JSON report {} was published without overwriting an existing file, but temporary report {} could not be removed: {error}",
            output_path.display(),
            temporary_path.display()
        )
    })
}

fn error_with_temporary_cleanup(temporary_path: &Path, primary_error: String) -> String {
    match fs::remove_file(temporary_path) {
        Ok(()) => primary_error,
        Err(cleanup_error) => format!(
            "{primary_error}; temporary JSON report {} could not be removed: {cleanup_error}",
            temporary_path.display()
        ),
    }
}

fn create_temporary_output(output_path: &Path) -> Result<(PathBuf, File), String> {
    output_path.file_name().ok_or_else(|| {
        format!(
            "JSON output path must name a file: {}",
            output_path.display()
        )
    })?;
    let parent = output_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    for _ in 0..128 {
        let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary_name = format!(".pdfdelta-{}-{sequence}.tmp", std::process::id());
        let temporary_path = parent.join(temporary_name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        match options.open(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot create temporary JSON report next to {}: {error}",
                    output_path.display()
                ));
            }
        }
    }

    Err(format!(
        "cannot create a unique temporary JSON report next to {}",
        output_path.display()
    ))
}

fn ensure_output_does_not_alias_input(
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    for (side, input_path) in [("old", old_path), ("new", new_path)] {
        if paths_refer_to_same_file(output_path, input_path)? {
            return Err(format!(
                "refusing JSON output {} because it refers to the {side} PDF {}",
                output_path.display(),
                input_path.display()
            ));
        }
    }
    Ok(())
}

fn paths_refer_to_same_file(output_path: &Path, input_path: &Path) -> Result<bool, String> {
    if output_path == input_path {
        return Ok(true);
    }

    let output_metadata = match fs::metadata(output_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "cannot inspect JSON output path {} for input collision: {error}",
                output_path.display()
            ));
        }
    };
    let input_metadata = fs::metadata(input_path).map_err(|error| {
        format!(
            "cannot inspect input path {} for JSON output collision: {error}",
            input_path.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if output_metadata.dev() == input_metadata.dev()
            && output_metadata.ino() == input_metadata.ino()
        {
            return Ok(true);
        }
    }

    let output_canonical = fs::canonicalize(output_path).map_err(|error| {
        format!(
            "cannot resolve JSON output path {} for input collision: {error}",
            output_path.display()
        )
    })?;
    let input_canonical = fs::canonicalize(input_path).map_err(|error| {
        format!(
            "cannot resolve input path {} for JSON output collision: {error}",
            input_path.display()
        )
    })?;
    Ok(output_canonical == input_canonical)
}

fn inspect_document(path: &Path, backend_info: bool, glyphs: bool) -> Result<(), String> {
    if !backend_info && !glyphs {
        return Err(format!("no inspection output selected: {}", path.display()));
    }

    let limits = ParseLimits::default();
    let bytes = read_limited(path, limits.max_input_bytes)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if backend_info {
        inspect_backend(path, Arc::clone(&bytes), limits, &mut stdout)?;
    }
    if glyphs {
        inspect_glyphs(path, bytes, limits, &mut stdout)?;
    }
    stdout.flush().map_err(|error| {
        format!(
            "cannot flush inspection output for {}: {error}",
            path.display()
        )
    })
}

fn inspect_backend<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    writer: &mut W,
) -> Result<(), String> {
    let pdf = LopdfParser
        .parse(bytes, limits)
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
    Ok(())
}

fn inspect_glyphs<W: Write>(
    path: &Path,
    bytes: Arc<[u8]>,
    parse_limits: ParseLimits,
    writer: &mut W,
) -> Result<(), String> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract(bytes, parse_limits, ExtractionLimits::default())
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;

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

fn write_inspection_line<W: Write>(
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

fn format_glyph(glyph: &Glyph) -> String {
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

fn lowercase_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn render_mode_name(mode: TextRenderMode) -> &'static str {
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

fn read_limited(path: &Path, max_bytes: usize) -> Result<Arc<[u8]>, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let read_limit = u64::try_from(max_bytes)
        .map_err(|_| "configured PDF input limit does not fit in u64".to_owned())?
        .saturating_add(1);
    let mut reader = file.take(read_limit);
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "cannot read {}: PDF input exceeds the {max_bytes}-byte limit",
            path.display()
        ));
    }
    Ok(Arc::from(bytes))
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use clap::Parser;
    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::ObjectRef,
    };

    use super::{Cli, Command, format_glyph, write_inspection_line};

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
    fn parses_strict_comparison_mode() {
        let cli = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--strict"])
            .expect("strict comparison arguments should parse");

        assert!(cli.strict);
        assert_eq!(cli.old.as_deref(), Some(std::path::Path::new("old.pdf")));
        assert_eq!(cli.new.as_deref(), Some(std::path::Path::new("new.pdf")));
    }

    #[test]
    fn strict_mode_requires_a_new_document() {
        assert!(Cli::try_parse_from(["pdfdelta", "old.pdf", "--strict"]).is_err());
    }

    #[test]
    fn parses_backend_info_inspection() {
        let cli = Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--backend-info"])
            .expect("backend inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: true,
                glyphs: false,
                ..
            })
        ));
    }

    #[test]
    fn parses_glyph_inspection() {
        let cli = Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--glyphs"])
            .expect("glyph inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: false,
                glyphs: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_backend_and_glyph_inspection() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "inspect",
            "document.pdf",
            "--backend-info",
            "--glyphs",
        ])
        .expect("combined inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: true,
                glyphs: true,
                ..
            })
        ));
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

    #[cfg(unix)]
    #[test]
    fn temporary_json_report_has_private_permissions() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let output_path = std::env::temp_dir().join(format!(
            "pdfdelta-temporary-mode-test-{}.json",
            std::process::id()
        ));
        let (temporary_path, temporary_file) = super::create_temporary_output(&output_path)
            .expect("temporary JSON report should be created");
        let mode = temporary_file
            .metadata()
            .expect("temporary JSON metadata should be readable")
            .permissions()
            .mode()
            & 0o777;
        drop(temporary_file);
        fs::remove_file(temporary_path).expect("temporary JSON report should be removed");

        assert_eq!(mode, 0o600);
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
