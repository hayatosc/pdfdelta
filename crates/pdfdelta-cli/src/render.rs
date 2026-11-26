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
/// Measured over 4,654 rendered pages of the 36-pair channel panel: median
/// about 0.032 s, p99 about 0.27 s, p99.9 about 1.35 s, and max about 2.9 s.
/// The maximum is two reproducible `ipcc-synthesis-ar5-to-ar6` pages at
/// 2.86-2.90 s; an earlier `/proc`-sampled measurement understated them. Ten
/// seconds is about a 3.5x margin over the maximum and 37x over p99.
pub(crate) const PAGE_CPU_BUDGET_SECONDS: u64 = 10;

/// Wall-clock safety stop for one page.
///
/// The deterministic budget is [`PAGE_CPU_BUDGET_SECONDS`]. This cap is 12
/// times that budget and only stops a page that is blocked or descheduled far
/// beyond the measured distribution. At a 25% CPU quota ten CPU seconds take
/// about 40 wall seconds, so the CPU budget still expires first. Its expiry is
/// reported as a distinct resource-limit reason because the outcome depends on
/// machine load.
const PAGE_WALL_SAFETY_SECONDS: u64 = 120;

/// Cumulative child CPU allowed to all rendered pages of one input document.
///
/// Measured per input over the 36-pair channel panel: max and p99 about 26.7 s
/// (with 72 documents the nearest-rank p99 is the maximum), and median about
/// 1.35 s. One hundred twenty seconds is about a 4.5x margin over the slowest
/// observed document and still bounds a hostile input that yields many cheap
/// pages. The
/// page that pushes the cumulative total over the budget is retained; the total
/// is therefore at most this budget plus one page budget.
pub(crate) const DOCUMENT_CPU_BUDGET_SECONDS: u64 = 120;
const DOCUMENT_CPU_BUDGET_REASON: &str =
    "document CPU budget exhausted; this outcome is deterministic for this input";

/// Wall-clock safety stop for one document's rendering loop.
///
/// Five times the document CPU budget. At a 25% CPU quota the full CPU budget
/// takes about 480 wall seconds, so the deterministic CPU budget still expires
/// before this cap. The CPU budget is the deterministic limit; this cap only
/// stops a page stream that is blocked or descheduled far beyond the measured
/// distribution. Its expiry is reported as a distinct resource-limit reason
/// that depends on machine load.
const DOCUMENT_WALL_SAFETY_SECONDS: u64 = 600;

/// Child CPU and wall-clock budgets for channel rendering.
///
/// [`Self::for_limit_scale`] multiplies every value by the command's
/// `--limit-scale`, so a slower machine can raise the deterministic CPU budgets
/// and their load-dependent wall backstops together. Integer conversion
/// saturates instead of wrapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RenderBudgets {
    pub(crate) page_cpu_seconds: u64,
    pub(crate) document_cpu_micros: u64,
    pub(crate) page_wall: Duration,
    pub(crate) document_wall: Duration,
}

impl RenderBudgets {
    /// Scales the measured defaults by a validated `--limit-scale` factor.
    ///
    /// The CLI rejects non-finite factors below one before this runs. Casting
    /// an out-of-range product through `f64` saturates at `u64::MAX`.
    pub(crate) fn for_limit_scale(limit_scale: f64) -> Self {
        let page_cpu_seconds = scale_seconds(PAGE_CPU_BUDGET_SECONDS, limit_scale);
        let document_cpu_seconds = scale_seconds(DOCUMENT_CPU_BUDGET_SECONDS, limit_scale);
        Self {
            page_cpu_seconds,
            document_cpu_micros: document_cpu_seconds.saturating_mul(1_000_000),
            page_wall: Duration::from_secs(scale_seconds(PAGE_WALL_SAFETY_SECONDS, limit_scale)),
            document_wall: Duration::from_secs(scale_seconds(
                DOCUMENT_WALL_SAFETY_SECONDS,
                limit_scale,
            )),
        }
    }
}

/// Multiplies a whole-second limit by `scale` without wrapping.
///
/// The caller validates that `scale` is finite and at least one. The cast
/// saturates at `u64::MAX` for products outside the representable range.
pub(crate) fn scale_seconds(seconds: u64, scale: f64) -> u64 {
    let scaled = seconds as f64 * scale;
    if scaled >= u64::MAX as f64 {
        u64::MAX
    } else if scaled <= 0.0 {
        0
    } else {
        scaled as u64
    }
}

/// Length of the complete render response: warning flag plus CPU micros.
const RESPONSE_HEADER: usize = 1 + 8;

/// Cumulative child CPU used by one input document's render pages.
///
/// Successful pages report their own process CPU. A failed page still consumed
/// up to [`RenderBudgets::page_cpu_seconds`], so it is charged that full
/// budget; this keeps a stream of failed pages from bypassing the document
/// budget.
#[derive(Clone, Copy, Debug)]
struct DocumentCpu {
    used_micros: u64,
    budget_micros: u64,
    page_budget_micros: u64,
}

