//! Isolated candidate-generator profiling on deterministic synthetic features.
//!
//! Each generator should be measured in its own process so its index and
//! query allocations cannot overlap another generator's memory footprint.
//! Latency and Linux process-memory observations are diagnostic single-run
//! evidence, not statistically stable benchmark measurements.

use std::{collections::HashSet, fs::OpenOptions, io::Write, path::Path, time::Instant};

use pdfdelta_core::{
    alignment::{
        BlockFeatures, CandidateGenerator, ExactHash, ExhaustiveCandidateGenerator,
        InvertedIndexCandidateGenerator, MinHashLshCandidateGenerator, NGram,
    },
    layout::{BlockId, BlockRole},
    normalize::ComparableToken,
};
use serde::{Deserialize, Serialize};

use crate::{BenchError, Result};

pub const DEFAULT_SYNTHETIC_PROFILE_BLOCKS: usize = 1_000;
pub const MIN_SYNTHETIC_PROFILE_BLOCKS: usize = 2;
pub const MAX_SYNTHETIC_PROFILE_BLOCKS: usize = 10_000;

const SYNTHETIC_GROUP_SIZE: usize = 32;
const SYNTHETIC_NGRAM_SIZE: usize = 3;

/// Candidate generator measured by one isolated profiling worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateProfileGenerator {
    InvertedIndex,
    MinhashLsh,
    Exhaustive,
}

impl CandidateProfileGenerator {
    pub const fn label(self) -> &'static str {
        match self {
            Self::InvertedIndex => "inverted-index",
            Self::MinhashLsh => "minhash-lsh",
            Self::Exhaustive => "exhaustive",
        }
    }
}

/// Process-memory source used by candidate profiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateProfileMemorySource {
    LinuxProcStatus,
}

/// Diagnostic observations for one candidate generator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateProfileRecord {
    pub generator: CandidateProfileGenerator,
    pub blocks: usize,
    pub top_k: Vec<usize>,
    pub recall_at_k: Vec<f64>,
    pub candidate_count_p50: usize,
    pub candidate_count_p95: usize,
    pub candidate_count_max: usize,
    pub index_build_latency_ns: u128,
    pub query_latency_ns: u128,
    pub memory_source: CandidateProfileMemorySource,
    /// Resident memory after the common feature corpus is constructed and
    /// immediately before the selected generator is built.
    pub rss_before_build_bytes: u64,
    /// Peak resident memory observed after index construction and all queries.
    pub peak_rss_bytes: u64,
    /// Saturating difference between `peak_rss_bytes` and
    /// `rss_before_build_bytes`; allocator retention means this is diagnostic
    /// process growth rather than exact index ownership.
    pub peak_rss_growth_bytes: u64,
}

impl CandidateProfileRecord {
    /// Whether every requested K retained the known identity counterpart.
    pub fn healthy(&self) -> bool {
        self.top_k.len() == self.recall_at_k.len()
            && !self.top_k.is_empty()
            && self.recall_at_k.iter().all(|recall| *recall == 1.0)
    }
}

/// Profiles one candidate generator against an identity-matched synthetic
/// corpus. Callers should run different generators in separate processes.
pub fn profile_synthetic_candidate_generator(
    blocks: usize,
    top_k: &[usize],
    generator: CandidateProfileGenerator,
) -> Result<CandidateProfileRecord> {
    validate_profile_options(blocks, top_k)?;
    let features = synthetic_features(blocks)?;
    let memory_before = read_process_memory()?;

    match generator {
        CandidateProfileGenerator::InvertedIndex => {
            let started = Instant::now();
            let candidate_generator = InvertedIndexCandidateGenerator::new(&features)
                .map_err(|error| core_error("candidate profile inverted index build", error))?;
            profile_with_generator(
                &features,
                top_k,
                generator,
                candidate_generator,
                started.elapsed().as_nanos(),
                memory_before,
            )
        }
        CandidateProfileGenerator::MinhashLsh => {
            let started = Instant::now();
            let candidate_generator = MinHashLshCandidateGenerator::new(&features)
                .map_err(|error| core_error("candidate profile MinHash index build", error))?;
            profile_with_generator(
                &features,
                top_k,
                generator,
                candidate_generator,
                started.elapsed().as_nanos(),
                memory_before,
            )
        }
        CandidateProfileGenerator::Exhaustive => {
            let started = Instant::now();
            let candidate_generator = ExhaustiveCandidateGenerator::new(&features)
                .map_err(|error| core_error("candidate profile exhaustive build", error))?;
            profile_with_generator(
                &features,
                top_k,
                generator,
                candidate_generator,
                started.elapsed().as_nanos(),
                memory_before,
            )
        }
    }
}

