//! Compare the cost of existing claims and optional joint alternatives on the
//! same supplied strings. No PDF extraction or correspondence is measured.

use std::{fs::File, hint::black_box, io::Read, time::Instant};

use pdfdelta_core::{
    diff::{
        LocalTextEnvelopeLimits, LocalTextEnvelopeResult, LocalTextSide, local_text_claims,
        local_text_envelope,
    },
    normalize::ComparableToken,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Input {
    pairs: Vec<Pair>,
}
#[derive(Deserialize)]
struct Pair {
    old: String,
    new: String,
}

struct Side {
    tokens: Vec<ComparableToken>,
    source: Vec<bool>,
    optional: Vec<bool>,
}
impl Side {
    fn new(text: &str) -> Self {
        let tokens: Vec<_> = text.chars().map(ComparableToken::Scalar).collect();
        Self {
            source: vec![true; tokens.len()],
            optional: vec![false; tokens.len()],
            tokens,
        }
    }
    fn input(&self) -> LocalTextSide<'_> {
        LocalTextSide {
            tokens: &self.tokens,
            source: &self.source,
            optional: &self.optional,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 4 || !matches!(args[1].as_str(), "claims" | "envelope") {
        return Err("usage: local_text_envelope_cost claims|envelope INPUT ITERATIONS".into());
    }
    let iterations: usize = args[3].parse()?;
    if !(1..=10000).contains(&iterations) {
        return Err("iteration cap is 10000".into());
    }
    let mut bytes = Vec::new();
    File::open(&args[2])?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1_048_576 {
        return Err("input byte cap".into());
    }
    let input: Input = serde_json::from_slice(&bytes)?;
    if input.pairs.len() > 1024
        || input
            .pairs
            .iter()
            .any(|p| p.old.len() + p.new.len() > 32768)
    {
        return Err("pair/token cap".into());
    }
    let pairs: Vec<_> = input
        .pairs
        .iter()
        .map(|p| (Side::new(&p.old), Side::new(&p.new)))
        .collect();
    let started = Instant::now();
    let mut work = 0usize;
    let mut checksum = 0usize;
    for _ in 0..iterations {
        for (old, new) in &pairs {
            let mut remaining = 1_000_000;
            if args[1] == "claims" {
                let claim = local_text_claims(old.input(), new.input(), &mut remaining)?
                    .ok_or("claim unfinished")?;
                checksum += black_box(claim).changed_source_lower;
            } else {
                let result = local_text_envelope(
                    old.input(),
                    new.input(),
                    &mut remaining,
                    LocalTextEnvelopeLimits::default(),
                )?;
                let LocalTextEnvelopeResult::Complete { envelope } = result else {
                    return Err("envelope unfinished".into());
                };
                checksum += black_box(envelope).claims.changed_source_lower;
            }
            work += 1_000_000 - remaining;
        }
    }
    let elapsed_ns = started.elapsed().as_nanos();
    let process_peak_rss_kib =
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("VmHWM:"))
                    .and_then(|line| line.split_whitespace().next())
                    .and_then(|n| n.parse::<usize>().ok())
            });
    println!(
        "{}",
        serde_json::json!({"mode":args[1],"iterations":iterations,"pairs":pairs.len(),
            "elapsed_ns":elapsed_ns,"work_per_iteration":work/iterations,"checksum":checksum,
            "process_peak_rss_kib":process_peak_rss_kib})
    );
    Ok(())
}