impl DocumentCpu {
    const fn new(budget_micros: u64, page_budget_micros: u64) -> Self {
        Self {
            used_micros: 0,
            budget_micros,
            page_budget_micros,
        }
    }

    const fn exhausted(&self) -> bool {
        self.used_micros >= self.budget_micros
    }

    fn charge(&mut self, micros: u64) {
        self.used_micros = self.used_micros.saturating_add(micros);
    }

    fn charge_failed_page(&mut self) {
        self.charge(self.page_budget_micros);
    }
}

/// Child CPU consumed while rendering one input document.
///
/// `page_micros` retains one measurement per successfully rendered page. A
/// failed render attempt is charged the full page budget in `used_micros` and
/// counted in `failed_pages`, but is not added to `page_micros`: the parent
/// deliberately charges a budget rather than measuring a missing response.
#[derive(Clone, Debug, Default)]
pub(crate) struct RenderCpuSummary {
    pub(crate) pages: usize,
    pub(crate) rendered_pages: usize,
    pub(crate) failed_pages: usize,
    pub(crate) used_micros: u64,
    pub(crate) page_micros: Vec<u64>,
}

impl RenderCpuSummary {
    pub(crate) fn max_page_micros(&self) -> u64 {
        self.page_micros.iter().copied().max().unwrap_or(0)
    }
}

/// Render-CPU evidence for both sides of one comparison.
#[derive(Clone, Debug, Default)]
pub(crate) struct RenderCpuReport {
    pub(crate) old: RenderCpuSummary,
    pub(crate) new: RenderCpuSummary,
}

/// Retains partial page evidence without claiming renderer or annotation coverage.
///
/// `limit_scale` is the validated `--limit-scale` factor; it scales both the
/// deterministic CPU budgets and their wall-clock backstops.
pub fn collect(
    store: &mut EvidenceStore,
    bytes: &[u8],
    page_refs: &[PageRef],
    password_supplied: bool,
    limit_scale: f64,
) -> RenderCpuSummary {
    collect_with_budgets(
        store,
        bytes,
        page_refs,
        password_supplied,
        RenderBudgets::for_limit_scale(limit_scale),
    )
}

