use std::{path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};

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
    Inspect { document: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Inspect { document }) => {
            eprintln!("inspection is not implemented yet: {}", document.display());
        }
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

fn path_display(path: &std::path::Path) -> &str {
    path.to_str().unwrap_or("<non-UTF-8 path>")
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;

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
}
