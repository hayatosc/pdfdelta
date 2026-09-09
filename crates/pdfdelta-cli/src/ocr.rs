//! Local recognition retains model identity, pixel coordinates, and unknown
//! confidence. It proposes text views; it never rewrites native glyph evidence.

use std::{
    io::{BufRead, Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, EvidenceFailure, EvidenceIssue,
        EvidenceStore, RecognizedWord, SourceRef, StructuredEvidence, StructuredValue,
    },
    model::{DecodedText, GlyphCropStatus, GlyphPathClipStatus, PageId, TextRenderMode},
};
use rten_imageproc::BoundingRect;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_PIXELS: usize = 8_000_000;
const MAX_HEADER: usize = 256 * 1024;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_MODEL_BYTES: usize = 256 * 1024 * 1024;
const MAX_WORDS: usize = 4096;
const MAX_NATIVE_RECTS: usize = 4096;

pub struct Models {
    pub detection: PathBuf,
    pub recognition: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    width: u32,
    height: u32,
    native_rects: Vec<[u32; 4]>,
}

#[derive(Serialize, Deserialize)]
struct Line {
    text: String,
    bounds: [u32; 4],
    words: Vec<RecognizedWord>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum Response {
    Recognized {
        detection_hash: String,
        recognition_hash: String,
        lines: Vec<Line>,
        skipped_native_words: usize,
    },
    Failed {
        kind: EvidenceFailure,
        reason: String,
    },
}

pub fn collect(store: &mut EvidenceStore, models: &Models) {
    let started = Instant::now();
    for page in &store.pages {
        if !store.rendered.iter().any(|region| region.page == page.page) {
            store.issues.push(issue(
                page.page,
                None,
                EvidenceFailure::Unresolved,
                "OCR was not examined because no page raster is available".into(),
            ));
        }
    }
    for region in &store.rendered {
        let result = (|| {
            let remaining = Duration::from_secs(60)
                .checked_sub(started.elapsed())
                .ok_or_else(|| {
                    (
                        EvidenceFailure::ResourceLimit,
                        "document recognition deadline exceeded".into(),
                    )
                })?;
            let mut native_rects = Vec::new();
            let [top_left, top_right, _, bottom_left] = region.polygon.as_slice() else {
                return Err((
                    EvidenceFailure::Unresolved,
                    "OCR requires a rectangular page raster".into(),
                ));
            };
            region
                .pixel_bounds_in_page([0, 0, region.raster.width, region.raster.height])
                .map_err(|error| (EvidenceFailure::Unresolved, error.to_string()))?;
            // Glyph boxes do not prove visibility through later paint. A glyph
            // emitted after the last recorded paint can still exclude OCR.
            let paint_boundary = store.native.last_non_text_paint().get(&region.page);
            for glyph in store.native.items().iter().filter(|glyph| {
                glyph.page == region.page
                    && paint_boundary.is_none_or(|order| glyph.render_order >= *order)
            }) {
                if !matches!(&glyph.text, DecodedText::Mapped(text) if !text.is_empty())
                    || glyph.crop_status != GlyphCropStatus::Inside
                    || !matches!(
                        glyph.path_clip_status,
                        GlyphPathClipStatus::Inside | GlyphPathClipStatus::Unclipped
                    )
                    || matches!(
                        glyph.render_mode,
                        TextRenderMode::Invisible | TextRenderMode::Clip
                    )
                {
                    continue;
                }
                if native_rects.len() == MAX_NATIVE_RECTS {
                    return Err((
                        EvidenceFailure::ResourceLimit,
                        "native OCR selection rectangles exceed the budget".into(),
                    ));
                }
                let x = |x: f64| {
                    ((x - top_left.x) / (top_right.x - top_left.x) * f64::from(region.raster.width))
                        .clamp(0.0, f64::from(region.raster.width))
                };
                let y = |y: f64| {
                    ((top_left.y - y) / (top_left.y - bottom_left.y)
                        * f64::from(region.raster.height))
                    .clamp(0.0, f64::from(region.raster.height))
                };
                let rect = [
                    x(glyph.bbox.min.x).floor() as u32,
                    y(glyph.bbox.max.y).floor() as u32,
                    x(glyph.bbox.max.x).ceil() as u32,
                    y(glyph.bbox.min.y).ceil() as u32,
                ];
                if rect[0] < rect[2] && rect[1] < rect[3] {
                    native_rects.push(rect);
                }
            }
            let request = Request {
                width: region.raster.width,
                height: region.raster.height,
                native_rects,
            };
            let mut input = serde_json::to_vec(&request)
                .map_err(|error| (EvidenceFailure::BackendFailure, error.to_string()))?;
            if input.len() + 1 > MAX_HEADER {
                return Err((
                    EvidenceFailure::ResourceLimit,
                    "OCR request header exceeds the budget".into(),
                ));
            }
            input.push(b'\n');
            input.extend_from_slice(&region.raster.rgb);
            let mut command = Command::new(
                std::env::current_exe()
                    .map_err(|error| (EvidenceFailure::BackendFailure, error.to_string()))?,
            );
            command
                .arg("recognize-region")
                .arg("--")
                .arg(&models.detection)
                .arg(&models.recognition)
                .env("RTEN_NUM_THREADS", "1")
                .env("RAYON_NUM_THREADS", "1");
            let output = crate::render::run_bounded(
                &mut command,
                &input,
                MAX_OUTPUT,
                remaining.min(Duration::from_secs(15)),
            )?;
            serde_json::from_slice::<Response>(&output).map_err(|error| {
                (
                    EvidenceFailure::BackendFailure,
                    format!("invalid OCR response: {error}"),
                )
            })
        })();
        let (detection_hash, recognition_hash, lines, skipped) = match result {
            Ok(Response::Recognized {
                detection_hash,
                recognition_hash,
                lines,
                skipped_native_words,
            }) => (
                detection_hash,
                recognition_hash,
                lines,
                skipped_native_words,
            ),
            Ok(Response::Failed { kind, reason }) | Err((kind, reason)) => {
                store
                    .issues
                    .push(issue(region.page, Some(region.id), kind, reason));
                continue;
            }
        };
        let identity = BackendIdentity {
            kind: BackendKind::Ocr,
            name: "ocrs".into(),
            version: "0.13.0".into(),
            profile: "rten-0.26-latin-greedy-native-covered-words-v2".into(),
            model: Some(format!(
                "detection-sha256:{detection_hash};recognition-sha256:{recognition_hash}"
            )),
        };
        let backend = store
            .backends
            .iter()
            .position(|item| item == &identity)
            .unwrap_or_else(|| {
                let index = store.backends.len();
                store.backends.push(identity);
                index
            });
        let mut sources = Vec::new();
        for line in lines {
            let bounds = match region.pixel_bounds_in_page(line.bounds) {
                Ok(bounds) => bounds,
                Err(error) => {
                    store.issues.push(issue(
                        region.page,
                        Some(region.id),
                        EvidenceFailure::BackendFailure,
                        error.to_string(),
                    ));
                    continue;
                }
            };
            let id = store.structured.len() as u64;
            store.structured.push(StructuredEvidence {
                id,
                page: Some(region.page),
                bounds: Some(bounds),
                object: None,
                backend,
                value: StructuredValue::RecognizedText {
                    text: line.text,
                    region: region.id,
                    pixel_bounds: line.bounds,
                    words: line.words,
                },
            });
            sources.push(SourceRef::Structured { element: id });
        }
        store.inventories.push(ChannelInventory {
            page: Some(region.page),
            channel: Channel::Text,
            backend,
            sources,
            complete: false,
        });
        store.issues.push(issue(region.page, Some(region.id), EvidenceFailure::Unresolved,
            format!("OCR readings are inferred; Latin-model detection does not prove complete text coverage ({skipped} native-covered word regions skipped)")));
    }
}

fn issue(
    page: PageId,
    region: Option<u64>,
    kind: EvidenceFailure,
    reason: String,
) -> EvidenceIssue {
    EvidenceIssue {
        page: Some(page),
        channel: Channel::Text,
        sources: region
            .map(|region| SourceRef::Rendered { region })
            .into_iter()
            .collect(),
        kind,
        reason,
    }
}

struct Failure {
    kind: EvidenceFailure,
    reason: String,
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            kind: EvidenceFailure::BackendFailure,
            reason,
        }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

fn limit(reason: &str) -> Failure {
    Failure {
        kind: EvidenceFailure::ResourceLimit,
        reason: reason.into(),
    }
}

pub fn worker(models: &Models) -> Result<(), u8> {
    crate::render::restrict_process(15)?;
    let response = match recognize(models) {
        Ok(response) => response,
        Err(Failure { kind, reason }) => Response::Failed { kind, reason },
    };
    let bytes = serde_json::to_vec(&response).map_err(|_| 2)?;
    if bytes.len() > MAX_OUTPUT {
        return Err(3);
    }
    std::io::stdout().write_all(&bytes).map_err(|_| 2)
}

fn recognize(models: &Models) -> Result<Response, Failure> {
    let mut input = std::io::BufReader::new(std::io::stdin().lock());
    let mut header = Vec::new();
    input
        .by_ref()
        .take(MAX_HEADER as u64 + 1)
        .read_until(b'\n', &mut header)
        .map_err(|error| error.to_string())?;
    if header.len() > MAX_HEADER {
        return Err(limit("OCR request header exceeds the byte limit"));
    }
    if header.last() != Some(&b'\n') {
        return Err("invalid OCR request header".into());
    }
    let request: Request = serde_json::from_slice(&header).map_err(|error| error.to_string())?;
    let pixels = (request.width as usize)
        .checked_mul(request.height as usize)
        .ok_or_else(|| limit("OCR raster overflow"))?;
    if pixels > MAX_PIXELS || request.native_rects.len() > MAX_NATIVE_RECTS {
        return Err(limit("OCR input exceeds limits"));
    }
    if pixels == 0
        || request.native_rects.iter().any(|rect| {
            rect[0] >= rect[2]
                || rect[1] >= rect[3]
                || rect[2] > request.width
                || rect[3] > request.height
        })
    {
        return Err("invalid OCR grid or native rectangle".into());
    }
    let mut rgb = Vec::new();
    input
        .take((pixels * 3 + 1) as u64)
        .read_to_end(&mut rgb)
        .map_err(|error| error.to_string())?;
    if rgb.len() != pixels * 3 {
        return Err("OCR raster length differs from its grid".into());
    }
    let (detection_model, detection_hash) = load_model(&models.detection)?;
    let (recognition_model, recognition_hash) = load_model(&models.recognition)?;
    let engine = OcrEngine::new(OcrEngineParams {
        detection_model: Some(detection_model),
        recognition_model: Some(recognition_model),
        ..Default::default()
    })
    .map_err(|error| error.to_string())?;
    let image = engine
        .prepare_input(
            ImageSource::from_bytes(&rgb, (request.width, request.height))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    let words = engine
        .detect_words(&image)
        .map_err(|error| error.to_string())?;
    if words.len() > MAX_WORDS {
        return Err(limit("OCR detection exceeds the word budget"));
    }
    let mut selected = Vec::new();
    let mut skipped_native_words = 0usize;
    for word in words {
        let bounds = pixel_bounds(
            word.bounding_rect().integral_bounding_rect(),
            request.width,
            request.height,
        )?;
        if native_covers(bounds, &request.native_rects) {
            skipped_native_words += 1;
        } else {
            selected.push(word);
        }
    }
    let lines = engine.find_text_lines(&image, &selected);
    let predictions = engine
        .recognize_text(&image, &lines)
        .map_err(|error| error.to_string())?;
    let mut lines = Vec::new();
    let mut text_bytes = 0usize;
    for prediction in predictions.into_iter().flatten() {
        let text = prediction.to_string();
        text_bytes = text_bytes.saturating_add(text.len());
        if text_bytes > MAX_OUTPUT / 4 {
            return Err(limit("OCR text exceeds the output budget"));
        }
        let bounds = pixel_bounds(prediction.bounding_rect(), request.width, request.height)?;
        let words = prediction
            .words()
            .map(|word| {
                Ok(RecognizedWord {
                    text: word.to_string(),
                    pixel_bounds: pixel_bounds(
                        word.bounding_rect(),
                        request.width,
                        request.height,
                    )?,
                    confidence: None,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        lines.push(Line {
            text,
            bounds,
            words,
        });
    }
    Ok(Response::Recognized {
        detection_hash,
        recognition_hash,
        lines,
        skipped_native_words,
    })
}

fn load_model(path: &Path) -> Result<(rten::Model, String), Failure> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("OCR model {}: {error}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(MAX_MODEL_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_MODEL_BYTES {
        return Err(limit("OCR model exceeds the byte limit"));
    }
    let hash = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    rten::Model::load(bytes)
        .map(|model| (model, hash))
        .map_err(|error| format!("OCR model {}: {error}", path.display()).into())
}

fn pixel_bounds(rect: rten_imageproc::Rect, width: u32, height: u32) -> Result<[u32; 4], String> {
    let bounds = [rect.left(), rect.top(), rect.right(), rect.bottom()];
    if bounds[0] < 0
        || bounds[1] < 0
        || bounds[0] >= bounds[2]
        || bounds[1] >= bounds[3]
        || i64::from(bounds[2]) > i64::from(width)
        || i64::from(bounds[3]) > i64::from(height)
    {
        return Err("OCR rectangle lies outside the source raster".into());
    }
    Ok(bounds.map(|value| value as u32))
}

fn native_covers(word: [u32; 4], native: &[[u32; 4]]) -> bool {
    let mut spans: Vec<_> = native
        .iter()
        .filter(|rect| rect[1] <= word[1] && rect[3] >= word[3])
        .map(|rect| (rect[0], rect[2]))
        .collect();
    spans.sort_unstable();
    let mut right = word[0];
    for (start, end) in spans {
        if start > right {
            break;
        }
        right = right.max(end);
        if right >= word[2] {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::native_covers;

    #[test]
    fn native_coverage_requires_a_gapless_union_at_full_word_height() {
        let word = [10, 10, 30, 20];
        assert!(native_covers(word, &[[20, 9, 31, 21], [9, 9, 20, 21]]));
        assert!(!native_covers(word, &[[9, 9, 19, 21], [20, 9, 31, 21]]));
        assert!(!native_covers(word, &[[9, 11, 31, 21]]));
        assert!(!native_covers(word, &[]));
    }
}
