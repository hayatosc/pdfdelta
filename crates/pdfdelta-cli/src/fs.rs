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

use flate2::{Compression, write::GzEncoder};
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
            message: format!("cannot read {target}: PDF input exceeds the {max_bytes}-byte limit"),
            limit: max_bytes,
        });
    }
    Ok(Arc::from(bytes))
}

pub fn read_password_file(path: &Path) -> Result<String, String> {
    if path == Path::new("-") {
        // Standard input carries PDF bytes; reading a password from it would
        // silently consume the document instead, so the collision is rejected.
        return Err(
            "cannot read password file -: standard input is reserved for PDF input".to_owned(),
        );
    }
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
    write_output_atomically(output_path, "trace report", |mut temporary_file| {
        trace.write_json(&mut temporary_file).map_err(|error| {
            format!(
                "cannot render diagnostic trace for {}: {error}",
                output_path.display()
            )
        })
    })
}

/// Writes one output file atomically.
///
/// A destination ending in `.gz` is compressed while it is written, so the
/// temporary file never holds an uncompressed copy. Compression and flush
/// errors fail closed before the file is published.
pub fn write_output_atomically(
    output_path: &Path,
    output_kind: &'static str,
    write: impl FnOnce(&mut dyn Write) -> Result<(), String>,
) -> Result<(), String> {
    let (temporary_path, temporary_file) = create_temporary_output_for(output_path, output_kind)?;
    let mut temporary_file = BufWriter::new(temporary_file);
    let compressed = output_path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"));
    let prepare_result = (|| {
        if compressed {
            // Batch the many small serializer writes into bounded chunks
            // before they reach the compressor. The buffer holds only pending
            // uncompressed bytes in memory; no plaintext file is created.
            let mut encoder = GzEncoder::new(&mut temporary_file, Compression::default());
            let mut buffered = BufWriter::with_capacity(64 * 1024, &mut encoder);
            let write_result = write(&mut buffered);
            let flush_result = buffered.flush();
            drop(buffered);
            let finish_result = encoder.finish();
            write_result?;
            flush_result.map_err(|error| {
                format!(
                    "cannot flush compressed {output_kind} for {}: {error}",
                    output_path.display()
                )
            })?;
            finish_result.map_err(|error| {
                format!(
                    "cannot finish compressed {output_kind} for {}: {error}",
                    output_path.display()
                )
            })?;
        } else {
            write(&mut temporary_file)?;
        }
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
        match create_private_file(&temporary_path) {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
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

/// Opens a new file that no other writer can have created first.
///
/// `create_new` refuses symlinked pre-created names and 0o600 keeps private
/// scratch data readable only by the current user.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    options.open(path)
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

/// Returns whether two paths name the same file, tolerating leaves that do
/// not exist yet.
///
/// Existing files compare by device/inode where available and otherwise by
/// canonical path. When either leaf is missing, both paths are normalized to
/// their absolute destination so lexical aliases of the same future file are
/// still detected.
pub fn paths_refer_to_same_file(
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

    let first_metadata = match fs::metadata(first_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "cannot inspect path {} for {context}: {error}",
                first_path.display()
            ));
        }
    };
    let second_metadata = match fs::metadata(second_path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "cannot inspect path {} for {context}: {error}",
                second_path.display()
            ));
        }
    };
    let (Some(first_metadata), Some(second_metadata)) = (first_metadata, second_metadata) else {
        return Ok(normalized_destination(first_path, context)?
            == normalized_destination(second_path, context)?);
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if first_metadata.dev() == second_metadata.dev()
            && first_metadata.ino() == second_metadata.ino()
        {
            return Ok(true);
        }
    }

    let first_canonical = fs::canonicalize(first_path).map_err(|error| {
        format!(
            "cannot resolve path {} for {context}: {error}",
            first_path.display()
        )
    })?;
    let second_canonical = fs::canonicalize(second_path).map_err(|error| {
        format!(
            "cannot resolve path {} for {context}: {error}",
            second_path.display()
        )
    })?;
    Ok(first_canonical == second_canonical)
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
    use super::{InputReadError, MAX_PASSWORD_FILE_BYTES, read_limited_typed, read_password_file};

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

    #[test]
    fn accepts_input_at_the_byte_limit_and_rejects_one_byte_more() {
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-input-boundary-test-{}.pdf",
            std::process::id()
        ));
        std::fs::write(&path, b"abc").expect("input boundary fixture should be written");

        let exact = read_limited_typed(&path, 3).expect("input at the limit is accepted");
        assert_eq!(exact.as_ref(), b"abc");
        let error = read_limited_typed(&path, 2).expect_err("input above the limit is rejected");
        std::fs::remove_file(path).expect("input boundary fixture should be removed");

        assert!(matches!(
            error,
            InputReadError::LimitExceeded { limit: 2, .. }
        ));
    }

    #[test]
    fn password_files_strip_one_line_ending_and_enforce_their_byte_limit() {
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-password-file-test-{}",
            std::process::id()
        ));

        std::fs::write(&path, b"secret\r\n").expect("CRLF password fixture");
        assert_eq!(
            read_password_file(&path).expect("CRLF password should parse"),
            "secret"
        );
        std::fs::write(&path, b"secret\n").expect("LF password fixture");
        assert_eq!(
            read_password_file(&path).expect("LF password should parse"),
            "secret"
        );
        std::fs::write(&path, b"").expect("empty password fixture");
        assert_eq!(
            read_password_file(&path).expect("empty password should be accepted"),
            ""
        );

        let exact = vec![b'a'; MAX_PASSWORD_FILE_BYTES];
        std::fs::write(&path, &exact).expect("password at the limit");
        assert_eq!(
            read_password_file(&path)
                .expect("password at the limit should parse")
                .len(),
            MAX_PASSWORD_FILE_BYTES
        );

        let oversized = vec![b'a'; MAX_PASSWORD_FILE_BYTES + 1];
        std::fs::write(&path, oversized).expect("oversized password fixture");
        let error = read_password_file(&path).expect_err("oversized password should be rejected");
        std::fs::remove_file(path).expect("password fixture should be removed");

        assert!(error.contains("password file"), "{error}");
    }

    #[test]
    fn existing_aliases_compare_by_identity_and_missing_leaves_by_destination() {
        let directory = std::env::temp_dir().join(format!(
            "pdfdelta-path-alias-test-{}-{}",
            std::process::id(),
            super::NEXT_TEMPORARY_FILE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).expect("alias fixture directory");
        let pdf = directory.join("input.pdf");
        std::fs::write(&pdf, b"pdf").expect("input fixture");
        #[cfg(unix)]
        let link = {
            let link = directory.join("alias.pdf");
            std::os::unix::fs::symlink(&pdf, &link).expect("symlink fixture");
            link
        };

        assert!(
            super::paths_refer_to_same_file(&pdf, &pdf, "self collision").expect("identical paths")
        );
        #[cfg(unix)]
        assert!(
            super::paths_refer_to_same_file(&pdf, &link, "symlink collision")
                .expect("symlinked alias")
        );
        assert!(
            !super::paths_refer_to_same_file(
                &directory.join("future.json"),
                &directory.join("other.json"),
                "distinct missing leaves"
            )
            .expect("distinct future destinations")
        );
        assert!(
            super::paths_refer_to_same_file(
                &directory.join("future.json"),
                &directory.join(".").join("future.json"),
                "lexical alias"
            )
            .expect("lexically aliased future destination")
        );

        std::fs::remove_dir_all(directory).expect("alias fixture cleanup");
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

#[cfg(test)]
mod compressed_output_tests {
    use std::{fs::File, io::Read, path::PathBuf};

    use super::write_output_atomically;

    fn scratch_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pdfdelta-compressed-output-{}-{name}",
            std::process::id()
        ))
    }

    #[test]
    fn gzip_destination_compresses_while_writing_and_refuses_overwrite() {
        let path = scratch_path("report.json.gz");
        write_output_atomically(&path, "JSON report", |writer| {
            writer
                .write_all(b"{\"schema_version\": 11}\n")
                .map_err(|error| error.to_string())
        })
        .expect("gzip output");

        let mut decoder = flate2::read::GzDecoder::new(File::open(&path).expect("gzip file"));
        let mut text = String::new();
        decoder.read_to_string(&mut text).expect("gzip content");
        assert_eq!(text, "{\"schema_version\": 11}\n");

        let error = write_output_atomically(&path, "JSON report", |_| Ok(()))
            .expect_err("existing output must not be overwritten");
        assert!(error.contains("refusing to overwrite"), "{error}");

        // A corrupt gzip payload is detectable from the published file alone.
        std::fs::write(&path, b"not gzip").expect("corrupt fixture");
        let mut decoder = flate2::read::GzDecoder::new(File::open(&path).expect("corrupt file"));
        let mut text = String::new();
        assert!(decoder.read_to_string(&mut text).is_err());

        std::fs::remove_file(&path).expect("cleanup");
    }

    #[test]
    fn plain_destination_stays_uncompressed() {
        let path = scratch_path("report.json");
        write_output_atomically(&path, "JSON report", |writer| {
            writer
                .write_all(b"plain\n")
                .map_err(|error| error.to_string())
        })
        .expect("plain output");
        assert_eq!(std::fs::read(&path).expect("plain file"), b"plain\n");
        std::fs::remove_file(&path).expect("cleanup");
    }
}
