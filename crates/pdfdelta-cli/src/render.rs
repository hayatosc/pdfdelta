//! Bounded local rendering in a child of this executable. PDF bytes stay local;
//! the core receives only neutral raster evidence, never renderer-library types.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use hayro::hayro_syntax::{
    object::{Dict, Name, Stream},
    page::Resources,
};
use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, EvidenceFailure, EvidenceIssue,
        EvidenceLimits, EvidenceStore, Raster, RenderedEvidence, SourceRef,
    },
    model::Vec2,
    pdf::{ObjectRef, PageRef, ParseLimits},
};

const MAX_PIXELS: usize = 8_000_000;

/// Upper bound on one image `XObject`'s declared pixel count before rendering.
///
/// Hayro 0.7.1 expands an image to an RGBA buffer before downscaling it to the
/// page. One 1-bit mask in the real-world corpus is 19,848 x 28,063 pixels
/// (556,994,424 pixels); the renderer requested a 2 GiB allocation and aborted
/// under the render child's 2 GiB address-space limit. The 64M-pixel bound
/// stays above the panel's ordinary images while keeping the worst-case RGBA
/// conversion (256 MiB) well inside the child's resource limits.
const MAX_RENDER_IMAGE_PIXELS: u64 = 64_000_000;

/// Bounds `XObject` scanning in the render preflight.
const MAX_PREFLIGHT_XOBJECTS: usize = 4_096;
const MAX_PREFLIGHT_DEPTH: usize = 8;

/// CPU seconds allowed to one page's render child.
///
/// Measured over 4,654 pages of the 36-pair channel panel: p99.9 0.51 s and
/// max 0.84 s. Five seconds is a sixfold margin over the slowest observed page
/// and leaves room for a slower machine or a colder cache.
const PAGE_CPU_BUDGET_SECONDS: u64 = 5;

/// Wall-clock safety stop for one page.
///
/// The deterministic budget is [`PAGE_CPU_BUDGET_SECONDS`]. This cap is 24
/// times that budget and only stops a page that is blocked or descheduled far
/// beyond the measured distribution. Its expiry is reported as a distinct
/// resource-limit reason because the outcome depends on machine load.
const PAGE_WALL_SAFETY: Duration = Duration::from_secs(120);

/// Cumulative child CPU allowed to all rendered pages of one input document.
///
/// Measured per input over the 36-pair channel panel: max 26.4 s, p99 26.4 s,
/// p90 9.3 s, median 1.1 s. Sixty seconds is a 2.3-fold margin over the
/// slowest observed document and still bounds a hostile input that yields many
/// cheap pages. The page that pushes the cumulative total over the budget is
/// retained; the total is therefore at most this budget plus one page budget.
const DOCUMENT_CPU_BUDGET_SECONDS: u64 = 60;
const DOCUMENT_CPU_BUDGET_MICROS: u64 = DOCUMENT_CPU_BUDGET_SECONDS * 1_000_000;
const DOCUMENT_CPU_BUDGET_REASON: &str =
    "document CPU budget exhausted; this outcome is deterministic for this input";

/// Wall-clock safety stop for one document's rendering loop.
///
/// Five times the document CPU budget. The CPU budget is the deterministic
/// limit; this cap only stops a page stream that is blocked or descheduled far
/// beyond the measured distribution. Its expiry is reported as a distinct
/// resource-limit reason that depends on machine load.
const DOCUMENT_WALL_SAFETY: Duration = Duration::from_secs(300);

/// Length of the complete render response: warning flag plus CPU micros.
const RESPONSE_HEADER: usize = 1 + 8;

/// Cumulative child CPU used by one input document's render pages.
///
/// Successful pages report their own process CPU. A failed page still consumed
/// up to [`PAGE_CPU_BUDGET_SECONDS`], so it is charged that full budget; this
/// keeps a stream of failed pages from bypassing the document budget.
#[derive(Clone, Copy, Debug)]
struct DocumentCpu {
    used_micros: u64,
    budget_micros: u64,
}

