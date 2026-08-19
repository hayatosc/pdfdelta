use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use pdfdelta_core::{
    model::{DecodedText, Glyph, TextRenderMode},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};

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
            Ok(()) => return ExitCode::SUCCESS,
            Err(error) => eprintln!("{error}"),
        },
        None => {
            let old = cli.old.as_deref().map_or("<missing>", path_display);
            let new = cli.new.as_deref().map_or("<missing>", path_display);
            let output = cli.json.as_deref().map_or("stdout", path_display);
            let mode = if cli.strict { "strict" } else { "default" };
            eprintln!("comparison is not implemented yet: {old} -> {new} ({output}, {mode})");
        }
    }
    ExitCode::from(2)
}

fn inspect_document(path: &Path, backend_info: bool, glyphs: bool) -> Result<(), String> {
    if !backend_info && !glyphs {
        return Err(format!("no inspection output selected: {}", path.display()));
    }

    let limits = ParseLimits::default();
    let bytes = read_limited(path, limits.max_input_bytes)?;
    if backend_info {
        inspect_backend(path, Arc::clone(&bytes), limits)?;
    }
    if glyphs {
        inspect_glyphs(path, bytes, limits)?;
    }
    Ok(())
}

fn inspect_backend(path: &Path, bytes: Arc<[u8]>, limits: ParseLimits) -> Result<(), String> {
    let pdf = LopdfParser
        .parse(bytes, limits)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    let version = pdf.version();
    let page_count = pdf
        .pages()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .len();

    println!("backend: {}", LopdfParser::NAME);
    println!("pdf-version: {}.{}", version.major, version.minor);
    println!("pages: {page_count}");
    Ok(())
}

fn inspect_glyphs(path: &Path, bytes: Arc<[u8]>, parse_limits: ParseLimits) -> Result<(), String> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let document = source
        .extract(bytes, parse_limits, ExtractionLimits::default())
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;

    println!("glyphs: {}", document.items().len());
    for glyph in document.items() {
        println!("{}", format_glyph(glyph));
    }
    Ok(())
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

fn path_display(path: &Path) -> &str {
    path.to_str().unwrap_or("<non-UTF-8 path>")
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use pdfdelta_core::{
        model::{
            DecodedText, FontId, Glyph, GlyphId, GlyphProvenance, PageId, Rect, TextRenderMode,
            Vec2,
        },
        pdf::ObjectRef,
    };

    use super::{Cli, Command, format_glyph};

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