/// Writes profile records as a pretty JSON array to a new file.
pub fn write_candidate_profiles_json(
    path: &Path,
    records: &[CandidateProfileRecord],
) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            BenchError::InvalidInput(format!(
                "cannot create candidate profile JSON output {}: {error}",
                path.display()
            ))
        })?;
    let bytes = serde_json::to_vec_pretty(records).map_err(|error| {
        BenchError::InvalidInput(format!("cannot serialize candidate profile JSON: {error}"))
    })?;
    file.write_all(&bytes).map_err(|error| {
        BenchError::InvalidInput(format!("cannot write candidate profile JSON: {error}"))
    })
}

fn validate_profile_options(blocks: usize, top_k: &[usize]) -> Result<()> {
    if !(MIN_SYNTHETIC_PROFILE_BLOCKS..=MAX_SYNTHETIC_PROFILE_BLOCKS).contains(&blocks) {
        return Err(BenchError::InvalidInput(format!(
            "candidate profile block count must be between {MIN_SYNTHETIC_PROFILE_BLOCKS} and {MAX_SYNTHETIC_PROFILE_BLOCKS}"
        )));
    }
    if top_k.is_empty() {
        return Err(BenchError::InvalidInput(
            "candidate profile requires at least one K value".to_owned(),
        ));
    }
    let mut seen = HashSet::with_capacity(top_k.len());
    for &k in top_k {
        if k == 0 {
            return Err(BenchError::InvalidInput(
                "candidate profile K values must be greater than zero".to_owned(),
            ));
        }
        if !seen.insert(k) {
            return Err(BenchError::InvalidInput(format!(
                "candidate profile K value {k} is listed more than once"
            )));
        }
    }
    Ok(())
}

fn synthetic_features(blocks: usize) -> Result<Vec<BlockFeatures>> {
    let mut features = Vec::with_capacity(blocks);
    for index in 0..blocks {
        let group_offset = u32::try_from(index / SYNTHETIC_GROUP_SIZE).map_err(|_| {
            BenchError::InvalidInput("candidate profile group index overflowed".to_owned())
        })?;
        let unique_offset = u32::try_from(index).map_err(|_| {
            BenchError::InvalidInput("candidate profile block index overflowed".to_owned())
        })?;
        let group = char::from_u32(0xE000 + group_offset).ok_or_else(|| {
            BenchError::InvalidInput("candidate profile group scalar overflowed".to_owned())
        })?;
        let unique = char::from_u32(0xF_0000 + unique_offset).ok_or_else(|| {
            BenchError::InvalidInput("candidate profile unique scalar overflowed".to_owned())
        })?;
        let matching_tokens = [group, group, group, unique, unique, unique]
            .into_iter()
            .map(ComparableToken::Scalar)
            .collect::<Vec<_>>();
        let ngram_counts = matching_tokens
            .windows(SYNTHETIC_NGRAM_SIZE)
            .map(|window| NGram(window.to_vec()))
            .map(|ngram| (ngram, 1))
            .collect();
        let block_id = u64::try_from(index).map_err(|_| {
            BenchError::InvalidInput("candidate profile block id overflowed".to_owned())
        })?;
        let exact_hash = block_id.checked_add(1).ok_or_else(|| {
            BenchError::InvalidInput("candidate profile exact hash overflowed".to_owned())
        })?;
        features.push(BlockFeatures {
            block: BlockId(block_id),
            role: BlockRole::Body,
            exact_hash: ExactHash(exact_hash),
            canonical_tokens: matching_tokens.clone(),
            matching_tokens,
            ngram_counts,
            ngram_size: SYNTHETIC_NGRAM_SIZE,
            page_position: None,
            numeric_mask_applied: false,
            has_normalization_issues: false,
        });
    }
    Ok(features)
}

fn profile_with_generator<G: CandidateGenerator>(
    features: &[BlockFeatures],
    top_k: &[usize],
    generator_kind: CandidateProfileGenerator,
    generator: G,
    index_build_latency_ns: u128,
    memory_before: ProcessMemory,
) -> Result<CandidateProfileRecord> {
    let mut recall_at_k = Vec::with_capacity(top_k.len());
    for &k in top_k {
        let mut recalled = 0_usize;
        for old in features {
            let candidates = generator
                .candidates(old, k)
                .map_err(|error| core_error("candidate profile recall query", error))?;
            if candidates
                .iter()
                .any(|candidate| candidate.block == old.block)
            {
                recalled += 1;
            }
        }
        recall_at_k.push(recalled as f64 / features.len() as f64);
    }

    let query_started = Instant::now();
    let mut candidate_counts = Vec::with_capacity(features.len());
    for old in features {
        let candidates = generator
            .candidates(old, usize::MAX)
            .map_err(|error| core_error("candidate profile full query", error))?;
        candidate_counts.push(candidates.len());
    }
    let query_latency_ns = query_started.elapsed().as_nanos();
    let memory_after = read_process_memory()?;

    Ok(CandidateProfileRecord {
        generator: generator_kind,
        blocks: features.len(),
        top_k: top_k.to_vec(),
        recall_at_k,
        candidate_count_p50: percentile(&candidate_counts, 0.50),
        candidate_count_p95: percentile(&candidate_counts, 0.95),
        candidate_count_max: candidate_counts.iter().copied().max().unwrap_or(0),
        index_build_latency_ns,
        query_latency_ns,
        memory_source: CandidateProfileMemorySource::LinuxProcStatus,
        rss_before_build_bytes: memory_before.rss_bytes,
        peak_rss_bytes: memory_after.peak_rss_bytes,
        peak_rss_growth_bytes: memory_after
            .peak_rss_bytes
            .saturating_sub(memory_before.rss_bytes),
    })
}