impl DocumentCpu {
    const fn new(budget_micros: u64) -> Self {
        Self {
            used_micros: 0,
            budget_micros,
        }
    }

    const fn exhausted(&self) -> bool {
        self.used_micros >= self.budget_micros
    }

    fn charge(&mut self, micros: u64) {
        self.used_micros = self.used_micros.saturating_add(micros);
    }

    fn charge_failed_page(&mut self) {
        self.charge(PAGE_CPU_BUDGET_SECONDS.saturating_mul(1_000_000));
    }
}

/// Retains partial page evidence without claiming renderer or annotation coverage.
pub fn collect(
    store: &mut EvidenceStore,
    bytes: &[u8],
    page_refs: &[PageRef],
    password_supplied: bool,
) {
    collect_with_limits(
        store,
        bytes,
        page_refs,
        password_supplied,
        DOCUMENT_CPU_BUDGET_MICROS,
        DOCUMENT_WALL_SAFETY,
    );
}

/// Collects rendered page evidence with explicit document budgets.
fn collect_with_limits(
    store: &mut EvidenceStore,
    bytes: &[u8],
    page_refs: &[PageRef],
    password_supplied: bool,
    document_cpu_budget_micros: u64,
    document_wall_safety: Duration,
) {
    let backend = store.backends.len();
    store.backends.push(BackendIdentity {
        kind: BackendKind::Renderer,
        name: "hayro".into(),
        version: "0.7.1".into(),
        profile: "page-rgb-white-72dpi-annotations-v1".into(),
        model: None,
    });
    let started = Instant::now();
    let mut retained_bytes = 0usize;
    let mut document_cpu = DocumentCpu::new(document_cpu_budget_micros);
    for page in &store.pages {
        let result = (|| {
            if password_supplied {
                return Err((
                    EvidenceFailure::Unsupported,
                    "password-assisted rendering is not implemented".into(),
                ));
            }
            let bounds = page.bounds.ok_or_else(|| {
                (
                    EvidenceFailure::Unresolved,
                    "native page geometry is unavailable".into(),
                )
            })?;
            let width = bounds.max.x - bounds.min.x;
            let height = bounds.max.y - bounds.min.y;
            if !width.is_finite()
                || !height.is_finite()
                || width <= 0.0
                || height <= 0.0
                || width.ceil() > f64::from(u16::MAX)
                || height.ceil() > f64::from(u16::MAX)
            {
                return Err((
                    EvidenceFailure::ResourceLimit,
                    "page dimensions exceed the raster limit".into(),
                ));
            }
            let (width, height) = (width.ceil() as u16, height.ceil() as u16);
            let pixels = usize::from(width) * usize::from(height);
            if pixels > MAX_PIXELS
                || retained_bytes.saturating_add(pixels * 3)
                    > EvidenceLimits::default().max_raster_bytes
            {
                return Err((
                    EvidenceFailure::ResourceLimit,
                    "retained raster budget exhausted".into(),
                ));
            }
            if document_cpu.exhausted() {
                return Err((
                    EvidenceFailure::ResourceLimit,
                    DOCUMENT_CPU_BUDGET_REASON.into(),
                ));
            }
            let remaining = document_wall_safety
                .checked_sub(started.elapsed())
                .ok_or_else(|| {
                    (
                        EvidenceFailure::ResourceLimit,
                        "document wall-clock safety stop expired; this outcome depends on machine load".into(),
                    )
                })?;
            let rendered = render_page(
                bytes,
                page.page.0 as usize,
                store.pages.len(),
                width,
                height,
                page_refs
                    .get(page.page.0 as usize)
                    .ok_or_else(|| {
                        (
                            EvidenceFailure::Unresolved,
                            "native page reference is unavailable".into(),
                        )
                    })?
                    .0,
                remaining.min(PAGE_WALL_SAFETY),
            );
            if rendered.is_err() {
                // A failed render attempt can consume up to the per-page CPU
                // budget without returning a complete response, so charge the
                // full page budget. A page rejected before the attempt (for
                // example by the raster budget) consumes no child CPU.
                document_cpu.charge_failed_page();
            }
            let (raster, warned, page_cpu_micros) = rendered?;
            document_cpu.charge(page_cpu_micros);
            Ok((bounds, raster, warned))
        })();
        let mut sources = Vec::new();
        let (kind, reason) = match result {
            Ok((bounds, raster, warned)) => {
                let id = store.rendered.len() as u64;
                retained_bytes += raster.rgb.len();
                store.rendered.push(RenderedEvidence {
                    id,
                    page: page.page,
                    backend,
                    raster,
                    composited_page: true,
                    polygon: vec![
                        Vec2 {
                            x: bounds.min.x,
                            y: bounds.max.y,
                        },
                        bounds.max,
                        Vec2 {
                            x: bounds.max.x,
                            y: bounds.min.y,
                        },
                        bounds.min,
                    ],
                });
                sources.push(SourceRef::Rendered { region: id });
                (EvidenceFailure::Unresolved, if warned {
                    "renderer reported interpretation warnings; retained pixels do not establish complete visible content"
                } else {
                    "page pixels retained; renderer feature coverage, annotation completeness, and content-region interpretation remain unverified"
                }.into())
            }
            Err(error) => error,
        };
        store.inventories.push(ChannelInventory {
            page: Some(page.page),
            channel: Channel::Visual,
            backend,
            sources: sources.clone(),
            complete: false,
        });
        store.issues.push(EvidenceIssue {
            boundary: None,
            page: Some(page.page),
            channel: Channel::Visual,
            sources,
            kind,
            reason,
        });
    }
}

