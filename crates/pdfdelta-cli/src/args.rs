use std::{
    io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};
use pdfdelta_core::pipeline::validate_limit_scale as validate_pipeline_limit_scale;

#[derive(Debug, Parser)]
#[command(
    name = "pdfdelta",
    version,
    about = "Compare PDF content and relationships with explicit evidence coverage",
    long_about = "pdfdelta compares selected PDF evidence channels and retains unresolved regions.\n\
                  The default contract includes text, visual content, forms, and relationships.\n\
                  Use --native-text-only for the explicit native-glyph adapter.",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Path to the older/original PDF document (use '-' for standard input).
    #[arg(value_name = "OLD_PDF")]
    pub old: Option<PathBuf>,

    /// Path to the newer/modified PDF document (use '-' for standard input).
    #[arg(value_name = "NEW_PDF")]
    pub new: Option<PathBuf>,

    /// Content channels to compare through the shared evidence pipeline.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "text,visual,forms,relations"
    )]
    pub channels: Vec<ComparisonChannel>,

    /// Compare only extracted native glyphs using the legacy report contract.
    #[arg(long, conflicts_with = "channels")]
    pub native_text_only: bool,

    /// Write a machine-readable JSON comparison report to a file.
    #[arg(short = 'j', long, value_name = "PATH", requires = "new")]
    pub json: Option<PathBuf>,

    /// Create a static HTML review directory with source PDFs and evidence JSON.
    #[arg(
        long,
        value_name = "DIR",
        requires = "new",
        conflicts_with = "native_text_only"
    )]
    pub review: Option<PathBuf>,

    /// Write the human-readable comparison report to a file instead of standard output.
    #[arg(short = 'o', long, value_name = "PATH", requires = "new")]
    pub output: Option<PathBuf>,

    /// Write a phase-by-phase diagnostic trace to a new JSON file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub trace_json: Option<PathBuf>,

    /// Compatibility alias for the default incomplete-comparison exit code 3.
    #[arg(short = 's', long, requires = "new")]
    pub strict: bool,

    /// Suppress human-readable diff output to standard output.
    #[arg(short = 'q', long, requires = "new")]
    pub quiet: bool,

    /// When to colorize the human-readable report: auto, always, or never.
    #[arg(
        long,
        value_name = "WHEN",
        value_enum,
        default_value = "auto",
        global = true
    )]
    pub color: ColorChoice,

    /// Scale comparison pipeline resource limits by a factor of at least 1.
    ///
    /// This scales n-gram token elements, alignment candidate visits and DP
    /// cells, and diff token and edit-distance limits. Parser and extraction
    /// limits are unchanged.
    #[arg(
        long,
        value_name = "FACTOR",
        default_value_t = 1.0,
        value_parser = parse_limit_scale,
        requires = "new"
    )]
    pub limit_scale: f64,

    /// Read the old PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub old_password_file: Option<PathBuf>,

    /// Read the new PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub new_password_file: Option<PathBuf>,

    /// Assert an external font identity as `BASE_FONT=IDENTITY` for the old PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    pub old_font_identity: Vec<String>,

    /// Assert an external font identity as `BASE_FONT=IDENTITY` for the new PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    pub new_font_identity: Vec<String>,

    /// Reuse cached glyph extraction results stored under this directory.
    ///
    /// Cache entries are keyed by the file contents and every extraction
    /// input; a missing, corrupt, or outdated entry falls back to a fresh
    /// extraction, so comparison results are identical with or without the
    /// cache.
    #[arg(long, value_name = "DIR", requires = "new")]
    pub extraction_cache_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ComparisonChannel {
    Text,
    Visual,
    Forms,
    Relations,
    Presentation,
}

