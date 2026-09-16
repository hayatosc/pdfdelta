//! Isolated image decoding. The parent receives hashes and placement evidence,
//! never parser or renderer objects or an unbounded decoded raster.

use std::{
    io::{Read, Write},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use hayro::{
    hayro_interpret::{self as hi, Device, Image, ImageData, Paint, TransformExt},
    vello_cpu::kurbo::{Affine, BezPath, Rect},
};
use pdfdelta_core::{
    document::image_diff::{
        ImageInventory, ImageOccurrence, MAX_IMAGE_PIXELS, MAX_IMAGES, rgba_hash,
    },
    model::PageId,
    pdf::{ObjectRef, PageRef, ParseLimits},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MAX_RESPONSE: usize = 8 * 1024 * 1024;
const MAX_TOTAL_PIXELS: u64 = 64_000_000;

fn revision(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Response {
    revision: String,
    pages: Vec<ObjectRef>,
    inventory: ImageInventory,
}

pub fn collect(bytes: &[u8], pages: &[PageRef], password_supplied: bool) -> ImageInventory {
    let result = (|| {
        if password_supplied {
            return Err("password-assisted image decoding is not implemented".into());
        }
        let mut command =
            Command::new(std::env::current_exe().map_err(|e| format!("image worker: {e}"))?);
        command.arg("hash-images");
        let output =
            crate::render::run_bounded(&mut command, bytes, MAX_RESPONSE, Duration::from_secs(30))
                .map_err(|(kind, reason)| format!("{kind:?}: {reason}"))?;
        let response: Response =
            serde_json::from_slice(&output).map_err(|e| format!("invalid image response: {e}"))?;
        if response.revision != revision(bytes)
            || response.pages != pages.iter().map(|p| p.0).collect::<Vec<_>>()
            || response
                .inventory
                .images
                .iter()
                .any(|image| image.page.0 as usize >= pages.len())
        {
            return Err("image worker input or page identity mismatch".into());
        }
        response.inventory.validate().map_err(|e| e.to_string())?;
        Ok(response.inventory)
    })();
    result.unwrap_or_else(|reason| ImageInventory {
        images: Vec::new(),
        complete: false,
        issues: vec![reason],
    })
}

pub fn worker() -> Result<(), u8> {
    crate::render::restrict_process(25)?;
    let mut bytes = Vec::new();
    let limit = ParseLimits::default().max_input_bytes;
    std::io::stdin()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| 2)?;
    if bytes.len() > limit {
        return Err(3);
    }
    let revision = revision(&bytes);
    let pdf = hayro::hayro_syntax::Pdf::new(bytes).map_err(|_| 2)?;
    if pdf.pages().len() > ParseLimits::default().max_pages {
        return Err(3);
    }
    let warned = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&warned);
    let settings = hi::InterpreterSettings {
        warning_sink: Arc::new(move |_| {
            sink.store(true, Ordering::Relaxed);
        }),
        render_annotations: false,
        ..Default::default()
    };
    let mut device = Images {
        inventory: ImageInventory {
            complete: true,
            ..Default::default()
        },
        page: PageId(0),
        occurrence: 0,
        pixels: 0,
        limited: false,
    };
    let mut pages = Vec::new();
    for (index, page) in pdf.pages().iter().enumerate() {
        let reference = page.raw().obj_id().ok_or(2)?;
        pages.push(ObjectRef {
            object_number: u32::try_from(reference.obj_number).map_err(|_| 2)?,
            generation: u16::try_from(reference.gen_number).map_err(|_| 2)?,
        });
        device.page = PageId(index as u32);
        device.occurrence = 0;
        // Annotation appearances and patterns require their own acquisition contract.
        if page
            .raw()
            .get::<hayro::hayro_syntax::object::Array<'_>>(b"Annots" as &[u8])
            .is_some()
        {
            device.incomplete("annotation image inventory is not examined");
        }
        let (width, height) = page.render_dimensions();
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return Err(2);
        }
        let cache = hi::InterpreterCache::new();
        let mut context = hi::Context::new(
            page.initial_transform(true).to_kurbo(),
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
            &cache,
            page.xref(),
            settings.clone(),
        );
        hi::interpret_page(page, &mut context, &mut device);
        if device.limited {
            return Err(3);
        }
    }
    if warned.load(Ordering::Relaxed) {
        device.incomplete("image interpreter reported warnings; image inventory may be incomplete");
    }
    device.inventory.validate().map_err(|_| 2)?;
    let response = Response {
        revision,
        pages,
        inventory: device.inventory,
    };
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    serde_json::to_writer(
        &mut crate::extraction_cache::CeilingWriter::new(&mut stdout, MAX_RESPONSE),
        &response,
    )
    .map_err(|_| 3)?;
    stdout.flush().map_err(|_| 2)
}

