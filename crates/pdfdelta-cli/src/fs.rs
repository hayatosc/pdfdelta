use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use pdfdelta_core::{
    diff::Comparison,
    model::GlyphEvidence,
    normalize::BlockText,
    pdf::{LopdfParser, ParseLimits, ParsedPdf, PdfParser},
    report::{ExtractionStatus, write_json},
    source::ExternalFontIdentities,
};

use crate::trace::ExecutionTrace;

pub static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(0);

const MAX_PASSWORD_FILE_BYTES: usize = 4_096;

#[derive(Debug)]
pub enum InputReadError {
    Io(String),
    InvalidConfiguration(String),
    LimitExceeded { message: String, limit: usize },
}

impl fmt::Display for InputReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message)
            | Self::InvalidConfiguration(message)
            | Self::LimitExceeded { message, .. } => formatter.write_str(message),
        }
    }
}

pub fn read_limited(path: &Path, max_bytes: usize) -> Result<Arc<[u8]>, String> {
    read_limited_typed(path, max_bytes).map_err(|error| error.to_string())
}

pub fn read_limited_typed(path: &Path, max_bytes: usize) -> Result<Arc<[u8]>, InputReadError> {
    let read_limit = u64::try_from(max_bytes)
        .map_err(|_| {
            InputReadError::InvalidConfiguration(
                "configured PDF input limit does not fit in u64".to_owned(),
            )
        })?
        .saturating_add(1);
    let bytes = if path == Path::new("-") {
        let stdin = io::stdin();
        let mut reader = stdin.lock().take(read_limit);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| InputReadError::Io(format!("cannot read standard input: {error}")))?;
        bytes
    } else {
        let file = File::open(path).map_err(|error| {
            InputReadError::Io(format!("cannot open {}: {error}", path.display()))
        })?;
        let mut reader = file.take(read_limit);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(|error| {
            InputReadError::Io(format!("cannot read {}: {error}", path.display()))
        })?;
        bytes
    };
    if bytes.len() > max_bytes {
        let target = if path == Path::new("-") {
            "standard input".to_owned()
        } else {
            path.display().to_string()
        };
        return Err(InputReadError::LimitExceeded {
            message: format!("cannot read {target}: PDF input exceeds the {max_bytes}-byte limit",),
            limit: max_bytes,
        });
    }
    Ok(Arc::from(bytes))
}

pub fn read_password_file(path: &Path) -> Result<String, String> {
    let bytes = read_limited_typed(path, MAX_PASSWORD_FILE_BYTES)
        .map_err(|error| format!("cannot read password file {}: {error}", path.display()))?;
    let mut bytes = bytes.as_ref();
    if let Some(without_newline) = bytes.strip_suffix(b"\r\n") {
        bytes = without_newline;
    } else if let Some(without_newline) = bytes.strip_suffix(b"\n") {
        bytes = without_newline;
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| format!("password file {} is not valid UTF-8", path.display()))
}

pub fn parse_external_font_identities(values: &[String]) -> Result<ExternalFontIdentities, String> {
    let mut identities = ExternalFontIdentities::default();
    for value in values {
        let (base_font, identity) = value.split_once('=').ok_or_else(|| {
            format!("external font identity must use BASE_FONT=IDENTITY, got {value:?}")
        })?;
        let base_font = base_font.strip_prefix('/').unwrap_or(base_font);
        identities
            .insert(base_font.as_bytes(), identity.as_bytes())
            .map_err(|error| format!("invalid external font identity for /{base_font}: {error}"))?;
    }
    Ok(identities)
}

pub fn parse_lopdf(
    bytes: Arc<[u8]>,
    limits: ParseLimits,
    password: Option<&str>,
) -> pdfdelta_core::Result<Box<dyn ParsedPdf>> {
    match password {
        Some(password) => LopdfParser.parse_with_password(bytes, limits, password),
        None => LopdfParser.parse(bytes, limits),
    }
}