impl From<ComparisonChannel> for pdfdelta_core::document::Channel {
    fn from(channel: ComparisonChannel) -> Self {
        match channel {
            ComparisonChannel::Text => Self::Text,
            ComparisonChannel::Visual => Self::Visual,
            ComparisonChannel::Forms => Self::Forms,
            ComparisonChannel::Relations => Self::Relations,
            ComparisonChannel::Presentation => Self::Presentation,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Internal bounded image-hashing process; consumes PDF bytes on standard input.
    #[command(hide = true)]
    HashImages,
    /// Internal bounded native acquisition process; consumes framed PDF bytes.
    #[command(hide = true)]
    AcquireNative,
    /// Internal bounded rendering process; consumes PDF bytes on standard input.
    #[command(hide = true)]
    RenderPage {
        page: usize,
        pages: usize,
        width: u16,
        height: u16,
        object_number: u32,
        generation: u16,
    },
    /// Inspect evidence extracted from one PDF.
    Inspect {
        /// Path to the PDF document to inspect (use '-' for standard input).
        #[arg(value_name = "DOCUMENT")]
        document: PathBuf,

        /// Print the selected parser backend and parsed document summary.
        #[arg(long)]
        backend_info: bool,

        /// Print extracted glyph evidence.
        #[arg(long)]
        glyphs: bool,

        /// Print parsed PDF indirect objects and structural summary.
        #[arg(long)]
        objects: bool,

        /// Read the PDF password from a file.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,

        /// Assert an external font identity as `BASE_FONT=IDENTITY`.
        #[arg(long, value_name = "BASE_FONT=IDENTITY")]
        font_identity: Vec<String>,

        /// Write glyph overlay debug visualization to an SVG file.
        #[arg(long, value_name = "PATH")]
        svg: Option<PathBuf>,
    },

    /// Generate shell completion script for the specified shell.
    Completions {
        /// Target shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Colorize only when stdout is a terminal.
    Auto,
    /// Always colorize, even when stdout is redirected.
    Always,
    /// Never colorize the report.
    Never,
}

/// Resolves the user's color preference against the actual output stream.
/// Plain redirected output stays readable because `auto` disables ANSI
/// escapes whenever stdout is not a TTY.
pub fn resolve_color(choice: ColorChoice) -> bool {
    use std::io::IsTerminal;

    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => io::stdout().is_terminal(),
    }
}

#[derive(Clone, Copy)]
pub struct ComparisonOptions<'a> {
    pub json_path: Option<&'a Path>,
    pub review_dir: Option<&'a Path>,
    pub output_path: Option<&'a Path>,
    pub strict: bool,
    pub quiet: bool,
    pub color: ColorChoice,
}

#[derive(Clone, Copy)]
pub struct CompareCommand<'a> {
    pub old_path: Option<&'a Path>,
    pub new_path: Option<&'a Path>,
    pub trace_path: Option<&'a Path>,
    pub old_password_file: Option<&'a Path>,
    pub new_password_file: Option<&'a Path>,
    pub old_font_identities: &'a [String],
    pub new_font_identities: &'a [String],
    pub limit_scale: f64,
    pub options: ComparisonOptions<'a>,
    pub extraction_cache_dir: Option<&'a Path>,
}

#[derive(Clone, Copy)]
pub struct ComparisonInput<'a> {
    pub path: &'a Path,
    pub password_file: Option<&'a Path>,
    pub font_identities: &'a [String],
}

