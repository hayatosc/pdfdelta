use rten::Model;
use rten_tensor::{NdTensor, prelude::*};
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 6 && args[1] == "--page" {
        return page(&args[2], &args[3], &args[4], &args[5]);
    }
    if args.len() == 4 && args[2] == "--detect" {
        return detect(&args[1], &args[3]);
    }
    if args.len() != 4 {
        return Err("expected model dictionary image".into());
    }
    let model = Model::load_file(&args[1])?;
    let dictionary = std::fs::read_to_string(&args[2])?;
    let mut alphabet = vec![""];
    alphabet.extend(dictionary.lines());
    alphabet.push(" ");
    let image = image::open(&args[3])?.to_rgb8();
    let width = (48 * image.width()).div_ceil(image.height()).max(8);
    if width > 2048 {
        return Err("crop width limit".into());
    }
    let resized = image::imageops::resize(&image, width, 48, image::imageops::FilterType::Triangle);
    let input = NdTensor::from_fn([1, 3, 48, width as usize], |[_, channel, y, x]| {
        (f32::from(resized.get_pixel(x as u32, y as u32)[2 - channel]) / 255.0 - 0.5) / 0.5
    });
    let output: NdTensor<f32, 3> = model.run_one((&input).into(), None)?.try_into()?;
    let [batch, steps, classes] = output.shape();
    if batch != 1 || classes != alphabet.len() || steps > 4096 {
        return Err(format!(
            "invalid output {:?}; dictionary {}",
            output.shape(),
            alphabet.len()
        )
        .into());
    }
    let mut text = String::new();
    let mut previous = 0;
    let mut minimum = 1.0_f32;
    for step in 0..steps {
        let mut best = (0, f32::NEG_INFINITY);
        for class in 0..classes {
            let score = output[[0, step, class]];
            if !score.is_finite() || !(0.0..=1.0).contains(&score) {
                return Err("nonprobability output".into());
            }
            if score > best.1 {
                best = (class, score);
            }
        }
        if best.0 != 0 && best.0 != previous {
            text.push_str(alphabet[best.0]);
            minimum = minimum.min(best.1);
        }
        previous = best.0;
    }
    println!(
        "text={text:?} minimum_selected_score={minimum} shape={:?}",
        output.shape()
    );
    Ok(())
}

fn detect(path: &str, image_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::load_file(path)?;
    let image = image::open(image_path)?.to_rgb8();
    let ratio = (960.0 / image.width().max(image.height()) as f64).min(1.0);
    let round = |v: u32| ((f64::from(v) * ratio / 32.0).round().max(1.0) as u32) * 32;
    let (width, height) = (round(image.width()), round(image.height()));
    let resized =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle);
    let means = [0.485, 0.456, 0.406];
    let stds = [0.229, 0.224, 0.225];
    let input = NdTensor::from_fn([1, 3, height as usize, width as usize], |[_, c, y, x]| {
        (f32::from(resized.get_pixel(x as u32, y as u32)[2 - c]) / 255.0 - means[c]) / stds[c]
    });
    let output: NdTensor<f32, 4> = model.run_one((&input).into(), None)?.try_into()?;
    let [batch, channels, h, w] = output.shape();
    if batch != 1 || channels != 1 || h * w > 960 * 960 {
        return Err("invalid detector output".into());
    }
    let mut minimum = 1.0_f32;
    let mut maximum = 0.0_f32;
    let mut foreground = 0;
    for y in 0..h {
        for x in 0..w {
            let v = output[[0, 0, y, x]];
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err("nonprobability detector output".into());
            }
            minimum = minimum.min(v);
            maximum = maximum.max(v);
            foreground += usize::from(v > 0.3);
        }
    }
    println!(
        "detector_shape={:?} minimum={minimum} maximum={maximum} above_0.3={foreground}",
        output.shape()
    );
    Ok(())
}