pub fn write_json_atomically(
    output_path: &Path,
    old_blocks: &[BlockText],
    new_blocks: &[BlockText],
    old_glyph_evidence: &[GlyphEvidence],
    new_glyph_evidence: &[GlyphEvidence],
    comparison: &Comparison,
    extraction: &ExtractionStatus,
) -> Result<(), String> {
    write_output_atomically(output_path, "JSON report", |temporary_file| {
        write_json(
            temporary_file,
            old_blocks,
            new_blocks,
            old_glyph_evidence,
            new_glyph_evidence,
            comparison,
            extraction,
        )
        .map_err(|error| {
            format!(
                "cannot render JSON comparison report for {}: {error}",
                output_path.display()
            )
        })
    })
}

pub fn write_text_report_atomically(output_path: &Path, content: &str) -> Result<(), String> {
    write_output_atomically(output_path, "text report", |temporary_file| {
        temporary_file
            .write_all(content.as_bytes())
            .map_err(|error| {
                format!(
                    "cannot render text comparison report for {}: {error}",
                    output_path.display()
                )
            })
    })
}

pub fn write_trace_atomically(output_path: &Path, trace: &ExecutionTrace) -> Result<(), String> {
    write_output_atomically(output_path, "trace report", |temporary_file| {
        trace.write_json(temporary_file).map_err(|error| {
            format!(
                "cannot render diagnostic trace for {}: {error}",
                output_path.display()
            )
        })
    })
}

pub fn write_output_atomically(
    output_path: &Path,
    output_kind: &'static str,
    write: impl FnOnce(&mut BufWriter<File>) -> Result<(), String>,
) -> Result<(), String> {
    let (temporary_path, temporary_file) = create_temporary_output_for(output_path, output_kind)?;
    let mut temporary_file = BufWriter::new(temporary_file);
    let prepare_result = (|| {
        write(&mut temporary_file)?;
        temporary_file.flush().map_err(|error| {
            format!(
                "cannot flush temporary {output_kind} for {}: {error}",
                output_path.display()
            )
        })?;
        temporary_file.get_ref().sync_all().map_err(|error| {
            format!(
                "cannot sync temporary {output_kind} for {}: {error}",
                output_path.display()
            )
        })
    })();
    drop(temporary_file);

    if let Err(error) = prepare_result {
        return Err(error_with_temporary_cleanup(
            &temporary_path,
            output_kind,
            error,
        ));
    }

    if let Err(error) = fs::hard_link(&temporary_path, output_path) {
        let message = if error.kind() == io::ErrorKind::AlreadyExists {
            format!(
                "refusing to overwrite existing {output_kind} {}: output path already exists",
                output_path.display()
            )
        } else {
            format!(
                "cannot publish {output_kind} {} atomically without replacing an existing file: {error}",
                output_path.display()
            )
        };
        return Err(error_with_temporary_cleanup(
            &temporary_path,
            output_kind,
            message,
        ));
    }

    match fs::remove_file(&temporary_path) {
        Ok(()) => Ok(()),
        // The output was already published successfully; failing to delete
        // the temporary file is a leftover-file nuisance, not an execution
        // error, so it must not flip the exit code.
        Err(error) => {
            eprintln!(
                "warning: {output_kind} {} was published without overwriting an existing file, but temporary output {} could not be removed: {error}",
                output_path.display(),
                temporary_path.display()
            );
            Ok(())
        }
    }
}

pub fn error_with_temporary_cleanup(
    temporary_path: &Path,
    output_kind: &str,
    primary_error: String,
) -> String {
    match fs::remove_file(temporary_path) {
        Ok(()) => primary_error,
        Err(cleanup_error) => format!(
            "{primary_error}; temporary {output_kind} {} could not be removed: {cleanup_error}",
            temporary_path.display()
        ),
    }
}

#[cfg(test)]
pub fn create_temporary_output(output_path: &Path) -> Result<(PathBuf, File), String> {
    create_temporary_output_for(output_path, "JSON report")
}

pub fn create_temporary_output_for(
    output_path: &Path,
    output_kind: &str,
) -> Result<(PathBuf, File), String> {
    output_path.file_name().ok_or_else(|| {
        format!(
            "{output_kind} path must name a file: {}",
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
                    "cannot create temporary {output_kind} next to {}: {error}",
                    output_path.display()
                ));
            }
        }
    }

    Err(format!(
        "cannot create a unique temporary {output_kind} next to {}",
        output_path.display()
    ))
}