fn render_page(
    bytes: &[u8],
    page: usize,
    pages: usize,
    width: u16,
    height: u16,
    reference: ObjectRef,
    timeout: Duration,
) -> Result<(Raster, bool, u64), (EvidenceFailure, String)> {
    let failure = |error: std::io::Error| {
        (
            EvidenceFailure::BackendFailure,
            format!("render process: {error}"),
        )
    };
    let mut command = Command::new(std::env::current_exe().map_err(failure)?);
    command.args([
        "render-page",
        &page.to_string(),
        &pages.to_string(),
        &width.to_string(),
        &height.to_string(),
        &reference.object_number.to_string(),
        &reference.generation.to_string(),
    ]);
    let max_output = RESPONSE_HEADER + usize::from(width) * usize::from(height) * 3;
    let output = run_bounded(&mut command, bytes, max_output, timeout)?;
    if output.len() != max_output || output[0] > 1 {
        return Err((
            EvidenceFailure::BackendFailure,
            "invalid render response".into(),
        ));
    }
    let cpu_micros = u64::from_le_bytes(
        output[1..RESPONSE_HEADER]
            .try_into()
            .expect("render response header has eight CPU bytes"),
    );
    Ok((
        Raster {
            width: u32::from(width),
            height: u32::from(height),
            rgb: output[RESPONSE_HEADER..].to_vec(),
        },
        output[0] != 0,
        cpu_micros,
    ))
}