/// Nearest-rank percentile without interpolation.
fn percentile(values: &[usize], quantile: f64) -> usize {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    sorted[index]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessMemory {
    rss_bytes: u64,
    peak_rss_bytes: u64,
}

#[cfg(target_os = "linux")]
fn read_process_memory() -> Result<ProcessMemory> {
    let status = std::fs::read_to_string("/proc/self/status").map_err(|error| {
        BenchError::InvalidInput(format!(
            "cannot read Linux process memory from /proc/self/status: {error}"
        ))
    })?;
    Ok(ProcessMemory {
        rss_bytes: parse_proc_status_kib(&status, "VmRSS:")?,
        peak_rss_bytes: parse_proc_status_kib(&status, "VmHWM:")?,
    })
}

#[cfg(not(target_os = "linux"))]
fn read_process_memory() -> Result<ProcessMemory> {
    Err(BenchError::InvalidInput(
        "candidate profile memory measurement requires Linux /proc/self/status".to_owned(),
    ))
}

#[cfg(target_os = "linux")]
fn parse_proc_status_kib(status: &str, field: &str) -> Result<u64> {
    let line = status
        .lines()
        .find(|line| line.starts_with(field))
        .ok_or_else(|| {
            BenchError::InvalidInput(format!("Linux process memory status is missing {field}"))
        })?;
    let kib = line[field.len()..]
        .split_whitespace()
        .next()
        .ok_or_else(|| {
            BenchError::InvalidInput(format!(
                "Linux process memory status has no value for {field}"
            ))
        })?
        .parse::<u64>()
        .map_err(|error| {
            BenchError::InvalidInput(format!(
                "Linux process memory status has an invalid value for {field}: {error}"
            ))
        })?;
    kib.checked_mul(1_024).ok_or_else(|| {
        BenchError::InvalidInput(format!(
            "Linux process memory status overflowed for {field}"
        ))
    })
}

fn core_error(stage: &'static str, error: pdfdelta_core::Error) -> BenchError {
    BenchError::Core {
        stage,
        source: error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_profile_options() {
        assert!(
            profile_synthetic_candidate_generator(
                1,
                &[1],
                CandidateProfileGenerator::InvertedIndex
            )
            .is_err()
        );
        assert!(
            profile_synthetic_candidate_generator(
                MAX_SYNTHETIC_PROFILE_BLOCKS + 1,
                &[1],
                CandidateProfileGenerator::InvertedIndex
            )
            .is_err()
        );
        assert!(
            profile_synthetic_candidate_generator(2, &[], CandidateProfileGenerator::InvertedIndex)
                .is_err()
        );
        assert!(
            profile_synthetic_candidate_generator(
                2,
                &[0],
                CandidateProfileGenerator::InvertedIndex
            )
            .is_err()
        );
        assert!(
            profile_synthetic_candidate_generator(
                2,
                &[1, 1],
                CandidateProfileGenerator::InvertedIndex
            )
            .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn profiles_each_generator_with_complete_identity_recall() {
        for generator in [
            CandidateProfileGenerator::InvertedIndex,
            CandidateProfileGenerator::MinhashLsh,
            CandidateProfileGenerator::Exhaustive,
        ] {
            let record = profile_synthetic_candidate_generator(64, &[1, 5], generator)
                .expect("Linux process status and synthetic profile are valid");
            assert!(record.healthy());
            assert_eq!(record.blocks, 64);
            assert_eq!(record.top_k, [1, 5]);
            assert!(record.candidate_count_p50 > 0);
            assert!(record.candidate_count_p95 >= record.candidate_count_p50);
            assert!(record.candidate_count_max >= record.candidate_count_p95);
            assert!(record.rss_before_build_bytes > 0);
            assert!(record.peak_rss_bytes >= record.rss_before_build_bytes);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_linux_process_memory_in_bytes() {
        let status = "Name:\tpdfbench\nVmHWM:\t2048 kB\nVmRSS:\t1024 kB\n";
        assert_eq!(parse_proc_status_kib(status, "VmRSS:"), Ok(1_048_576));
        assert_eq!(parse_proc_status_kib(status, "VmHWM:"), Ok(2_097_152));
    }
}
