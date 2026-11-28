//! Temporary, owner-only case stores for one agent-review plan.
//!
//! The planner's ordering index is small and stays resident. Case bodies are
//! the part that grows with the case count, so they are written to a spool in
//! a private temporary directory and read back one at a time by the publisher.
//! The directory is removed when the plan and its writer are dropped, whether
//! publication succeeded, failed a limit, or returned an I/O error.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use pdfdelta_core::review::{CaseContext, CaseStore, EvidenceRef, PlanError, ReviewCase};
use pdfdelta_diff::Error as DiffError;
use serde::{Deserialize, Serialize};

/// Largest single case record a spool will read back.
///
/// The spool is written by this process, so the cap only turns a truncated or
/// corrupted file into a typed failure instead of an unbounded allocation.
const MAX_RECORD_BYTES: usize = 256 * 1024 * 1024;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize, Deserialize)]
struct SpoolRecord {
    case: ReviewCase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<CaseContext>,
}

/// One append-only case file.
///
/// Handles are positions in the offset table. Appending evidence writes a new
/// record at the end and repoints the handle; old records stay unreachable and
/// are reclaimed with the directory.
pub(crate) struct FileCaseStore {
    writer: File,
    reader: File,
    offsets: Vec<u64>,
    end: u64,
}

impl FileCaseStore {
    fn create(path: &Path) -> Result<Self, DiffError> {
        let writer = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                DiffError::backend(format!(
                    "cannot create review case spool {}: {error}",
                    path.display()
                ))
            })?;
        // A separate descriptor is required: `try_clone` duplicates the
        // shared file offset on Unix, so a read would move the writer and the
        // next append would land inside the record that was just read.
        let reader = File::open(path).map_err(|error| {
            DiffError::backend(format!(
                "cannot open review case spool {} for reading: {error}",
                path.display()
            ))
        })?;
        Ok(Self {
            writer,
            reader,
            offsets: Vec::new(),
            end: 0,
        })
    }

    fn append(&mut self, record: &SpoolRecord) -> Result<u64, PlanError> {
        let bytes = serde_json::to_vec(record)
            .map_err(|error| PlanError::new(format!("cannot serialize a review case: {error}")))?;
        let length = u64::try_from(bytes.len())
            .map_err(|_| PlanError::new("review case record exceeds the supported size"))?;
        let offset = self.end;
        self.writer
            .write_all(&length.to_le_bytes())
            .and_then(|()| self.writer.write_all(&bytes))
            .map_err(|error| {
                PlanError::new(format!("cannot write the review case spool: {error}"))
            })?;
        self.end = self.end.saturating_add(8).saturating_add(length);
        Ok(offset)
    }

    fn read(&mut self, offset: u64) -> Result<SpoolRecord, PlanError> {
        self.reader.seek(SeekFrom::Start(offset)).map_err(|error| {
            PlanError::new(format!("cannot seek the review case spool: {error}"))
        })?;
        let mut length = [0_u8; 8];
        self.reader.read_exact(&mut length).map_err(|error| {
            PlanError::new(format!(
                "cannot read the review case spool record length: {error}"
            ))
        })?;
        let length = usize::try_from(u64::from_le_bytes(length))
            .map_err(|_| PlanError::new("review case record length is not addressable"))?;
        if length > MAX_RECORD_BYTES {
            return Err(PlanError::new(format!(
                "review case record is {length} bytes; the limit is {MAX_RECORD_BYTES}"
            )));
        }
        let mut bytes = vec![0_u8; length];
        self.reader.read_exact(&mut bytes).map_err(|error| {
            PlanError::new(format!("cannot read the review case spool record: {error}"))
        })?;
        serde_json::from_slice(&bytes).map_err(|error| {
            PlanError::new(format!(
                "cannot parse the review case spool record: {error}"
            ))
        })
    }
}