/// Drains bounded output concurrently with input, then kills and reaps on timeout.
pub fn run_bounded(
    command: &mut Command,
    bytes: &[u8],
    max_output: usize,
    timeout: Duration,
) -> Result<Vec<u8>, (EvidenceFailure, String)> {
    let failure = |error: std::io::Error| {
        (
            EvidenceFailure::BackendFailure,
            format!("evidence process: {error}"),
        )
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failure)?;
    let stdin = child
        .stdin
        .take()
        .expect("render process was spawned with piped stdin");
    let stdout = child
        .stdout
        .take()
        .expect("render process was spawned with piped stdout");
    thread::scope(|scope| {
        let writer = scope.spawn(move || {
            let mut stdin = stdin;
            stdin.write_all(bytes)
        });
        let reader = scope.spawn(move || {
            let mut output = Vec::new();
            stdout
                .take((max_output + 1) as u64)
                .read_to_end(&mut output)
                .map(|_| output)
        });
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(10)),
                Ok(None) => {
                    break Err((
                        EvidenceFailure::ResourceLimit,
                        "page wall-clock safety stop expired; this outcome depends on machine load"
                            .into(),
                    ));
                }
                Err(error) => break Err(failure(error)),
            }
        };
        if status.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        let written = writer.join().map_err(|_| {
            (
                EvidenceFailure::BackendFailure,
                "render input thread failed".into(),
            )
        })?;
        let output = reader.join().map_err(|_| {
            (
                EvidenceFailure::BackendFailure,
                "render output thread failed".into(),
            )
        })?;
        let output = output.map_err(failure)?;
        // A blocking oversized writer would otherwise be killed at the parent
        // deadline and misreported as a deadline exhaustion.
        if output.len() > max_output {
            return Err((
                EvidenceFailure::BackendFailure,
                "oversized evidence response".into(),
            ));
        }
        let status = status?;
        if !status.success() {
            if status.code().is_none() && cpu_limit_termination(status) {
                return Err((
                    EvidenceFailure::ResourceLimit,
                    "evidence process exceeded its CPU budget; no complete response was retained"
                        .into(),
                ));
            }
            let kind = match status.code() {
                Some(3) => EvidenceFailure::ResourceLimit,
                Some(4) => EvidenceFailure::Unsupported,
                _ => EvidenceFailure::BackendFailure,
            };
            return Err((
                kind,
                format!("evidence process failed ({status}); no complete response was retained"),
            ));
        }
        written.map_err(failure)?;
        Ok(output)
    })
}

/// Returns the calling process's consumed user and system CPU time in micros.
///
/// `CLOCK_PROCESS_CPUTIME_ID` is the safe POSIX process CPU clock; it measures
/// the same user+system work as `getrusage(RUSAGE_SELF)` without sampling
/// `/proc` and without an unsafe `wait4` binding. The render child reports this
/// value in its response so the parent can enforce the document CPU budget.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn process_cpu_micros() -> u64 {
    use rustix::time::{ClockId, clock_gettime};

    let timespec = clock_gettime(ClockId::ProcessCPUTime);
    let seconds = u64::try_from(timespec.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(timespec.tv_nsec).unwrap_or(0);
    seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanos / 1_000)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cpu_micros() -> u64 {
    0
}

/// The CPU budget configured by [`restrict_process`] terminates an exhausted
/// child with `SIGXCPU`, which is a typed resource-limit outcome rather than a
/// backend failure.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn cpu_limit_termination(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    const SIGXCPU: i32 = 24;
    status.signal() == Some(SIGXCPU)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn cpu_limit_termination(_status: std::process::ExitStatus) -> bool {
    false
}

/// Whether a page contains an image `XObject` above the render pixel bound.
///
/// The scan reads only image dictionaries; it never decodes a stream. Form
/// `XObject` resources are followed recursively with a depth and entry bound so
/// a self-referential or pathological resource tree cannot make the preflight
/// itself unbounded.
fn page_image_exceeds_render_bound(page: &hayro::hayro_syntax::page::Page<'_>) -> bool {
    if page.page_stream().is_some_and(content_exceeds_render_bound) {
        return true;
    }
    let mut remaining = MAX_PREFLIGHT_XOBJECTS;
    resources_exceed_render_bound(page.resources(), 0, &mut remaining)
}

/// Whether one decoded content stream declares an oversized inline image.
fn content_exceeds_render_bound(content: &[u8]) -> bool {
    pdfdelta_core::pdf::max_inline_image_pixels(content) > MAX_RENDER_IMAGE_PIXELS
}