struct Images {
    inventory: ImageInventory,
    page: PageId,
    occurrence: u32,
    pixels: u64,
    limited: bool,
}

// The decoder can silently skip unknown filter names. Reject those and unsupported
// color-key masks before decoding, including filters on intrinsic alpha images.
fn supported_stream(stream: &hayro::hayro_syntax::object::Stream<'_>, depth: usize) -> bool {
    use hayro::hayro_syntax::object::{Name, Object};
    if depth > 8 {
        return false;
    }
    let dict = stream.dict();
    let known = |name: &Name<'_>| {
        matches!(
            name.as_ref(),
            b"ASCIIHexDecode"
                | b"AHx"
                | b"ASCII85Decode"
                | b"A85"
                | b"LZWDecode"
                | b"LZW"
                | b"FlateDecode"
                | b"Fl"
                | b"RunLengthDecode"
                | b"RL"
                | b"CCITTFaxDecode"
                | b"CCF"
                | b"JBIG2Decode"
                | b"DCTDecode"
                | b"DCT"
                | b"JPXDecode"
        )
    };
    for key in [b"Filter".as_slice(), b"F".as_slice()] {
        if !dict.contains_key(key) {
            continue;
        }
        match dict.get::<Object<'_>>(key) {
            Some(Object::Name(name)) if known(&name) => {}
            Some(Object::Array(array)) => {
                let raw_count = array.raw_iter().count();
                let mut count = 0;
                for object in array.iter::<Object<'_>>() {
                    count += 1;
                    if !matches!(object, Object::Name(name) if known(&name)) {
                        return false;
                    }
                }
                if count != raw_count {
                    return false;
                }
            }
            _ => return false,
        }
    }
    for key in [b"SMask".as_slice(), b"Mask".as_slice()] {
        if !dict.contains_key(key) {
            continue;
        }
        match dict.get::<Object<'_>>(key) {
            Some(Object::Stream(mask)) if supported_stream(&mask, depth + 1) => {
                let w = mask.dict().get::<u32>(b"Width" as &[u8]).unwrap_or(0);
                let h = mask.dict().get::<u32>(b"Height" as &[u8]).unwrap_or(0);
                if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_IMAGE_PIXELS as u64 {
                    return false;
                }
            }
            Some(Object::Name(name)) if name.as_ref() == b"None" => {}
            _ => return false,
        }
    }
    true
}

impl Images {
    fn incomplete(&mut self, reason: &str) {
        self.inventory.complete = false;
        if !self.inventory.issues.iter().any(|issue| issue == reason) {
            self.inventory.issues.push(reason.into());
        }
    }

    fn paint(&mut self, paint: &Paint<'_>) {
        if matches!(paint, Paint::Pattern(_)) {
            self.incomplete("pattern image inventory is not examined");
        }
    }
}

