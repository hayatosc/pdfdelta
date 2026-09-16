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

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, Channel, ChannelInventory, EvidenceFailure, EvidenceIssue,
        EvidenceLimits, EvidenceStore, Raster, RenderedEvidence, SourceRef,
    },
    model::Vec2,
    pdf::{ObjectRef, PageRef, ParseLimits},
};

const MAX_PIXELS: usize = 8_000_000;
const PAGE_TIMEOUT: Duration = Duration::from_secs(5);
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Retains partial page evidence without claiming renderer or annotation coverage.
pub fn collect(
    store: &mut EvidenceStore,
    bytes: &[u8],
    page_refs: &[PageRef],
    password_supplied: bool,
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
            let remaining = DOCUMENT_TIMEOUT
                .checked_sub(started.elapsed())
                .ok_or_else(|| {
                    (
                        EvidenceFailure::ResourceLimit,
                        "document rendering deadline exceeded".into(),
                    )
                })?;
            let (raster, warned) = render_page(
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
                remaining.min(PAGE_TIMEOUT),
            )?;
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
) -> Result<(Raster, bool), (EvidenceFailure, String)> {
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
    let max_output = 1 + usize::from(width) * usize::from(height) * 3;
    let output = run_bounded(&mut command, bytes, max_output, timeout)?;
    if output.len() != max_output || output[0] > 1 {
        return Err((
            EvidenceFailure::BackendFailure,
            "invalid render response".into(),
        ));
    }
    Ok((
        Raster {
            width: u32::from(width),
            height: u32::from(height),
            rgb: output[1..].to_vec(),
        },
        output[0] != 0,
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
                        "evidence process deadline exceeded".into(),
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
            let kind = match status.code() {
                Some(3) => EvidenceFailure::ResourceLimit,
                Some(4) => EvidenceFailure::Unsupported,
                None if cpu_limit_termination(status) => EvidenceFailure::ResourceLimit,
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

/// The CPU budget configured by [`restrict_process`] terminates an exhausted
/// child with `SIGXCPU`, which is a typed resource-limit outcome rather than a
/// backend failure.
#[cfg(target_os = "linux")]
fn cpu_limit_termination(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;

    const SIGXCPU: i32 = 24;
    status.signal() == Some(SIGXCPU)
}

#[cfg(not(target_os = "linux"))]
fn cpu_limit_termination(_status: std::process::ExitStatus) -> bool {
    false
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
    restrict_process(5)?;
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
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    stdout
        .write_all(&[u8::from(warned.load(Ordering::Relaxed))])
        .map_err(|_| 2)?;
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
    fn worker_deadline_releases_blocked_input() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let started = Instant::now();
        let error = run_bounded(
            &mut command,
            &vec![0; 512 * 1024],
            64,
            Duration::from_millis(50),
        )
        .expect_err("worker must not outlive its request deadline");
        assert_eq!(error.0, EvidenceFailure::ResourceLimit);
        assert!(error.1.contains("deadline"));
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
    fn worker_cpu_exhaustion_is_a_resource_limit() {
        let mut command = Command::new("sh");
        // Only the soft limit is set so the child is terminated by SIGXCPU
        // instead of the shell's ignored-signal SIGKILL fallback.
        command.args(["-c", "ulimit -S -t 1; while :; do :; done"]);
        let error = run_bounded(&mut command, &[], 64, Duration::from_secs(5))
            .expect_err("a cpu-exhausted worker must not be reported as healthy");
        assert_eq!(error.0, EvidenceFailure::ResourceLimit);
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