fn resources_exceed_render_bound(
    resources: &Resources<'_>,
    depth: usize,
    remaining: &mut usize,
) -> bool {
    if depth > MAX_PREFLIGHT_DEPTH || *remaining == 0 {
        return false;
    }
    for (name, _) in resources.x_objects.entries() {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        let Some(stream) = resources.x_objects.get::<Stream<'_>>(name) else {
            continue;
        };
        let dict = stream.dict();
        let width = dict.get::<u32>(b"Width" as &[u8]).unwrap_or(0);
        let height = dict.get::<u32>(b"Height" as &[u8]).unwrap_or(0);
        if u64::from(width).saturating_mul(u64::from(height)) > MAX_RENDER_IMAGE_PIXELS {
            return true;
        }
        let is_form = dict
            .get::<Name<'_>>(b"Subtype" as &[u8])
            .is_some_and(|subtype| subtype.as_ref() == b"Form");
        if is_form
            && let Ok(decoded) = stream.decoded()
            && content_exceeds_render_bound(decoded.as_ref())
        {
            return true;
        }
        if let Some(form_resources) = dict.get::<Dict<'_>>(b"Resources" as &[u8]) {
            let nested = Resources::from_parent(form_resources, resources.clone());
            if resources_exceed_render_bound(&nested, depth + 1, remaining) {
                return true;
            }
        }
    }
    resources
        .parent()
        .is_some_and(|parent| resources_exceed_render_bound(parent, depth + 1, remaining))
}

