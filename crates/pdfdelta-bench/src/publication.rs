//! Atomic publication of new artifact files.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{BenchError, Result};

pub(crate) const MAX_TEMP_CREATE_ATTEMPTS: usize = 64;

static TEMP_PUBLISH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically publishes `bytes` to a new file at `path`, refusing to overwrite
/// any existing destination file, symlink, or directory. Cleans up temporary
/// files on failure.
pub fn publish_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_new_file_with_counter(path, bytes, &TEMP_PUBLISH_COUNTER)
}

/// Testable variant of [`publish_new_file`] with an explicit temporary-name
/// counter so collision handling can be exercised deterministically.
pub(crate) fn publish_new_file_with_counter(
    path: &Path,
    bytes: &[u8],
    counter: &AtomicU64,
) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    if !parent.is_dir() {
        return Err(BenchError::Publication(format!(
            "destination directory does not exist: {}",
            parent.display()
        )));
    }

    if path.symlink_metadata().is_ok() {
        return Err(BenchError::Publication(format!(
            "destination path already exists: {}",
            path.display()
        )));
    }

    let (mut temp_file, temp_path) = create_temp_artifact_with_counter(parent, counter)?;

    let write_result = (|| -> Result<()> {
        temp_file.write_all(bytes).map_err(|error| {
            BenchError::Publication(format!(
                "cannot write temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.flush().map_err(|error| {
            BenchError::Publication(format!(
                "cannot flush temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        temp_file.sync_all().map_err(|error| {
            BenchError::Publication(format!(
                "cannot sync temporary artifact {}: {error}",
                temp_path.display()
            ))
        })?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    let link_result = fs::hard_link(&temp_path, path);
    let _ = fs::remove_file(&temp_path);

    link_result.map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            BenchError::Publication(format!(
                "destination path already exists: {}",
                path.display()
            ))
        } else {
            BenchError::Publication(format!(
                "cannot publish artifact to {}: {error}",
                path.display()
            ))
        }
    })
}

fn create_temp_artifact_with_counter(
    parent: &Path,
    counter: &AtomicU64,
) -> Result<(File, PathBuf)> {
    let pid = std::process::id();
    for _ in 0..MAX_TEMP_CREATE_ATTEMPTS {
        let seq = counter.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(".pdfbench-artifact-{pid}-{seq}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((file, temp_path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(BenchError::Publication(format!(
                    "cannot create temporary artifact {}: {error}",
                    temp_path.display()
                )));
            }
        }
    }
    Err(BenchError::Publication(format!(
        "exhausted {MAX_TEMP_CREATE_ATTEMPTS} attempts creating unique temporary artifact in {}",
        parent.display()
    )))
}
