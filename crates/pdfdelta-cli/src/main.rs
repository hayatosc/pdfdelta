use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};

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
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Inspect {
            document,
            backend_info: true,
        }) => match inspect_backend(&document) {
            Ok(()) => return ExitCode::SUCCESS,
            Err(error) => eprintln!("{error}"),
        },
        Some(Command::Inspect {
            document,
            backend_info: false,
        }) => {
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

fn inspect_backend(path: &Path) -> Result<(), String> {
    let limits = ParseLimits::default();
    let bytes = read_limited(path, limits.max_input_bytes)?;
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

    use super::{Cli, Command};

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
                ..
            })
        ));
    }
}