/// Binary response: one warning flag followed by exactly width × height RGB bytes.
/// Exit codes distinguish rejected input (2), limits (3), and unsupported (4).
pub fn worker(
    page: usize,
    pages: usize,
    width: u16,
    height: u16,
    object_number: u32,
    generation: u16,
) -> Result<(), u8> {
    restrict_process(PAGE_CPU_BUDGET_SECONDS)?;
    if width == 0 || height == 0 || usize::from(width) * usize::from(height) > MAX_PIXELS {
        return Err(3);
    }
    let input_limit = ParseLimits::default().max_input_bytes;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(input_limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| 2)?;
    if bytes.len() > input_limit {
        return Err(3);
    }
    let pdf = hayro::hayro_syntax::Pdf::new(bytes).map_err(|_| 2)?;
    if pdf.pages().len() != pages {
        return Err(2);
    }
    let page = pdf.pages().get(page).ok_or(2)?;
    let reference = page.raw().obj_id().ok_or(2)?;
    if u32::try_from(reference.obj_number).ok() != Some(object_number)
        || u16::try_from(reference.gen_number).ok() != Some(generation)
    {
        return Err(2);
    }
    let (actual_width, actual_height) = page.render_dimensions();
    if actual_width.ceil() != f32::from(width) || actual_height.ceil() != f32::from(height) {
        return Err(2);
    }
    if page_image_exceeds_render_bound(page) {
        return Err(3);
    }
    let warned = Arc::new(AtomicBool::new(false));
    let sink = Arc::clone(&warned);
    let settings = hayro::hayro_interpret::InterpreterSettings {
        warning_sink: Arc::new(move |_| {
            sink.store(true, Ordering::Relaxed);
        }),
        ..Default::default()
    };
    let pixmap = hayro::render(
        page,
        &hayro::RenderCache::new(),
        &settings,
        &hayro::RenderSettings {
            width: Some(width),
            height: Some(height),
            bg_color: hayro::vello_cpu::color::palette::css::WHITE,
            ..Default::default()
        },
    );
    let cpu_micros = process_cpu_micros();
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    stdout
        .write_all(&[u8::from(warned.load(Ordering::Relaxed))])
        .map_err(|_| 2)?;
    stdout.write_all(&cpu_micros.to_le_bytes()).map_err(|_| 2)?;
    for pixel in pixmap.data_as_u8_slice().as_chunks::<4>().0 {
        stdout.write_all(&pixel[..3]).map_err(|_| 2)?;
    }
    stdout.flush().map_err(|_| 2)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn restrict_process(cpu_seconds: u64) -> Result<(), u8> {
    use rustix::process::{Resource, Rlimit, setrlimit};
    for (resource, limit) in [
        (Resource::As, 2 * 1024 * 1024 * 1024),
        (Resource::Cpu, cpu_seconds),
        (Resource::Core, 0),
    ] {
        setrlimit(
            resource,
            Rlimit {
                current: Some(limit),
                maximum: Some(limit),
            },
        )
        .map_err(|_| 4)?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn restrict_process(_cpu_seconds: u64) -> Result<(), u8> {
    Err(4)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    fn image_page_pdf(width: u32, height: u32) -> Vec<u8> {
        use lopdf::{Document, Object, Stream as LopdfStream, dictionary};

        let mut document = Document::with_version("1.5");
        let pages = document.new_object_id();
        let image = document.add_object(LopdfStream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => i64::from(width),
                "Height" => i64::from(height),
                "BitsPerComponent" => 1,
                "ColorSpace" => "DeviceGray",
                "ImageMask" => true,
            },
            vec![0_u8; 8],
        ));
        let contents = document.add_object(LopdfStream::new(
            dictionary! {},
            b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec(),
        ));
        let page = document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(100), Object::from(100)],
            "Resources" => dictionary! { "XObject" => dictionary! { "Im0" => image } },
            "Contents" => contents,
        });
        document.objects.insert(
            pages,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page)],
                "Count" => 1,
            }),
        );
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document
            .save_to(&mut bytes)
            .expect("image fixture serializes");
        bytes
    }

    #[test]
    fn oversized_image_x_object_is_rejected_before_render() {
        let bytes = image_page_pdf(19_848, 28_063);
        let pdf = hayro::hayro_syntax::Pdf::new(bytes).expect("fixture parses");
        let page = pdf.pages().first().expect("fixture page");
        assert!(page_image_exceeds_render_bound(page));
    }

    #[test]
    fn ordinary_image_x_object_passes_the_render_preflight() {
        let bytes = image_page_pdf(100, 100);
        let pdf = hayro::hayro_syntax::Pdf::new(bytes).expect("fixture parses");
        let page = pdf.pages().first().expect("fixture page");
        assert!(!page_image_exceeds_render_bound(page));
    }

    #[test]
    fn document_cpu_exhaustion_is_a_resource_limit_with_no_raster() {
        use pdfdelta_core::{
            document::PageEvidence,
            model::{Document, Glyph, PageId, Rect},
        };

        let mut store = EvidenceStore {
            revision: String::new(),
            backends: Vec::new(),
            pages: vec![PageEvidence {
                page: PageId(0),
                bounds: Some(Rect {
                    min: Vec2 { x: 0.0, y: 0.0 },
                    max: Vec2 { x: 100.0, y: 100.0 },
                }),
            }],
            native: Document::<Glyph>::new(Vec::new()),
            rendered: Vec::new(),
            structured: Vec::new(),
            inventories: Vec::new(),
            key_inventories: Vec::new(),
            native_structures: Vec::new(),
            issues: Vec::new(),
        };
        collect_with_limits(&mut store, &[], &[], false, 0, Duration::from_secs(1));
        assert!(store.rendered.is_empty(), "an exhausted page has no raster");
        assert_eq!(store.issues.len(), 1);
        assert_eq!(store.issues[0].kind, EvidenceFailure::ResourceLimit);
        assert_eq!(store.issues[0].reason, DOCUMENT_CPU_BUDGET_REASON);
    }

    #[test]
    fn document_cpu_budget_charges_pages_and_failed_pages() {
        let mut cpu = DocumentCpu::new(2 * PAGE_CPU_BUDGET_SECONDS * 1_000_000);
        assert!(!cpu.exhausted());
        cpu.charge(PAGE_CPU_BUDGET_SECONDS * 1_000_000 - 1);
        assert!(!cpu.exhausted());
        cpu.charge_failed_page();
        assert!(!cpu.exhausted());
        cpu.charge(1);
        assert!(cpu.exhausted());
        cpu.charge(1);
        assert!(cpu.exhausted());
    }

    #[test]
    fn document_cpu_budget_reason_is_explicit() {
        assert!(DOCUMENT_CPU_BUDGET_REASON.contains("document CPU budget"));
        assert!(DOCUMENT_CPU_BUDGET_REASON.contains("deterministic"));
    }

    #[test]
    fn worker_address_space_limit_rejects_excess_reservation() {
        const CHILD: &str = "PDFDELTA_LIMIT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            restrict_process(3).expect("install worker resource limits");
            let mut bytes = Vec::<u8>::new();
            assert!(bytes.try_reserve_exact(3 * 1024 * 1024 * 1024).is_err());
            println!("address-space-ceiling-verified");
            return;
        }
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "render::tests::worker_address_space_limit_rejects_excess_reservation",
                "--nocapture",
            ])
            .env(CHILD, "1");
        let output = run_bounded(&mut command, &[], 16 * 1024, Duration::from_secs(5))
            .expect("bounded child rejects an allocation above the process ceiling");
        assert!(String::from_utf8_lossy(&output).contains("address-space-ceiling-verified"));
    }

    #[test]
    fn worker_wall_safety_stop_is_distinct_from_the_cpu_budget() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        let error = run_bounded(
            &mut command,
            &vec![0; 512 * 1024],
            64,
            Duration::from_millis(50),
        )
        .expect_err("worker must not outlive its wall-clock safety stop");
        assert_eq!(error.0, EvidenceFailure::ResourceLimit);
        assert!(error.1.contains("wall-clock safety stop"), "{}", error.1);
        assert!(error.1.contains("machine load"), "{}", error.1);
        assert!(!error.1.contains("CPU budget"), "{}", error.1);
        // A missing kill would leave the scoped input writer waiting for sleep.
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn worker_output_limit_does_not_return_a_truncated_success() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 123456789"]);
        let error = run_bounded(&mut command, &[], 8, Duration::from_secs(5))
            .expect_err("the ninth byte exceeds the response budget");
        assert_eq!(error.0, EvidenceFailure::BackendFailure);

        let mut command = Command::new("cat");
        let output = run_bounded(&mut command, b"healthy", 8, Duration::from_secs(5))
            .expect("an independent bounded request remains usable");
        assert_eq!(output, b"healthy");
    }

    #[test]
    fn worker_cpu_exhaustion_reports_the_cpu_budget() {
        let mut command = Command::new("sh");
        // Only the soft limit is set so the child is terminated by SIGXCPU
        // instead of the shell's ignored-signal SIGKILL fallback.
        command.args(["-c", "ulimit -S -t 1; while :; do :; done"]);
        let error = run_bounded(&mut command, &[], 64, Duration::from_secs(5))
            .expect_err("a cpu-exhausted worker must not be reported as healthy");
        assert_eq!(error.0, EvidenceFailure::ResourceLimit);
        assert!(error.1.contains("CPU budget"), "{}", error.1);
        assert!(!error.1.contains("wall-clock safety"), "{}", error.1);
    }

    #[test]
    fn blocking_oversized_output_is_not_reported_as_a_deadline() {
        let mut command = Command::new("sh");
        command.args(["-c", "yes 1234567890"]);
        let started = Instant::now();
        let error = run_bounded(&mut command, &[], 8, Duration::from_secs(5))
            .expect_err("unbounded output must not be returned");
        assert_eq!(error.0, EvidenceFailure::BackendFailure);
        assert!(error.1.contains("oversized"), "{}", error.1);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