impl<'a> Device<'a> for Images {
    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        if self.limited {
            return;
        }
        let (width, height) = (image.width(), image.height());
        let pixels = u64::from(width) * u64::from(height);
        if self.inventory.images.len() == MAX_IMAGES
            || pixels > MAX_IMAGE_PIXELS as u64
            || self.pixels.saturating_add(pixels) > MAX_TOTAL_PIXELS
        {
            self.limited = true;
            return;
        }
        self.pixels += pixels;
        let unit_transform =
            transform * Affine::scale_non_uniform(f64::from(width), f64::from(height));
        if unit_transform.as_coeffs().iter().any(|n| !n.is_finite()) {
            self.incomplete("invalid image placement");
            return;
        }
        let mut occurrence = ImageOccurrence {
            page: self.page,
            occurrence: self.occurrence,
            object: None,
            transform: unit_transform.as_coeffs(),
            width,
            height,
            sha256: None,
            unresolved: Some("image decoding failed or is unsupported".into()),
        };
        self.occurrence += 1;
        if let Image::Raster(raster) = image {
            let reference = raster.stream().obj_id();
            if let (Ok(object_number), Ok(generation)) = (
                u32::try_from(reference.obj_number),
                u16::try_from(reference.gen_number),
            ) && object_number != 0
            {
                occurrence.object = Some(ObjectRef {
                    object_number,
                    generation,
                });
            }
            if !supported_stream(raster.stream(), 0) {
                occurrence.unresolved = Some("unsupported image filter or mask".into());
                self.inventory.complete = false;
                self.inventory.images.push(occurrence);
                return;
            }
            raster.with_rgba(
                |data, alpha| {
                    let (w, h) = (data.width(), data.height());
                    let count = u64::from(w) * u64::from(h);
                    if count == 0 || count > MAX_IMAGE_PIXELS as u64 || w != width || h != height {
                        return;
                    }
                    let count = count as usize;
                    if alpha
                        .as_ref()
                        .is_some_and(|a| a.width != w || a.height != h || a.data.len() != count)
                    {
                        return;
                    }
                    let mut rgba = Vec::with_capacity(count * 4);
                    match data {
                        ImageData::Rgb(data) => {
                            if data.data.len() != count * 3 {
                                return;
                            }
                            for (i, rgb) in data.data.as_chunks::<3>().0.iter().enumerate() {
                                rgba.extend_from_slice(rgb);
                                rgba.push(alpha.as_ref().map_or(255, |a| a.data[i]));
                            }
                        }
                        ImageData::Luma(data) => {
                            if data.data.len() != count {
                                return;
                            }
                            for (i, value) in data.data.iter().enumerate() {
                                rgba.extend_from_slice(&[
                                    *value,
                                    *value,
                                    *value,
                                    alpha.as_ref().map_or(255, |a| a.data[i]),
                                ]);
                            }
                        }
                    }
                    if let Ok(hash) = rgba_hash(w, h, &rgba) {
                        occurrence.sha256 = Some(hash);
                        occurrence.unresolved = None;
                    }
                },
                None,
            );
        } else {
            occurrence.unresolved = Some("stencil image hashing is not implemented".into());
        }
        if occurrence.unresolved.is_some() {
            self.inventory.complete = false;
        }
        self.inventory.images.push(occurrence);
    }
    fn set_soft_mask(&mut self, mask: Option<hi::SoftMask<'a>>) {
        if mask.is_some() {
            self.incomplete("graphics soft-mask image inventory is not examined");
        }
    }
    fn set_blend_mode(&mut self, _: hi::BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, paint: &Paint<'a>, _: &hi::PathDrawMode) {
        self.paint(paint);
    }
    fn push_clip_path(&mut self, _: &hi::ClipPath) {}
    fn pop_clip_path(&mut self) {}
    fn push_transparency_group(
        &mut self,
        _: f32,
        mask: Option<hi::SoftMask<'a>>,
        _: hi::BlendMode,
    ) {
        self.set_soft_mask(mask);
    }
    fn pop_transparency_group(&mut self) {}
    fn draw_glyph(
        &mut self,
        glyph: &hi::font::Glyph<'a>,
        _: Affine,
        _: Affine,
        paint: &Paint<'a>,
        _: &hi::GlyphDrawMode,
    ) {
        self.paint(paint);
        if matches!(glyph, hi::font::Glyph::Type3(_)) {
            self.incomplete("Type3 glyph image inventory is not examined");
        }
    }
}
