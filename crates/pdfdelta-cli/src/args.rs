use std::{
    io,
    path::{Path, PathBuf},
};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "pdfdelta",
    version,
    about = "Compare meaningful text changes between two PDF documents",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(value_name = "OLD_PDF")]
    pub old: Option<PathBuf>,

    #[arg(value_name = "NEW_PDF")]
    pub new: Option<PathBuf>,

    #[arg(long, value_name = "PATH", requires = "new")]
    pub json: Option<PathBuf>,

    /// Write a phase-by-phase diagnostic trace to a new JSON file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub trace_json: Option<PathBuf>,

    #[arg(long, requires = "new")]
    pub strict: bool,

    /// When to colorize the human-readable report: auto, always, or never.
    #[arg(
        long,
        value_name = "WHEN",
        value_enum,
        default_value = "auto",
        global = true
    )]
    pub color: ColorChoice,

    /// Read the old PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub old_password_file: Option<PathBuf>,

    /// Read the new PDF password from a file.
    #[arg(long, value_name = "PATH", requires = "new")]
    pub new_password_file: Option<PathBuf>,

    /// Assert an external font identity as BASE_FONT=IDENTITY for the old PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    pub old_font_identity: Vec<String>,

    /// Assert an external font identity as BASE_FONT=IDENTITY for the new PDF.
    #[arg(long, value_name = "BASE_FONT=IDENTITY", requires = "new")]
    pub new_font_identity: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Inspect evidence extracted from one PDF.
    Inspect {
        document: PathBuf,

        /// Print the selected parser backend and parsed document summary.
        #[arg(long)]
        backend_info: bool,

        /// Print extracted glyph evidence.
        #[arg(long)]
        glyphs: bool,

        /// Read the PDF password from a file.
        #[arg(long, value_name = "PATH")]
        password_file: Option<PathBuf>,

        /// Assert an external font identity as BASE_FONT=IDENTITY.
        #[arg(long, value_name = "BASE_FONT=IDENTITY")]
        font_identity: Vec<String>,
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
pub struct CompareCommand<'a> {
    pub old_path: Option<&'a Path>,
    pub new_path: Option<&'a Path>,
    pub json_path: Option<&'a Path>,
    pub trace_path: Option<&'a Path>,
    pub strict: bool,
    pub color: ColorChoice,
    pub old_password_file: Option<&'a Path>,
    pub new_password_file: Option<&'a Path>,
    pub old_font_identities: &'a [String],
    pub new_font_identities: &'a [String],
}

#[derive(Clone, Copy)]
pub struct ComparisonInput<'a> {
    pub path: &'a Path,
    pub password_file: Option<&'a Path>,
    pub font_identities: &'a [String],
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
}