impl CaseStore for FileCaseStore {
    fn store(
        &mut self,
        case: ReviewCase,
        context: Option<CaseContext>,
    ) -> Result<usize, PlanError> {
        let offset = self.append(&SpoolRecord { case, context })?;
        self.offsets.push(offset);
        Ok(self.offsets.len() - 1)
    }

    fn load(&mut self, handle: usize) -> Result<(ReviewCase, Option<CaseContext>), PlanError> {
        let Some(offset) = self.offsets.get(handle).copied() else {
            return Err(PlanError::new(format!(
                "review case handle {handle} is outside the spool"
            )));
        };
        let record = self.read(offset)?;
        Ok((record.case, record.context))
    }

    fn append_evidence(
        &mut self,
        handle: usize,
        evidence: &[EvidenceRef],
    ) -> Result<(), PlanError> {
        let (mut case, context) = self.load(handle)?;
        let mut retained: BTreeSet<EvidenceRef> = case.evidence.iter().copied().collect();
        for reference in evidence {
            if retained.insert(*reference) {
                case.evidence.push(*reference);
            }
        }
        let offset = self.append(&SpoolRecord { case, context })?;
        let slot = self.offsets.get_mut(handle).ok_or_else(|| {
            PlanError::new(format!("review case handle {handle} is outside the spool"))
        })?;
        *slot = offset;
        Ok(())
    }
}

/// A temporary directory that removes itself when dropped.
struct TempDirectory {
    path: PathBuf,
}

impl TempDirectory {
    fn create() -> Result<Self, DiffError> {
        loop {
            let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "pdfdelta-agent-plan-{}-{sequence}",
                std::process::id()
            ));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(DiffError::backend(format!(
                        "cannot create the review plan directory {}: {error}",
                        path.display()
                    )));
                }
            }
        }
    }
}

impl Drop for TempDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The raw and final case stores of one plan.
///
/// Fields drop in declaration order, so both files close before the temporary
/// directory is removed.
pub(crate) struct PlanSpool {
    pub(crate) raw: FileCaseStore,
    pub(crate) final_store: FileCaseStore,
    _directory: TempDirectory,
}

impl PlanSpool {
    pub(crate) fn new() -> Result<Self, DiffError> {
        let directory = TempDirectory::create()?;
        let raw = FileCaseStore::create(&directory.path.join("raw.cases"))?;
        let final_store = FileCaseStore::create(&directory.path.join("final.cases"))?;
        Ok(Self {
            raw,
            final_store,
            _directory: directory,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfdelta_core::{document::SourceRef, model::GlyphId, review::Side};

    fn case(identifier: &str) -> ReviewCase {
        serde_json::from_value(serde_json::json!({
            "case_id": identifier,
            "content_digest": "00".repeat(32),
            "question": "compare_content",
            "pipeline": "native_text",
            "engine_class": "strict",
            "finding": "difference_established",
            "channels": ["text"],
            "completeness": {
                "evidence": "complete",
                "candidate_enumeration": "complete",
                "solver_search": "complete",
                "response": "complete"
            },
            "old_text": {
                "side": "old",
                "text": "abc",
                "scalar_count": 3,
                "order": "render_order"
            },
            "alternatives_returned": 0
        }))
        .expect("fixture case")
    }

    #[test]
    fn evidence_merge_does_not_shift_the_read_cursor_of_later_records() {
        let spool = PlanSpool::new().expect("spool");
        let mut store = spool.final_store;
        let first = store.store(case("R0"), None).expect("first");
        let second = store.store(case("R1"), None).expect("second");
        store
            .append_evidence(
                first,
                &[EvidenceRef::new(
                    Side::Old,
                    SourceRef::Native { glyph: GlyphId(7) },
                )],
            )
            .expect("evidence merge");
        let (second_case, _) = store.load(second).expect("second record");
        assert_eq!(second_case.case_id.as_str(), "R1");
        let (first_case, _) = store.load(first).expect("first record");
        assert_eq!(first_case.evidence.len(), 1);
    }
}