// Timing prototype only: connected boxes with axis-aligned padding are not a
// production DB polygon decoder, and detection omissions are not resolved.
fn page(det: &str, rec: &str, dict: &str, path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let detector = Model::load_file(det)?;
    let recognizer = Model::load_file(rec)?;
    let dictionary = std::fs::read_to_string(dict)?;
    let mut alphabet = vec![""];
    alphabet.extend(dictionary.lines());
    alphabet.push(" ");
    let loaded = started.elapsed();
    let image = image::open(path)?.to_rgb8();
    if u64::from(image.width()) * u64::from(image.height()) > 9_000_000 {
        return Err("controlled page exceeds pixel budget".into());
    }
    let ratio = (960.0 / f64::from(image.width().max(image.height()))).min(1.0);
    let round = |v: u32| ((f64::from(v) * ratio / 32.0).round().max(1.0) as u32) * 32;
    let (width, height) = (round(image.width()), round(image.height()));
    let resized =
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Triangle);
    let means = [0.485, 0.456, 0.406];
    let stds = [0.229, 0.224, 0.225];
    let input = NdTensor::from_fn([1, 3, height as usize, width as usize], |[_, c, y, x]| {
        (f32::from(resized.get_pixel(x as u32, y as u32)[2 - c]) / 255.0 - means[c]) / stds[c]
    });
    let output: NdTensor<f32, 4> = detector.run_one((&input).into(), None)?.try_into()?;
    let [batch, channels, h, w] = output.shape();
    if batch != 1 || channels != 1 || h * w > 960 * 960 {
        return Err("unexpected detector shape".into());
    }
    let mut remaining = vec![false; h * w];
    for y in 0..h {
        for x in 0..w {
            let value = output[[0, 0, y, x]];
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err("nonprobability detector output".into());
            }
            remaining[y * w + x] = value > 0.3;
        }
    }
    let mut boxes = Vec::new();
    for [x0, y0, x1, y1] in component_boxes(remaining, w, h, 1024)? {
        let pad = y1 - y0;
        let scale_x = |x: usize| (x as f64 * f64::from(image.width()) / w as f64) as u32;
        let scale_y = |y: usize| (y as f64 * f64::from(image.height()) / h as f64) as u32;
        boxes.push([
            scale_x(x0.saturating_sub(pad)),
            scale_y(y0.saturating_sub(pad)),
            scale_x((x1 + pad).min(w)),
            scale_y((y1 + pad).min(h)),
        ]);
    }
    boxes.sort_by_key(|b| (b[1], b[0]));
    let detected = started.elapsed();
    let mut lines = Vec::new();
    for bounds in boxes {
        let [x0, y0, x1, y1] = bounds;
        if x1 <= x0 || y1 <= y0 {
            return Err("empty detected region".into());
        }
        let crop = image::imageops::crop_imm(&image, x0, y0, x1 - x0, y1 - y0).to_image();
        let width = (48 * crop.width()).div_ceil(crop.height()).max(8);
        if width > 2048 {
            return Err("recognition width budget".into());
        }
        let resized =
            image::imageops::resize(&crop, width, 48, image::imageops::FilterType::Triangle);
        let input = NdTensor::from_fn([1, 3, 48, width as usize], |[_, c, y, x]| {
            (f32::from(resized.get_pixel(x as u32, y as u32)[2 - c]) / 255.0 - 0.5) / 0.5
        });
        let output: NdTensor<f32, 3> = recognizer.run_one((&input).into(), None)?.try_into()?;
        let [batch, steps, classes] = output.shape();
        if batch != 1 || classes != alphabet.len() || steps > 4096 {
            return Err("recognizer shape or dictionary mismatch".into());
        }
        let mut text = String::new();
        let mut previous = 0;
        let mut score: Option<f32> = None;
        for step in 0..steps {
            let mut best = (0, f32::NEG_INFINITY);
            for class in 0..classes {
                let value = output[[0, step, class]];
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err("nonprobability recognition output".into());
                }
                if value > best.1 {
                    best = (class, value);
                }
            }
            if best.0 != 0 && best.0 != previous {
                text.push_str(alphabet[best.0]);
                score = Some(score.map_or(best.1, |v| v.min(best.1)));
            }
            previous = best.0;
        }
        lines.push(serde_json::json!({"bounds":bounds,"text":text,"minimum_selected_score":score}));
    }
    println!(
        "{}",
        serde_json::json!({"width":image.width(),"height":image.height(),
        "load_seconds":loaded.as_secs_f64(), "decode_detect_seconds":(detected-loaded).as_secs_f64(),
        "recognize_seconds":(started.elapsed()-detected).as_secs_f64(),
        "total_seconds":started.elapsed().as_secs_f64(), "lines":lines,
        "inventory_complete":false,"prototype_localization":true,"localization_profile":"eight-connected-axis-box-v2"})
    );
    Ok(())
}

// Diagonal foreground pixels belong to one region. Uncertain detector output
// remains a candidate regardless of connectivity or downstream recognition.
fn component_boxes(
    mut remaining: Vec<bool>,
    w: usize,
    h: usize,
    limit: usize,
) -> Result<Vec<[usize; 4]>, Box<dyn std::error::Error>> {
    if w == 0 || h == 0 || w.checked_mul(h) != Some(remaining.len()) || remaining.len() > 960 * 960
    {
        return Err("invalid bounded detector bitmap".into());
    }
    let mut boxes = Vec::new();
    for index in 0..remaining.len() {
        if !remaining[index] {
            continue;
        }
        remaining[index] = false;
        let mut stack = vec![index];
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
            for (nx, ny) in [
                (x.wrapping_sub(1), y),
                (x + 1, y),
                (x, y.wrapping_sub(1)),
                (x, y + 1),
                (x.wrapping_sub(1), y.wrapping_sub(1)),
                (x + 1, y.wrapping_sub(1)),
                (x.wrapping_sub(1), y + 1),
                (x + 1, y + 1),
            ] {
                if nx < w && ny < h && remaining[ny * w + nx] {
                    remaining[ny * w + nx] = false;
                    stack.push(ny * w + nx);
                }
            }
        }
        if boxes.len() >= limit {
            return Err("detected region budget".into());
        }
        boxes.push([x0, y0, x1, y1]);
    }
    Ok(boxes)
}

#[cfg(test)]
mod tests {
    use super::component_boxes;

    #[test]
    fn diagonal_connections_share_a_region_without_merging_separated_pixels() {
        assert_eq!(
            component_boxes(vec![true, false, false, true], 2, 2, 1).unwrap(),
            vec![[0, 0, 2, 2]]
        );
        assert_eq!(
            component_boxes(vec![true, false, true], 3, 1, 2).unwrap(),
            vec![[0, 0, 1, 1], [2, 0, 3, 1]]
        );
        assert!(component_boxes(vec![true, false, true], 3, 1, 1).is_err());
    }

    #[test]
    fn invalid_dimensions_and_empty_foreground_remain_distinct() {
        assert!(component_boxes(vec![false], 0, 1, 1).is_err());
        assert!(component_boxes(vec![false], 2, 1, 1).is_err());
        assert!(component_boxes(vec![false; 4], 2, 2, 1).unwrap().is_empty());
    }
}
