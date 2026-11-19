//! Isolated image decoding. The parent receives hashes and placement evidence,
//! never parser or renderer objects or an unbounded decoded raster.

use std::{
    collections::HashMap,
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
    hayro_syntax::object::{Dict, Name, Object, Stream},
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
    #[serde(default)]
    encoded: Option<EncodedPageIdentity>,
}

/// Sufficient page identity from encoded image streams without decoding.
///
/// Equal `sha256` values mean the same ordered image `XObject` streams with the same
/// encoded stream bytes, pixel-affecting dictionaries and CTMs were painted.
/// Different values prove nothing about pixel equality.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncodedPageIdentity {
    pub page: PageId,
    pub complete: bool,
    pub occurrences: u32,
    pub sha256: Option<String>,
    pub issues: Vec<String>,
}

impl EncodedPageIdentity {
    fn fail(&mut self, reason: &str) {
        self.complete = false;
        if !self.issues.iter().any(|issue| issue == reason) {
            self.issues.push(reason.into());
        }
    }

    fn validate(&self, page: PageId) -> Result<(), String> {
        if self.page != page {
            return Err("encoded image identity page mismatch".into());
        }
        let valid_hash = self.sha256.as_ref().is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if self.complete != valid_hash
            || self.complete != self.issues.is_empty()
            || (self.complete && self.occurrences == 0)
        {
            return Err("invalid encoded image identity evidence".into());
        }
        Ok(())
    }
}

const MAX_ENCODED_DEPTH: usize = 8;
const MAX_ENCODED_NODES: usize = 4_096;
const MAX_ENCODED_BYTES: u64 = 256 * 1024 * 1024;
const IMAGE_DICTIONARY_KEYS: [&[u8]; 12] = [
    b"Width",
    b"Height",
    b"ColorSpace",
    b"CS",
    b"BitsPerComponent",
    b"BPC",
    b"Filter",
    b"F",
    b"DecodeParms",
    b"DP",
    b"Decode",
    b"ImageMask",
];
const IMAGE_MASK_KEYS: [&[u8]; 2] = [b"SMask", b"Mask"];

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

