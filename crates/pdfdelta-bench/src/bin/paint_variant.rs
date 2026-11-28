//! Measure layout-tolerant non-text paint variants on the H1 pair set.
//!
//! This is benchmark evidence, not a shipped comparison rule. It extracts the
//! same paint inventory the flow engine uses and reports whether two
//! translation policies would certify paint equality:
//!
//! * `baseline`: page-relative absolute identity (the M4b rule);
//! * `group-shift`: per page, all paint shapes may move by one common vertical
//!   displacement while horizontal placement and shape stay identical;
//! * `item-shift`: each paint item may move vertically independently, keeping
//!   shape, size, and horizontal placement.
//!
//! The variants never read or change the shipped flow report.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use clap::Parser;
use pdfdelta_core::{
    model::{NonTextPaint, NonTextPaintKind, PageId, Rect},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
struct Args {
    /// H1 pair manifest.
    #[arg(long)]
    pairs: PathBuf,
    /// M4 corpus summary used for the before/after exit baseline.
    #[arg(long)]
    baseline: PathBuf,
    /// Capture directory containing the input PDFs.
    #[arg(long, default_value = "benchmark/realworld/cache")]
    cache: PathBuf,
    /// Output summary path.
    #[arg(long)]
    output: PathBuf,
    /// Restrict to one or more pair ids.
    #[arg(long)]
    pair: Vec<String>,
}

#[derive(Clone, Debug)]
struct PaintItem {
    page: PageId,
    kind: NonTextPaintKind,
    identity: Option<String>,
    shape: Option<String>,
    bounds: Option<Rect>,
}

impl From<&NonTextPaint> for PaintItem {
    fn from(paint: &NonTextPaint) -> Self {
        Self {
            page: paint.page,
            kind: paint.kind,
            identity: paint.identity.clone(),
            shape: paint.shape.clone(),
            bounds: paint.bounds,
        }
    }
}

struct Extraction {
    paint: Vec<PaintItem>,
    wall_s: f64,
    issues: usize,
}

fn extract(path: &Path, limits: ExtractionLimits) -> Result<Extraction, String> {
    let started = Instant::now();
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() > ParseLimits::default().max_input_bytes {
        return Err(format!("{} exceeds the input byte limit", path.display()));
    }
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    let outcome = source
        .extract_outcome(
            Arc::from(bytes.into_boxed_slice()),
            ParseLimits::default(),
            limits,
        )
        .map_err(|error| format!("cannot extract {}: {error}", path.display()))?;
    let paint = outcome
        .document()
        .non_text_paint_bounds()
        .ok_or_else(|| format!("{} has no paint census", path.display()))?
        .iter()
        .map(PaintItem::from)
        .collect();
    Ok(Extraction {
        paint,
        wall_s: started.elapsed().as_secs_f64(),
        issues: outcome.issues().len(),
    })
}

fn bits(value: f64) -> u64 {
    value.to_bits()
}

fn kind_name(kind: NonTextPaintKind) -> &'static str {
    match kind {
        NonTextPaintKind::Path => "path",
        NonTextPaintKind::Image => "image",
        NonTextPaintKind::InlineImage => "inline_image",
        NonTextPaintKind::Shading => "shading",
        NonTextPaintKind::Opaque => "opaque",
    }
}

fn kind_counts(paint: &[PaintItem]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for item in paint {
        *counts.entry(kind_name(item.kind)).or_default() += 1;
    }
    counts
}

/// Exact page-relative identity computed by the extraction layer.
fn baseline_key(item: &PaintItem) -> Option<String> {
    item.identity.clone()
}

/// One common vertical displacement per page. The vertical reference is the
/// minimum painted y on that page, so a whole page's paint may shift together.
fn group_shift_keys(paint: &[PaintItem]) -> Vec<String> {
    let mut anchors = BTreeMap::<PageId, f64>::new();
    for item in paint {
        let Some(bounds) = item.bounds else {
            continue;
        };
        if item.shape.is_none() {
            continue;
        }
        anchors
            .entry(item.page)
            .and_modify(|value| *value = value.min(bounds.min.y))
            .or_insert(bounds.min.y);
    }
    let mut keys = Vec::with_capacity(paint.len());
    for item in paint {
        let Some(bounds) = item.bounds else {
            continue;
        };
        let Some(anchor) = anchors.get(&item.page).copied() else {
            continue;
        };
        let Some(shape) = item.shape.as_deref() else {
            continue;
        };
        keys.push(format!(
            "{:?}|{}|{}|{}|{}|{}|{}",
            item.kind,
            shape,
            bits(bounds.min.x),
            bits(bounds.max.x - bounds.min.x),
            bits(bounds.max.y - bounds.min.y),
            bits(bounds.min.y - anchor),
            bits(bounds.max.y - anchor),
        ));
    }
    keys.sort();
    keys
}