pub fn ensure_output_does_not_alias_input(
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    ensure_named_output_does_not_alias_input("JSON output", output_path, old_path, new_path)
}

pub fn ensure_trace_does_not_alias_input(
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    ensure_named_output_does_not_alias_input("trace output", output_path, old_path, new_path)
}

pub fn ensure_named_output_does_not_alias_input(
    output_kind: &str,
    output_path: &Path,
    old_path: &Path,
    new_path: &Path,
) -> Result<(), String> {
    for (side, input_path) in [("old", old_path), ("new", new_path)] {
        if paths_refer_to_same_file(output_path, input_path, "input collision")? {
            return Err(format!(
                "refusing {output_kind} {} because it refers to the {side} PDF {}",
                output_path.display(),
                input_path.display()
            ));
        }
    }
    Ok(())
}

pub fn paths_refer_to_same_file(
    output_path: &Path,
    input_path: &Path,
    context: &str,
) -> Result<bool, String> {
    if output_path == Path::new("-") || input_path == Path::new("-") {
        return Ok(false);
    }
    if output_path == input_path {
        return Ok(true);
    }

    let output_metadata = match fs::metadata(output_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // The output leaf does not exist yet, so inode comparison is
            // impossible; fall back to normalized destination comparison so
            // lexical aliases of the same future file are still rejected.
            return Ok(normalized_destination(output_path, context)?
                == normalized_destination(input_path, context)?);
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect output path {} for {context}: {error}",
                output_path.display()
            ));
        }
    };
    let input_metadata = fs::metadata(input_path).map_err(|error| {
        format!(
            "cannot inspect input path {} for {context}: {error}",
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
            "cannot resolve output path {} for {context}: {error}",
            output_path.display()
        )
    })?;
    let input_canonical = fs::canonicalize(input_path).map_err(|error| {
        format!(
            "cannot resolve input path {} for {context}: {error}",
            input_path.display()
        )
    })?;
    Ok(output_canonical == input_canonical)
}

pub fn output_paths_refer_to_same_file(
    first_path: &Path,
    second_path: &Path,
    context: &str,
) -> Result<bool, String> {
    if first_path == Path::new("-") || second_path == Path::new("-") {
        return Ok(false);
    }
    if first_path == second_path {
        return Ok(true);
    }
    match fs::metadata(second_path) {
        Ok(_) => paths_refer_to_same_file(first_path, second_path, context),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(normalized_destination(first_path, context)?
                == normalized_destination(second_path, context)?)
        }
        Err(error) => Err(format!(
            "cannot inspect output path {} for {context}: {error}",
            second_path.display()
        )),
    }
}

/// Resolve a path to its normalized absolute destination without requiring
/// the leaf to exist: canonicalize the deepest existing ancestor (following
/// symlinks in existing parents), then append the lexically normalized
/// remaining components.
pub fn normalized_destination(path: &Path, context: &str) -> Result<PathBuf, String> {
    let mut accumulated = PathBuf::new();
    let mut deepest_existing = None;
    let mut deepest_component_count = 0_usize;
    for (index, component) in path.components().enumerate() {
        accumulated.push(component);
        if fs::symlink_metadata(&accumulated).is_ok() {
            deepest_existing = Some(accumulated.clone());
            deepest_component_count = index + 1;
        }
    }

    let mut normalized = match &deepest_existing {
        Some(existing) => fs::canonicalize(existing).map_err(|error| {
            format!(
                "cannot resolve output path {} for {context}: {error}",
                path.display()
            )
        })?,
        // Nothing along the path exists yet; anchor the relative components
        // at the current working directory.
        None => std::env::current_dir().map_err(|error| {
            format!(
                "cannot resolve output path {} for {context}: {error}",
                path.display()
            )
        })?,
    };
    for component in path.components().skip(deepest_component_count) {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{InputReadError, read_limited_typed};

    #[test]
    fn classifies_input_size_limits_separately_from_io_failures() {
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-input-limit-test-{}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, b"four").expect("input limit fixture should be written");

        let error = read_limited_typed(&path, 3).expect_err("input should exceed the limit");
        std::fs::remove_file(path).expect("input limit fixture should be removed");

        assert!(matches!(
            error,
            InputReadError::LimitExceeded { limit: 3, .. }
        ));
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
}
