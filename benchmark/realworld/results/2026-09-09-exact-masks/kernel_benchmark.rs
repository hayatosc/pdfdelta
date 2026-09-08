#![allow(dead_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidConfiguration(String),
    Unresolved(String),
    LimitExceeded {
        resource: &'static str,
        limit: usize,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

static CURRENT_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record_alloc(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        CURRENT_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
        if !new_pointer.is_null() {
            if new_size >= layout.size() {
                record_alloc(new_size - layout.size());
            } else {
                CURRENT_BYTES.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        new_pointer
    }
}

fn record_alloc(bytes: usize) {
    let current = CURRENT_BYTES.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let mut peak = PEAK_BYTES.load(Ordering::Relaxed);
    while current > peak {
        match PEAK_BYTES.compare_exchange_weak(peak, current, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => break,
            Err(observed) => peak = observed,
        }
    }
}

fn reset_peak() -> usize {
    let current = CURRENT_BYTES.load(Ordering::Relaxed);
    PEAK_BYTES.store(current, Ordering::Relaxed);
    current
}

#[path = "../../../../crates/pdfdelta-core/src/diff/assessment/claims.rs"]
mod claims;

#[derive(Clone, Copy, Debug)]
enum Mode {
    Adaptive,
    Band,
    Dense,
    DenseUncapped,
}

impl Mode {
    const ALL: [Self; 4] = [Self::Adaptive, Self::Band, Self::Dense, Self::DenseUncapped];

    const fn name(self) -> &'static str {
        match self {
            Self::Adaptive => "adaptive",
            Self::Band => "forced-band",
            Self::Dense => "forced-dense",
            Self::DenseUncapped => "forced-dense-uncapped",
        }
    }

    const fn memory_limit_bytes(self) -> usize {
        match self {
            Self::DenseUncapped => 512 * 1024 * 1024,
            _ => 64 * 1024 * 1024,
        }
    }
}

struct Case {
    name: &'static str,
    old: Vec<u8>,
    new: Vec<u8>,
    old_source: Vec<bool>,
    new_source: Vec<bool>,
    old_residual: Vec<bool>,
    new_residual: Vec<bool>,
}

fn small_case(length: usize) -> Case {
    let old = (0..length)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let mut new = old.clone();
    new[length / 2] = 255;
    Case {
        name: match length {
            500 => "small-edit-500",
            2_000 => "small-edit-2000",
            4_000 => "small-edit-4000",
            _ => "small-edit",
        },
        old_source: vec![true; length],
        new_source: vec![true; length],
        old_residual: (0..length).map(|index| index % 3 == 0).collect(),
        new_residual: (0..length).map(|index| index % 3 == 1).collect(),
        old,
        new,
    }
}

fn dsa_case() -> Case {
    let old = include_bytes!("../structure-claim-probe/inputs/dsa-old-introduction.txt")
        .strip_suffix(b"\n")
        .unwrap()
        .to_vec();
    let new = include_bytes!("../structure-claim-probe/inputs/dsa-new-introduction.txt")
        .strip_suffix(b"\n")
        .unwrap()
        .to_vec();
    let mut new_source = vec![false; new.len()];
    new_source[1611..1718].fill(true);
    let mut source_work = usize::MAX;
    let source_claims = claims::literal_claims(
        &old,
        &new,
        &vec![false; old.len()],
        &new_source,
        &vec![false; old.len()],
        &vec![false; new.len()],
        &mut source_work,
    )
    .expect("the DSA source query should not fail")
    .expect("the DSA source query should fit its budget");
    let mut new_residual = vec![false; new.len()];
    for index in 1611..1718 {
        new_residual[index] = !source_claims.mandatory.new[index];
    }
    let mut residual_work = usize::MAX;
    let residual_claims = claims::literal_claims(
        &old,
        &new,
        &vec![false; old.len()],
        &new_source,
        &vec![false; old.len()],
        &new_residual,
        &mut residual_work,
    )
    .expect("the DSA residual query should not fail")
    .expect("the DSA residual query should fit its budget");
    assert_eq!(
        residual_claims.residual,
        claims::CountBounds {
            lower: 29,
            upper: 33,
        }
    );
    Case {
        name: "actual-dsa-introduction",
        old_source: vec![false; old.len()],
        new_source,
        old_residual: vec![false; old.len()],
        new_residual,
        old,
        new,
    }
}

fn run(case: &Case, mode: Mode, work: &mut usize) -> Result<Option<claims::LiteralClaims>> {
    match mode {
        Mode::Adaptive => claims::literal_claims(
            &case.old,
            &case.new,
            &case.old_source,
            &case.new_source,
            &case.old_residual,
            &case.new_residual,
            work,
        ),
        Mode::Band => claims::literal_claims_for_benchmark(
            &case.old,
            &case.new,
            &case.old_source,
            &case.new_source,
            &case.old_residual,
            &case.new_residual,
            work,
            true,
        ),
        Mode::Dense => claims::literal_claims_for_benchmark(
            &case.old,
            &case.new,
            &case.old_source,
            &case.new_source,
            &case.old_residual,
            &case.new_residual,
            work,
            false,
        ),
        Mode::DenseUncapped => claims::literal_claims_for_benchmark_with_limit(
            &case.old,
            &case.new,
            &case.old_source,
            &case.new_source,
            &case.old_residual,
            &case.new_residual,
            work,
            false,
            512 * 1024 * 1024,
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Digest {
    status: &'static str,
    source_lower: usize,
    source_upper: usize,
    residual_lower: usize,
    residual_upper: usize,
    lcs_length: usize,
    old_mandatory_count: usize,
    new_mandatory_count: usize,
    old_mandatory_mask: Vec<bool>,
    new_mandatory_mask: Vec<bool>,
}

fn digest(result: &Result<Option<claims::LiteralClaims>>) -> Digest {
    match result {
        Ok(Some(claims)) => Digest {
            status: "complete",
            source_lower: claims.source.lower,
            source_upper: claims.source.upper,
            residual_lower: claims.residual.lower,
            residual_upper: claims.residual.upper,
            lcs_length: claims.mandatory.lcs_length,
            old_mandatory_count: claims
                .mandatory
                .old
                .iter()
                .filter(|changed| **changed)
                .count(),
            new_mandatory_count: claims
                .mandatory
                .new
                .iter()
                .filter(|changed| **changed)
                .count(),
            old_mandatory_mask: claims.mandatory.old.clone(),
            new_mandatory_mask: claims.mandatory.new.clone(),
        },
        Ok(None) => empty_digest("budget-exhausted"),
        Err(Error::LimitExceeded { .. }) => empty_digest("limit-exceeded"),
        Err(_) => empty_digest("error"),
    }
}

fn empty_digest(status: &'static str) -> Digest {
    Digest {
        status,
        source_lower: 0,
        source_upper: 0,
        residual_lower: 0,
        residual_upper: 0,
        lcs_length: 0,
        old_mandatory_count: 0,
        new_mandatory_count: 0,
        old_mandatory_mask: Vec::new(),
        new_mandatory_mask: Vec::new(),
    }
}

struct Measurement {
    median_ns: u128,
    allocated_proof_bytes: usize,
    work_charged: usize,
    digest: Digest,
}

fn measure(case: &Case, mode: Mode) -> Measurement {
    for _ in 0..2 {
        let mut work = usize::MAX;
        let _ = run(case, mode, &mut work);
    }
    let mut times = Vec::with_capacity(5);
    let mut allocated_proof_bytes = 0;
    let mut work_charged = 0;
    let mut measured_digest = None;
    for _ in 0..5 {
        let initial_work = usize::MAX;
        let mut work = initial_work;
        let baseline_bytes = reset_peak();
        let start = Instant::now();
        let result = run(case, mode, &mut work);
        times.push(start.elapsed().as_nanos());
        allocated_proof_bytes = allocated_proof_bytes.max(
            PEAK_BYTES
                .load(Ordering::Relaxed)
                .saturating_sub(baseline_bytes),
        );
        work_charged = work_charged.max(initial_work - work);
        let current_digest = digest(&result);
        if let Some(previous) = &measured_digest {
            assert_eq!(
                previous, &current_digest,
                "mode result changed between runs"
            );
        } else {
            measured_digest = Some(current_digest);
        }
        drop(result);
    }
    times.sort_unstable();
    Measurement {
        median_ns: times[2],
        allocated_proof_bytes,
        work_charged,
        digest: measured_digest.unwrap(),
    }
}

fn print_digest(digest: &Digest) -> String {
    format!(
        "{{\"status\":\"{}\",\"source\":[{},{}],\"residual\":[{},{}],\"lcs\":{},\"old_mandatory_count\":{},\"new_mandatory_count\":{},\"old_mandatory_mask_hex\":\"{}\",\"new_mandatory_mask_hex\":\"{}\"}}",
        digest.status,
        digest.source_lower,
        digest.source_upper,
        digest.residual_lower,
        digest.residual_upper,
        digest.lcs_length,
        digest.old_mandatory_count,
        digest.new_mandatory_count,
        mask_hex(&digest.old_mandatory_mask),
        mask_hex(&digest.new_mandatory_mask),
    )
}

fn mask_hex(mask: &[bool]) -> String {
    let mut bytes = vec![0u8; mask.len().div_ceil(8)];
    for (index, changed) in mask.iter().copied().enumerate() {
        if changed {
            bytes[index / 8] |= 1 << (index % 8);
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn main() {
    let cases = [
        small_case(500),
        small_case(2_000),
        small_case(4_000),
        dsa_case(),
    ];
    for case in &cases {
        let mut measurements = Vec::new();
        for mode in Mode::ALL {
            let measurement = measure(case, mode);
            println!(
                "RESULT {{\"case\":\"{}\",\"old_len\":{},\"new_len\":{},\"mode\":\"{}\",\"memory_limit_bytes\":{},\"warm_runs\":5,\"median_ns\":{},\"allocated_proof_bytes\":{},\"work_charged\":{},\"digest\":{}}}",
                case.name,
                case.old.len(),
                case.new.len(),
                mode.name(),
                mode.memory_limit_bytes(),
                measurement.median_ns,
                measurement.allocated_proof_bytes,
                measurement.work_charged,
                print_digest(&measurement.digest),
            );
            measurements.push((mode, measurement));
        }
        let adaptive = &measurements[0].1;
        let band = &measurements[1].1;
        let dense = &measurements[2].1;
        let dense_uncapped = &measurements[3].1;
        println!(
            "PARITY {{\"case\":\"{}\",\"adaptive_band\":{},\"adaptive_dense\":{},\"adaptive_dense_uncapped\":{},\"band_dense\":{},\"band_dense_uncapped\":{},\"dense_dense_uncapped\":{}}}",
            case.name,
            adaptive.digest == band.digest,
            adaptive.digest == dense.digest,
            adaptive.digest == dense_uncapped.digest,
            band.digest == dense.digest,
            band.digest == dense_uncapped.digest,
            dense.digest == dense_uncapped.digest,
        );
    }
}