/// Each item may shift vertically independently; horizontal placement and
/// shape stay identical.
fn item_shift_keys(paint: &[PaintItem]) -> Vec<String> {
    let mut keys = Vec::with_capacity(paint.len());
    for item in paint {
        let Some(bounds) = item.bounds else {
            continue;
        };
        let Some(shape) = item.shape.as_deref() else {
            continue;
        };
        keys.push(format!(
            "{:?}|{}|{}|{}|{}",
            item.kind,
            shape,
            bits(bounds.min.x),
            bits(bounds.max.x - bounds.min.x),
            bits(bounds.max.y - bounds.min.y),
        ));
    }
    keys.sort();
    keys
}

fn shape_keys(paint: &[PaintItem]) -> Vec<String> {
    let mut keys = paint
        .iter()
        .filter_map(|item| Some(format!("{:?}|{}", item.kind, item.shape.as_deref()?)))
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

fn multiset_equal(left: &[String], right: &[String]) -> bool {
    left == right
}

fn unmatched_count(left: &[String], right: &[String]) -> usize {
    let mut counts = BTreeMap::<String, i64>::new();
    for key in left {
        *counts.entry(key.clone()).or_default() += 1;
    }
    for key in right {
        *counts.entry(key.clone()).or_default() -= 1;
    }
    counts
        .values()
        .map(|count| count.unsigned_abs() as usize)
        .sum()
}

fn unmatched_examples(left: &[String], right: &[String], limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, i64>::new();
    for key in left {
        *counts.entry(key.clone()).or_default() += 1;
    }
    for key in right {
        *counts.entry(key.clone()).or_default() -= 1;
    }
    counts
        .into_iter()
        .filter(|(_, count)| *count != 0)
        .map(|(key, count)| format!("{count:+}: {key}"))
        .take(limit)
        .collect()
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let pairs: Value = serde_json::from_slice(
        &fs::read(&args.pairs).map_err(|error| format!("cannot read pairs: {error}"))?,
    )
    .map_err(|error| format!("cannot parse pairs: {error}"))?;
    let baseline: Value = serde_json::from_slice(
        &fs::read(&args.baseline).map_err(|error| format!("cannot read baseline: {error}"))?,
    )
    .map_err(|error| format!("cannot parse baseline: {error}"))?;
    let baseline_rows = baseline["pairs"]
        .as_array()
        .ok_or_else(|| "baseline summary has no pairs".to_owned())?;
    let expected = baseline_rows
        .iter()
        .filter_map(|row| Some((row["pair"].as_str()?.to_owned(), row["exit"].as_i64()?)))
        .collect::<BTreeMap<_, _>>();
    let pairs = pairs
        .as_array()
        .ok_or_else(|| "pair manifest is not an array".to_owned())?;
    let selected = if args.pair.is_empty() {
        pairs.clone()
    } else {
        pairs
            .iter()
            .filter(|pair| {
                pair["pair"]
                    .as_str()
                    .is_some_and(|id| args.pair.iter().any(|selected| selected == id))
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    let started = Instant::now();
    let mut rows = Vec::new();
    let mut baseline_equal = 0usize;
    let mut group_equal = 0usize;
    let mut item_equal = 0usize;
    let mut exits = BTreeMap::<String, usize>::new();
    let mut group_exits = BTreeMap::<String, usize>::new();
    let mut item_exits = BTreeMap::<String, usize>::new();
    for (index, pair) in selected.iter().enumerate() {
        let id = pair["pair"]
            .as_str()
            .ok_or_else(|| "pair has no id".to_owned())?;
        let set = pair["set"].as_str().unwrap_or("?");
        let scale = if set == "M" {
            pair["limit_scale"].as_f64().unwrap_or(1.0)
        } else {
            1.0
        };
        let limits = ExtractionLimits::default()
            .scaled(scale)
            .map_err(|error| format!("invalid scale for {id}: {error}"))?;
        let old_path = Path::new(
            pair["old"]
                .as_str()
                .ok_or_else(|| format!("{id} has no old path"))?,
        );
        let new_path = Path::new(
            pair["new"]
                .as_str()
                .ok_or_else(|| format!("{id} has no new path"))?,
        );
        let old = extract(old_path, limits)?;
        let new = extract(new_path, limits)?;
        let baseline_keys = |paint: &[PaintItem]| {
            let mut keys = paint.iter().filter_map(baseline_key).collect::<Vec<_>>();
            keys.sort();
            keys
        };
        let old_baseline = baseline_keys(&old.paint);
        let new_baseline = baseline_keys(&new.paint);
        let old_group = group_shift_keys(&old.paint);
        let new_group = group_shift_keys(&new.paint);
        let old_item = item_shift_keys(&old.paint);
        let new_item = item_shift_keys(&new.paint);
        let old_shape = shape_keys(&old.paint);
        let new_shape = shape_keys(&new.paint);
        let old_unknown = old
            .paint
            .iter()
            .filter(|item| item.shape.is_none() || item.bounds.is_none())
            .count();
        let new_unknown = new
            .paint
            .iter()
            .filter(|item| item.shape.is_none() || item.bounds.is_none())
            .count();
        let old_unknown_shape = old.paint.iter().filter(|item| item.shape.is_none()).count();
        let new_unknown_shape = new.paint.iter().filter(|item| item.shape.is_none()).count();
        let old_unknown_bounds = old
            .paint
            .iter()
            .filter(|item| item.bounds.is_none())
            .count();
        let new_unknown_bounds = new
            .paint
            .iter()
            .filter(|item| item.bounds.is_none())
            .count();
        let is_baseline_equal =
            old_unknown == 0 && new_unknown == 0 && multiset_equal(&old_baseline, &new_baseline);
        let is_group_equal =
            old_unknown == 0 && new_unknown == 0 && multiset_equal(&old_group, &new_group);
        let is_item_equal =
            old_unknown == 0 && new_unknown == 0 && multiset_equal(&old_item, &new_item);
        let shape_unmatched = unmatched_count(&old_shape, &new_shape);
        let group_unmatched = unmatched_count(&old_group, &new_group);
        let item_unmatched = unmatched_count(&old_item, &new_item);
        let group_examples = unmatched_examples(&old_group, &new_group, 4);
        if is_baseline_equal {
            baseline_equal += 1;
        }
        if is_group_equal {
            group_equal += 1;
        }
        if is_item_equal {
            item_equal += 1;
        }
        let m4_exit = expected.get(id).copied().unwrap_or(-1);
        let baseline_exit = if is_baseline_equal { m4_exit } else { 3 };
        let group_exit = if is_group_equal { m4_exit } else { 3 };
        let item_exit = if is_item_equal { m4_exit } else { 3 };
        *exits.entry(baseline_exit.to_string()).or_default() += 1;
        *group_exits.entry(group_exit.to_string()).or_default() += 1;
        *item_exits.entry(item_exit.to_string()).or_default() += 1;
        let pages = old
            .paint
            .iter()
            .map(|item| item.page)
            .chain(new.paint.iter().map(|item| item.page))
            .collect::<BTreeSet<_>>()
            .len();
        rows.push(json!({
            "pair": id,
            "set": set,
            "limit_scale": scale,
            "baseline_equal": is_baseline_equal,
            "group_shift_equal": is_group_equal,
            "item_shift_equal": is_item_equal,
            "shape_unmatched": shape_unmatched,
            "group_unmatched": group_unmatched,
            "item_unmatched": item_unmatched,
            "group_unmatched_examples": group_examples,
            "old_unknown_evidence": old_unknown,
            "new_unknown_evidence": new_unknown,
            "old_unknown_shape": old_unknown_shape,
            "new_unknown_shape": new_unknown_shape,
            "old_unknown_bounds": old_unknown_bounds,
            "new_unknown_bounds": new_unknown_bounds,
            "old_kinds": kind_counts(&old.paint),
            "new_kinds": kind_counts(&new.paint),
            "baseline_exit": baseline_exit,
            "group_shift_exit": group_exit,
            "item_shift_exit": item_exit,
            "m4_exit": m4_exit,
            "old_paint_items": old.paint.len(),
            "new_paint_items": new.paint.len(),
            "old_extraction_issues": old.issues,
            "new_extraction_issues": new.issues,
            "old_wall_s": old.wall_s,
            "new_wall_s": new.wall_s,
            "pages_with_paint": pages,
        }));
        println!(
            "{}/64 {set}/{id}: baseline {is_baseline_equal} group {is_group_equal} item {is_item_equal} \
             m4 exit {m4_exit} -> group exit {group_exit}",
            index + 1
        );
    }
    let payload = json!({
        "schema": "pdfdelta-m4b-paint-variant/1",
        "pairs": rows,
        "wall_s": started.elapsed().as_secs_f64(),
        "baseline_equal": baseline_equal,
        "group_shift_equal": group_equal,
        "item_shift_equal": item_equal,
        "baseline_exits": exits,
        "group_shift_exits": group_exits,
        "item_shift_exits": item_exits,
    });
    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("cannot create output: {error}"))?;
    }
    fs::write(
        &args.output,
        serde_json::to_vec_pretty(&payload)
            .map_err(|error| format!("cannot encode summary: {error}"))?,
    )
    .map_err(|error| format!("cannot write summary: {error}"))?;
    println!(
        "baseline {baseline_equal}/64, group-shift {group_equal}/64, item-shift {item_equal}/64; \
         wrote {}",
        args.output.display()
    );
    Ok(())
}