fn parse_limit_scale(value: &str) -> Result<f64, String> {
    let scale = value
        .parse::<f64>()
        .map_err(|error| format!("invalid limit scale {value:?}: {error}"))?;
    validate_pipeline_limit_scale(scale).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, ColorChoice, Command};

    #[test]
    fn parses_strict_comparison_mode() {
        let cli = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--strict"])
            .expect("strict comparison arguments should parse");

        assert!(cli.strict);
        assert_eq!(cli.old.as_deref(), Some(std::path::Path::new("old.pdf")));
        assert_eq!(cli.new.as_deref(), Some(std::path::Path::new("new.pdf")));

        let cli_short = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "-s"])
            .expect("short strict flag should parse");
        assert!(cli_short.strict);
    }

    #[test]
    fn parses_output_and_quiet_flags() {
        let cli = Cli::try_parse_from([
            "pdfdelta", "old.pdf", "new.pdf", "--output", "diff.txt", "--quiet",
        ])
        .expect("output and quiet flags should parse");

        assert_eq!(
            cli.output.as_deref(),
            Some(std::path::Path::new("diff.txt"))
        );
        assert!(cli.quiet);

        let cli_short = Cli::try_parse_from([
            "pdfdelta",
            "old.pdf",
            "new.pdf",
            "-o",
            "diff.txt",
            "-q",
            "-j",
            "diff.json",
        ])
        .expect("short flags should parse");

        assert_eq!(
            cli_short.output.as_deref(),
            Some(std::path::Path::new("diff.txt"))
        );
        assert!(cli_short.quiet);
        assert_eq!(
            cli_short.json.as_deref(),
            Some(std::path::Path::new("diff.json"))
        );
    }

    #[test]
    fn parses_valid_limit_scale_and_rejects_lower_values() {
        let cli = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--limit-scale", "16"])
            .expect("a scale above one should parse");

        assert_eq!(cli.limit_scale, 16.0);
        assert!(
            Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--limit-scale", "0.5"])
                .is_err()
        );
    }

    #[test]
    fn color_defaults_to_auto_and_accepts_explicit_choices() {
        let default = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf"])
            .expect("default comparison arguments should parse");
        assert_eq!(default.color, ColorChoice::Auto);

        let always = Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--color", "always"])
            .expect("always color should parse");
        assert_eq!(always.color, ColorChoice::Always);

        // The flag is global, so subcommand invocations accept it too.
        let inspect = Cli::try_parse_from([
            "pdfdelta",
            "inspect",
            "document.pdf",
            "--glyphs",
            "--color",
            "never",
        ])
        .expect("global color should parse with subcommands");
        assert_eq!(inspect.color, ColorChoice::Never);

        assert!(
            Cli::try_parse_from(["pdfdelta", "old.pdf", "new.pdf", "--color", "sometimes"])
                .is_err()
        );
    }

    #[test]
    fn strict_mode_requires_a_new_document() {
        assert!(Cli::try_parse_from(["pdfdelta", "old.pdf", "--strict"]).is_err());
    }

    #[test]
    fn parses_side_specific_password_and_font_identity_inputs() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "old.pdf",
            "new.pdf",
            "--old-password-file",
            "old.password",
            "--new-password-file",
            "new.password",
            "--old-font-identity",
            "TraditionalArabic=windows-v1",
            "--new-font-identity",
            "TraditionalArabic=windows-v1",
        ])
        .expect("explicit comparison identities should parse");

        assert_eq!(
            cli.old_password_file.as_deref(),
            Some(std::path::Path::new("old.password"))
        );
        assert_eq!(
            cli.new_password_file.as_deref(),
            Some(std::path::Path::new("new.password"))
        );
        assert_eq!(cli.old_font_identity, ["TraditionalArabic=windows-v1"]);
        assert_eq!(cli.new_font_identity, ["TraditionalArabic=windows-v1"]);
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
                objects: false,
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
                objects: false,
                ..
            })
        ));
    }

    #[test]
    fn parses_objects_inspection() {
        let cli = Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--objects"])
            .expect("objects inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: false,
                glyphs: false,
                objects: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_inspection_password_and_font_identity_inputs() {
        let cli = Cli::try_parse_from([
            "pdfdelta",
            "inspect",
            "document.pdf",
            "--glyphs",
            "--password-file",
            "document.password",
            "--font-identity",
            "TraditionalArabic=windows-v1",
        ])
        .expect("inspection identity inputs should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                password_file: Some(path),
                font_identity,
                ..
            }) if path == std::path::Path::new("document.password")
                && font_identity == ["TraditionalArabic=windows-v1"]
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
            "--objects",
        ])
        .expect("combined inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                backend_info: true,
                glyphs: true,
                objects: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_svg_inspection() {
        let cli =
            Cli::try_parse_from(["pdfdelta", "inspect", "document.pdf", "--svg", "debug.svg"])
                .expect("svg inspection arguments should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Inspect {
                svg: Some(path),
                ..
            }) if path == std::path::Path::new("debug.svg")
        ));
    }

    #[test]
    fn parses_completions_subcommand() {
        let cli = Cli::try_parse_from(["pdfdelta", "completions", "bash"])
            .expect("completions subcommand should parse");

        assert!(matches!(
            cli.command,
            Some(Command::Completions {
                shell: clap_complete::Shell::Bash
            })
        ));
    }
}