pub fn collect_page(
    bytes: &[u8],
    pages: &[PageRef],
    page: PageId,
    password_supplied: bool,
) -> ImageInventory {
    let result = (|| {
        if password_supplied {
            return Err("password-assisted image decoding is not implemented".into());
        }
        if page.0 as usize >= pages.len() {
            return Err("image page index is outside the page list".into());
        }
        let mut command =
            Command::new(std::env::current_exe().map_err(|e| format!("image worker: {e}"))?);
        command
            .arg("hash-image-page")
            .arg("--page")
            .arg(page.0.to_string());
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
                .any(|image| image.page != page)
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

/// Collects the encoded-only identity for one image-only page.
///
/// This never decodes image data. Callers must use an equal hash only as a
/// sufficient equality certificate and fall back to decoded pixels otherwise.
pub fn collect_encoded_page(
    bytes: &[u8],
    pages: &[PageRef],
    page: PageId,
    password_supplied: bool,
) -> EncodedPageIdentity {
    let fail = |reason: String| EncodedPageIdentity {
        page,
        complete: false,
        occurrences: 0,
        sha256: None,
        issues: vec![reason],
    };
    if password_supplied {
        return fail("password-assisted image decoding is not implemented".into());
    }
    if page.0 as usize >= pages.len() {
        return fail("image page index is outside the page list".into());
    }
    let result = (|| {
        let mut command =
            Command::new(std::env::current_exe().map_err(|e| format!("image worker: {e}"))?);
        command
            .arg("hash-image-page-encoded")
            .arg("--page")
            .arg(page.0.to_string());
        let output =
            crate::render::run_bounded(&mut command, bytes, MAX_RESPONSE, Duration::from_secs(30))
                .map_err(|(kind, reason)| format!("{kind:?}: {reason}"))?;
        let response: Response =
            serde_json::from_slice(&output).map_err(|e| format!("invalid image response: {e}"))?;
        if response.revision != revision(bytes)
            || response.pages != pages.iter().map(|p| p.0).collect::<Vec<_>>()
        {
            return Err("image worker input or page identity mismatch".into());
        }
        let encoded = response
            .encoded
            .ok_or_else(|| "encoded image identity response is missing".to_owned())?;
        encoded.validate(page)?;
        Ok(encoded)
    })();
    result.unwrap_or_else(fail)
}

pub fn worker() -> Result<(), u8> {
    worker_inner(None, true, false)
}

pub fn worker_page(page: usize) -> Result<(), u8> {
    worker_inner(Some(page), true, false)
}

pub fn worker_encoded_page(page: usize) -> Result<(), u8> {
    worker_inner(Some(page), false, true)
}

fn worker_inner(
    selected_page: Option<usize>,
    decode: bool,
    record_encoded: bool,
) -> Result<(), u8> {
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
        decode,
        record_encoded,
        encoded: EncodedPageIdentity {
            page: PageId(0),
            complete: false,
            occurrences: 0,
            sha256: None,
            issues: Vec::new(),
        },
        encoded_hash: Sha256::new(),
        encoded_state: EncodedHashState::default(),
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
        if selected_page.is_some_and(|selected| selected != index) {
            continue;
        }
        if record_encoded {
            device.encoded = EncodedPageIdentity {
                page: device.page,
                complete: true,
                occurrences: 0,
                sha256: None,
                issues: Vec::new(),
            };
            device.encoded_hash = Sha256::new();
            device.encoded_hash.update(b"flow-image-encoded-page-v1\0");
            device.encoded_state = EncodedHashState::default();
        }
        // Annotation appearances and patterns require their own acquisition contract.
        if page
            .raw()
            .get::<hayro::hayro_syntax::object::Array<'_>>(b"Annots" as &[u8])
            .is_some()
        {
            device.incomplete("annotation image inventory is not examined");
            device
                .encoded
                .fail("annotation image inventory is not examined");
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
        device
            .encoded
            .fail("image interpreter reported warnings; encoded image identity may be incomplete");
    }
    device.inventory.validate().map_err(|_| 2)?;
    let encoded = if record_encoded {
        if device.encoded.occurrences == 0 {
            device.encoded.fail("no painted image was found");
        }
        if device.encoded.complete {
            device.encoded.sha256 = Some(digest_hex(&device.encoded_hash.finalize()));
        }
        device
            .encoded
            .validate(device.encoded.page)
            .map_err(|_| 2)?;
        Some(device.encoded)
    } else {
        None
    };
    let response = Response {
        revision,
        pages,
        inventory: device.inventory,
        encoded,
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
    decode: bool,
    record_encoded: bool,
    encoded: EncodedPageIdentity,
    encoded_hash: Sha256,
    encoded_state: EncodedHashState,
}

#[derive(Default)]
struct EncodedHashState {
    bytes: u64,
    nodes: usize,
    image_memo: HashMap<(i32, i32), String>,
    generic_memo: HashMap<(i32, i32), String>,
}

impl EncodedHashState {
    fn spend_node(&mut self) -> Option<()> {
        if self.nodes >= MAX_ENCODED_NODES {
            return None;
        }
        self.nodes += 1;
        Some(())
    }

    fn account_bytes(&mut self, bytes: u64) -> Option<()> {
        self.bytes = self.bytes.checked_add(bytes)?;
        (self.bytes <= MAX_ENCODED_BYTES).then_some(())
    }

    fn image_stream_identity(&mut self, stream: &Stream<'_>, depth: usize) -> Option<String> {
        if depth > MAX_ENCODED_DEPTH {
            return None;
        }
        let id = stream.obj_id();
        let key = (id.obj_number, id.gen_number);
        if key.0 != 0
            && let Some(hash) = self.image_memo.get(&key)
        {
            return Some(hash.clone());
        }
        let mut hasher = Sha256::new();
        hasher.update(b"image-stream\0");
        self.hash_image_dict(stream.dict(), &mut hasher, depth + 1)?;
        let raw = stream.raw_data();
        self.account_bytes(raw.len() as u64)?;
        hasher.update(b"raw\0");
        hasher.update(raw.as_ref());
        let hash = digest_hex(&hasher.finalize());
        if key.0 != 0 {
            self.image_memo.insert(key, hash.clone());
        }
        Some(hash)
    }

    fn generic_stream_identity(&mut self, stream: &Stream<'_>, depth: usize) -> Option<String> {
        if depth > MAX_ENCODED_DEPTH {
            return None;
        }
        let id = stream.obj_id();
        let key = (id.obj_number, id.gen_number);
        if key.0 != 0
            && let Some(hash) = self.generic_memo.get(&key)
        {
            return Some(hash.clone());
        }
        let mut hasher = Sha256::new();
        hasher.update(b"generic-stream\0");
        let dict = stream.dict();
        let mut keys = dict.keys().collect::<Vec<Name<'_>>>();
        keys.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
        hasher.update((keys.len() as u64).to_be_bytes());
        for key in keys {
            hasher.update(key.as_ref());
            match dict.get::<Object<'_>>(key.as_ref()) {
                Some(value) => {
                    hasher.update([1]);
                    self.hash_object(&value, &mut hasher, depth + 1)?;
                }
                None => hasher.update([0]),
            }
        }
        let raw = stream.raw_data();
        self.account_bytes(raw.len() as u64)?;
        hasher.update(b"raw\0");
        hasher.update(raw.as_ref());
        let hash = digest_hex(&hasher.finalize());
        if key.0 != 0 {
            self.generic_memo.insert(key, hash.clone());
        }
        Some(hash)
    }

    fn hash_image_dict(
        &mut self,
        dict: &Dict<'_>,
        hasher: &mut Sha256,
        depth: usize,
    ) -> Option<()> {
        if depth > MAX_ENCODED_DEPTH {
            return None;
        }
        self.spend_node()?;
        for key in IMAGE_DICTIONARY_KEYS {
            hasher.update(key);
            hasher.update([0]);
            match dict.get::<Object<'_>>(key) {
                Some(value) => {
                    hasher.update([1]);
                    self.hash_object(&value, hasher, depth + 1)?;
                }
                None => hasher.update([0]),
            }
        }
        for key in IMAGE_MASK_KEYS {
            hasher.update(key);
            hasher.update([0]);
            match dict.get::<Object<'_>>(key) {
                Some(Object::Stream(stream)) => {
                    hasher.update([2]);
                    let hash = self.image_stream_identity(&stream, depth + 1)?;
                    hasher.update(hash.as_bytes());
                }
                Some(value) => {
                    hasher.update([1]);
                    self.hash_object(&value, hasher, depth + 1)?;
                }
                None => hasher.update([0]),
            }
        }
        Some(())
    }

    fn hash_object(
        &mut self,
        object: &Object<'_>,
        hasher: &mut Sha256,
        depth: usize,
    ) -> Option<()> {
        if depth > MAX_ENCODED_DEPTH {
            return None;
        }
        self.spend_node()?;
        match object {
            Object::Null(_) => hasher.update(b"null"),
            Object::Boolean(value) => hasher.update([b'b', u8::from(*value)]),
            Object::Number(value) => {
                hasher.update(b"number");
                hasher.update(value.as_f64().to_bits().to_be_bytes());
            }
            Object::String(value) => {
                hasher.update(b"string");
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
            Object::Name(value) => {
                hasher.update(b"name");
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_ref());
            }
            Object::Array(array) => {
                hasher.update(b"array");
                hasher.update((array.raw_iter().count() as u64).to_be_bytes());
                for item in array.iter::<Object<'_>>() {
                    self.hash_object(&item, hasher, depth + 1)?;
                }
            }
            Object::Dict(dict) => {
                let mut keys = dict.keys().collect::<Vec<Name<'_>>>();
                keys.sort_by(|left, right| left.as_ref().cmp(right.as_ref()));
                hasher.update(b"dict");
                hasher.update((keys.len() as u64).to_be_bytes());
                for key in keys {
                    hasher.update(key.as_ref());
                    match dict.get::<Object<'_>>(key.as_ref()) {
                        Some(value) => {
                            hasher.update([1]);
                            self.hash_object(&value, hasher, depth + 1)?;
                        }
                        None => hasher.update([0]),
                    }
                }
            }
            Object::Stream(stream) => {
                hasher.update(b"stream");
                let hash = self.generic_stream_identity(stream, depth + 1)?;
                hasher.update(hash.as_bytes());
            }
        }
        Some(())
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
            self.encoded.fail("pattern image inventory is not examined");
        }
    }

    fn hash_encoded_image(&mut self, image: &Image<'_, '_>, transform: Affine) {
        let Image::Raster(raster) = image else {
            self.encoded
                .fail("stencil image encoded identity is not implemented");
            return;
        };
        let (width, height) = (image.width(), image.height());
        let coefficients = transform.as_coeffs();
        if coefficients.iter().any(|value| !value.is_finite()) {
            self.encoded.fail("invalid encoded image placement");
            return;
        }
        let Some(identity) = self.encoded_state.image_stream_identity(raster.stream(), 0) else {
            self.encoded
                .fail("encoded image stream identity is unavailable");
            return;
        };
        let Some(occurrence) = self.encoded.occurrences.checked_add(1) else {
            self.encoded
                .fail("encoded image occurrence address space is exhausted");
            return;
        };
        let hasher = &mut self.encoded_hash;
        hasher.update(b"image\0");
        hasher.update(self.encoded.occurrences.to_be_bytes());
        for value in coefficients {
            hasher.update((if value == 0.0 { 0 } else { value.to_bits() }).to_be_bytes());
        }
        hasher.update(width.to_be_bytes());
        hasher.update(height.to_be_bytes());
        hasher.update(identity.as_bytes());
        self.encoded.occurrences = occurrence;
    }
}

impl<'a> Device<'a> for Images {
    fn draw_image(&mut self, image: Image<'a, '_>, transform: Affine) {
        if self.record_encoded {
            self.hash_encoded_image(&image, transform);
        }
        if !self.decode {
            return;
        }
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
            self.encoded
                .fail("graphics soft-mask image inventory is not examined");
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
            self.encoded
                .fail("Type3 glyph image inventory is not examined");
        }
    }
}
