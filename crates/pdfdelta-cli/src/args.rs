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

    /// Create an agent review bundle: a manifest, per-case packets, and the
    /// source PDFs, for bounded retrieval with `pdfdelta review`.
    ///
    /// The comparison result, its coverage, and its exit status are unchanged.
    #[arg(long, value_name = "DIR", requires = "new", conflicts_with = "review")]
    pub agent_review: Option<PathBuf>,

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

    /// Read an existing agent review bundle within an explicit byte budget.
    ///
    /// A path whose first component is literally `review` must be written as
    /// `./review` so it is not read as this subcommand.
    Review {
        #[command(subcommand)]
        action: ReviewCommand,
    },

    /// Generate shell completion script for the specified shell.
    Completions {
        /// Target shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    /// List unretrieved cases, newest budget first.
    List {
        /// The bundle directory written by `--agent-review`.
        #[arg(value_name = "DIR")]
        directory: PathBuf,

        /// Continue a previous listing of the same bundle.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,

        /// Hard cap on the encoded JSON response, including its metadata.
        #[arg(long, value_name = "BYTES", default_value_t = 8192, value_parser = parse_output_budget)]
        max_output_bytes: usize,
    },

    /// Produce local images for one case from the bundle's retained pages.
    Render {
        /// The bundle directory written by `--agent-review`.
        #[arg(value_name = "DIR")]
        directory: PathBuf,

        /// Case identifier from a listing.
        #[arg(long, value_name = "CASE")]
        case: String,

        /// New directory to write the images into.
        #[arg(long, value_name = "DIR")]
        output: PathBuf,

        /// Hard cap on the encoded JSON response, including its metadata.
        #[arg(long, value_name = "BYTES", default_value_t = 16384, value_parser = parse_output_budget)]
        max_output_bytes: usize,
    },

    /// Read one case at one detail level.
    Show {
        /// The bundle directory written by `--agent-review`.
        #[arg(value_name = "DIR")]
        directory: PathBuf,

        /// Case identifier from a listing.
        #[arg(long, value_name = "CASE")]
        case: String,

        /// How much of the case to return.
        #[arg(long, value_enum, default_value = "text")]
        detail: ReviewDetail,

        /// Continue a previous paged view of the same case.
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,

        /// Hard cap on the encoded JSON response, including its metadata.
        #[arg(long, value_name = "BYTES", default_value_t = 16384, value_parser = parse_output_budget)]
        max_output_bytes: usize,
    },
}

/// Detail levels this build serves.
///
/// Visual retrieval is served by `review render` rather than by a detail level,
/// so it is not offered here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ReviewDetail {
    /// Identity, question, location, and what the case still needs.
    Index,
    /// The question, both sides' retained text, reasons, and evidence.
    Text,
    /// Enclosing headings, neighbours, table structure, and other occurrences.
    Context,
    /// The competing hypotheses.
    Alternatives,
}

impl From<ReviewDetail> for pdfdelta_core::review::Detail {
    fn from(detail: ReviewDetail) -> Self {
        match detail {
            ReviewDetail::Index => Self::Index,
            ReviewDetail::Text => Self::Text,
            ReviewDetail::Context => Self::Context,
            ReviewDetail::Alternatives => Self::Alternatives,
        }
    }
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
    pub agent_review_dir: Option<&'a Path>,
    /// Resource-limit scale, retained because it is part of what a review
    /// bundle's identity is bound to: a different search budget can produce a
    /// different comparison from the same inputs.
    pub limit_scale: f64,
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

/// Rejects a budget too small to carry any answer.
///
/// Refusing here is clearer than accepting the value and failing on every
/// query with a budget error.
fn parse_output_budget(value: &str) -> Result<usize, String> {
    let budget = value
        .parse::<usize>()
        .map_err(|error| format!("invalid output budget {value:?}: {error}"))?;
    if budget < crate::agent_review::MIN_OUTPUT_BYTES {
        return Err(format!(
            "an output budget of {budget} bytes cannot carry a response; the minimum is {}",
            crate::agent_review::MIN_OUTPUT_BYTES
        ));
    }
    Ok(budget)
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

    use super::{Cli, ColorChoice, Command, ReviewCommand, ReviewDetail};

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
    fn agent_review_pairs_with_the_native_text_contract_but_not_the_html_bundle() {
        let native = Cli::try_parse_from([
            "pdfdelta",
            "old.pdf",
            "new.pdf",
            "--native-text-only",
            "--agent-review",
            "run",
        ])
        .expect("the native-text contract publishes its own bundle");
        assert!(native.native_text_only);
        assert_eq!(
            native.agent_review.as_deref(),
            Some(std::path::Path::new("run"))
        );

        // Two bundles in one run would publish into two destinations from one
        // comparison; the first version refuses the combination outright.
        assert!(
            Cli::try_parse_from([
                "pdfdelta",
                "old.pdf",
                "new.pdf",
                "--review",
                "a",
                "--agent-review",
                "b",
            ])
            .is_err()
        );
    }

    #[test]
    fn the_review_subcommand_takes_precedence_over_a_positional_named_review() {
        let query = Cli::try_parse_from(["pdfdelta", "review", "list", "bundle"])
            .expect("review list should parse");
        assert!(matches!(
            query.command,
            Some(Command::Review {
                action: ReviewCommand::List { .. }
            })
        ));

        // A file actually named `review` therefore has to be spelled with a
        // path prefix, which the subcommand's help states.
        let comparison = Cli::try_parse_from(["pdfdelta", "./review", "new.pdf"])
            .expect("a prefixed path is still a comparison");
        assert_eq!(
            comparison.old.as_deref(),
            Some(std::path::Path::new("./review"))
        );
        assert!(comparison.command.is_none());
    }

    #[test]
    fn an_output_budget_below_the_minimum_is_rejected() {
        assert!(
            Cli::try_parse_from([
                "pdfdelta",
                "review",
                "list",
                "bundle",
                "--max-output-bytes",
                "16"
            ])
            .is_err()
        );
        let accepted = Cli::try_parse_from([
            "pdfdelta",
            "review",
            "show",
            "bundle",
            "--case",
            "R17",
            "--detail",
            "alternatives",
            "--max-output-bytes",
            "4096",
        ])
        .expect("a sufficient budget parses");
        assert!(matches!(
            accepted.command,
            Some(Command::Review {
                action: ReviewCommand::Show {
                    detail: ReviewDetail::Alternatives,
                    max_output_bytes: 4096,
                    ..
                }
            })
        ));
    }

    #[test]
    fn unserved_detail_levels_are_not_offered() {
        let context = Cli::try_parse_from([
            "pdfdelta", "review", "show", "bundle", "--case", "R17", "--detail", "context",
        ])
        .expect("context retrieval is served");
        assert!(matches!(
            context.command,
            Some(Command::Review {
                action: ReviewCommand::Show {
                    detail: ReviewDetail::Context,
                    ..
                }
            })
        ));

        // Images are produced by `review render`, so `--detail visual` is not
        // a request this parser accepts.
        assert!(
            Cli::try_parse_from([
                "pdfdelta", "review", "show", "bundle", "--case", "R17", "--detail", "visual",
            ])
            .is_err()
        );
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
