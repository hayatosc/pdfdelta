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
            eprintln!("comparison is not implemented yet: {old} -> {new} ({output})");
        }
    }
    ExitCode::from(2)
}

fn path_display(path: &std::path::Path) -> &str {
    path.to_str().unwrap_or("<non-UTF-8 path>")
}