/// Collects rendered page evidence with explicit budgets.
fn collect_with_budgets(
    store: &mut EvidenceStore,
    bytes: &[u8],
    page_refs: &[PageRef],
    password_supplied: bool,
    budgets: RenderBudgets,
) -> RenderCpuSummary {
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
    let mut document_cpu = DocumentCpu::new(
        budgets.document_cpu_micros,
        budgets.page_cpu_seconds.saturating_mul(1_000_000),
    );
    let mut summary = RenderCpuSummary::default();
    for page in &store.pages {
        let mut attempted = false;
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
            let remaining = budgets
                .document_wall
                .checked_sub(started.elapsed())
                .ok_or_else(|| {
                    (
                        EvidenceFailure::ResourceLimit,
                        "document wall-clock safety stop expired; this outcome depends on machine load".into(),
                    )
                })?;
            attempted = true;
            let rendered = render_page(
                bytes,
                PageRender {
                    page: page.page.0 as usize,
                    pages: store.pages.len(),
                    reference: page_refs
                        .get(page.page.0 as usize)
                        .ok_or_else(|| {
                            (
                                EvidenceFailure::Unresolved,
                                "native page reference is unavailable".into(),
                            )
                        })?
                        .0,
                },
                width,
                height,
                remaining.min(budgets.page_wall),
                budgets,
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
            Ok((bounds, raster, warned, page_cpu_micros))
        })();
        let mut sources = Vec::new();
        let (kind, reason) = match result {
            Ok((bounds, raster, warned, page_cpu_micros)) => {
                summary.rendered_pages += 1;
                summary.page_micros.push(page_cpu_micros);
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
            Err(error) => {
                if attempted {
                    summary.failed_pages += 1;
                }
                error
            }
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
    summary.pages = store.pages.len();
    summary.used_micros = document_cpu.used_micros;
    summary
}

/// One page identity passed to the bounded render child.
#[derive(Clone, Copy, Debug)]
struct PageRender {
    page: usize,
    pages: usize,
    reference: ObjectRef,
}

fn render_page(
    bytes: &[u8],
    request: PageRender,
    width: u16,
    height: u16,
    timeout: Duration,
    budgets: RenderBudgets,
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
        &request.page.to_string(),
        &request.pages.to_string(),
        &width.to_string(),
        &height.to_string(),
        &request.reference.object_number.to_string(),
        &request.reference.generation.to_string(),
        "--cpu-seconds",
        &budgets.page_cpu_seconds.to_string(),
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
                        "worker wall-clock safety stop expired; this outcome depends on machine load"
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
/// `/proc` and without an unsafe `wait4` binding. The render and image workers
/// report this value in their responses so the parent can enforce their CPU
/// budgets and record the measured headroom.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn process_cpu_micros() -> u64 {
    use rustix::time::{ClockId, clock_gettime};

    let timespec = clock_gettime(ClockId::ProcessCPUTime);
    let seconds = u64::try_from(timespec.tv_sec).unwrap_or(0);
    let nanos = u64::try_from(timespec.tv_nsec).unwrap_or(0);
    seconds
        .saturating_mul(1_000_000)
        .saturating_add(nanos / 1_000)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_cpu_micros() -> u64 {
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
    cpu_seconds: u64,
) -> Result<(), u8> {
    restrict_process(cpu_seconds)?;
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
    // macOS accepts RLIMIT_AS but does not enforce it. Arm the worker
    // allocator before any input is read so the 2 GiB ceiling is real on both
    // supported platforms. Linux keeps its existing RLIMIT_AS behaviour.
    #[cfg(target_os = "macos")]
    crate::worker_allocator::enforce(crate::worker_allocator::WORKER_MEMORY_BUDGET_BYTES);
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
        collect_with_budgets(
            &mut store,
            &[],
            &[],
            false,
            RenderBudgets {
                page_cpu_seconds: PAGE_CPU_BUDGET_SECONDS,
                document_cpu_micros: 0,
                page_wall: Duration::from_secs(1),
                document_wall: Duration::from_secs(1),
            },
        );
        assert!(store.rendered.is_empty(), "an exhausted page has no raster");
        assert_eq!(store.issues.len(), 1);
        assert_eq!(store.issues[0].kind, EvidenceFailure::ResourceLimit);
        assert_eq!(store.issues[0].reason, DOCUMENT_CPU_BUDGET_REASON);
    }

    #[test]
    fn document_cpu_budget_charges_pages_and_failed_pages() {
        let page_micros = PAGE_CPU_BUDGET_SECONDS * 1_000_000;
        let mut cpu = DocumentCpu::new(2 * page_micros, page_micros);
        assert!(!cpu.exhausted());
        cpu.charge(page_micros - 1);
        assert!(!cpu.exhausted());
        cpu.charge_failed_page();
        assert!(!cpu.exhausted());
        cpu.charge(1);
        assert!(cpu.exhausted());
        cpu.charge(1);
        assert!(cpu.exhausted());
    }

    #[test]
    fn render_budgets_scale_with_the_limit_factor() {
        let base = RenderBudgets::for_limit_scale(1.0);
        assert_eq!(base.page_cpu_seconds, 10);
        assert_eq!(base.document_cpu_micros, 120_000_000);
        assert_eq!(base.page_wall, Duration::from_mins(2));
        assert_eq!(base.document_wall, Duration::from_mins(10));

        let scaled = RenderBudgets::for_limit_scale(2.5);
        assert_eq!(scaled.page_cpu_seconds, 25);
        assert_eq!(scaled.document_cpu_micros, 300_000_000);
        assert_eq!(scaled.page_wall, Duration::from_mins(5));
        assert_eq!(scaled.document_wall, Duration::from_mins(25));

        let saturated = RenderBudgets::for_limit_scale(f64::MAX);
        assert_eq!(saturated.page_cpu_seconds, u64::MAX);
        assert_eq!(saturated.document_cpu_micros, u64::MAX);
        assert_eq!(saturated.page_wall, Duration::from_secs(u64::MAX));
        assert_eq!(saturated.document_wall, Duration::from_secs(u64::MAX));
    }

    #[test]
    fn document_cpu_budget_reason_is_explicit() {
        assert!(DOCUMENT_CPU_BUDGET_REASON.contains("document CPU budget"));
        assert!(DOCUMENT_CPU_BUDGET_REASON.contains("deterministic"));
    }

    #[test]
    fn worker_memory_limit_terminates_or_rejects_an_excess_reservation() {
        const CHILD: &str = "PDFDELTA_LIMIT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            restrict_process(3).expect("install worker resource limits");
            let mut bytes = Vec::<u8>::new();
            // Linux enforces RLIMIT_AS here and returns the allocation error.
            // macOS does not enforce RLIMIT_AS, so the worker allocator
            // terminates the process with the limits exit code before this
            // reservation can return. In both cases the parent must see the
            // same typed resource-limit outcome.
            assert!(bytes.try_reserve_exact(3 * 1024 * 1024 * 1024).is_err());
            std::process::exit(3);
        }
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "render::tests::worker_memory_limit_terminates_or_rejects_an_excess_reservation",
                "--nocapture",
            ])
            .env(CHILD, "1");
        let error = run_bounded(&mut command, &[], 16 * 1024, Duration::from_secs(5))
            .expect_err("the worker memory ceiling must terminate an excess reservation");
        assert_eq!(error.0, EvidenceFailure::ResourceLimit);
        assert!(error.1.contains("exit status: 3"), "{}", error.1);
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
