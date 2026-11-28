//! End-to-end exit and evidence tests for reordering and formatting fixtures.
//!
//! The fixtures are generated at test time from `pdfdelta-bench`, so no binary
//! PDFs are committed. The exit contract under test is: a deletion, insertion,
//! replacement, or move selects exit 1; a layout-only change does not.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use pdfdelta_bench::{
    formatting::formatting_cases,
    renderers::{RenderLimits, RendererKind},
    reorder::{ReorderExpectation, reorder_cases},
};
use serde_json::Value;

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-cli-reorder-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_flow(old: &Path, new: &Path, report: &Path) -> (Output, Option<Value>) {
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args([
            "flow",
            old.to_str().expect("UTF-8 path"),
            new.to_str().expect("UTF-8 path"),
            "--json",
            report.to_str().expect("UTF-8 path"),
            "--color",
            "never",
        ])
        .output()
        .expect("pdfdelta flow should run");
    let json = if report.is_file() {
        Some(
            serde_json::from_str(&fs::read_to_string(report).expect("report readable"))
                .expect("report is JSON"),
        )
    } else {
        None
    };
    (output, json)
}

fn content_changes(json: &Value) -> u64 {
    ["delete", "insert", "replace"]
        .iter()
        .map(|kind| json["stats"][kind]["changes"].as_u64().unwrap_or(0))
        .sum()
}

fn moves(json: &Value) -> u64 {
    json["stats"]["move"]["changes"].as_u64().unwrap_or(0)
}

#[test]
fn meaning_changing_reorders_select_exit_one() {
    let directory = TestDirectory::new();
    for case in reorder_cases().expect("fixtures constructible") {
        let expectation = case.expectation();
        if expectation == ReorderExpectation::NoContentChange {
            continue;
        }
        for renderer in RendererKind::all() {
            let old = directory.join(&format!("{}-{}-old.pdf", case.id(), renderer.name()));
            let new = directory.join(&format!("{}-{}-new.pdf", case.id(), renderer.name()));
            fs::write(
                &old,
                renderer
                    .render(case.old(), RenderLimits::default())
                    .expect("old PDF"),
            )
            .expect("write old");
            fs::write(
                &new,
                renderer
                    .render(case.new_plan(), RenderLimits::default())
                    .expect("new PDF"),
            )
            .expect("write new");
            let report = directory.join(&format!("{}-{}.json", case.id(), renderer.name()));
            let (output, json) = run_flow(&old, &new, &report);
            let json = json.expect("flow JSON report");
            let detected = content_changes(&json) + moves(&json);
            assert!(
                detected > 0,
                "{} renderer={}: no detected change",
                case.id(),
                renderer.name()
            );
            assert_eq!(
                output.status.code(),
                Some(1),
                "{} renderer={}: {}",
                case.id(),
                renderer.name(),
                String::from_utf8_lossy(&output.stderr)
            );
            for change in json["changes"].as_array().expect("changes array") {
                assert!(change.get("confidence").is_some(), "{change}");
                assert!(change.get("ambiguity").is_some(), "{change}");
            }
        }
    }
}

#[test]
fn no_content_controls_select_exit_zero_except_reordered_blocks() {
    let directory = TestDirectory::new();
    for case in reorder_cases().expect("fixtures constructible") {
        if case.expectation() != ReorderExpectation::NoContentChange {
            continue;
        }
        // A same-page whole-block reorder is reported as a detected change
        // rather than suppressed unless narrower row-band evidence applies.
        let expected_exit = u8::from(case.id() == "control-two-column-reorder");
        for renderer in RendererKind::all() {
            let old = directory.join(&format!("{}-{}-old.pdf", case.id(), renderer.name()));
            let new = directory.join(&format!("{}-{}-new.pdf", case.id(), renderer.name()));
            fs::write(
                &old,
                renderer
                    .render(case.old(), RenderLimits::default())
                    .expect("old PDF"),
            )
            .expect("write old");
            fs::write(
                &new,
                renderer
                    .render(case.new_plan(), RenderLimits::default())
                    .expect("new PDF"),
            )
            .expect("write new");
            let report = directory.join(&format!("{}-{}.json", case.id(), renderer.name()));
            let (output, json) = run_flow(&old, &new, &report);
            let json = json.expect("flow JSON report");
            assert_eq!(
                content_changes(&json),
                0,
                "{} renderer={}: layout control reported content",
                case.id(),
                renderer.name()
            );
            assert_eq!(
                output.status.code(),
                Some(i32::from(expected_exit)),
                "{} renderer={}: {}",
                case.id(),
                renderer.name(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn the_word_order_swap_is_flagged_as_a_short_move() {
    let directory = TestDirectory::new();
    let case = reorder_cases()
        .expect("fixtures constructible")
        .into_iter()
        .find(|case| case.id() == "word-order-swap")
        .expect("word-order fixture");
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    fs::write(
        &old,
        RendererKind::LopdfTj
            .render(case.old(), RenderLimits::default())
            .expect("old PDF"),
    )
    .expect("write old");
    fs::write(
        &new,
        RendererKind::LopdfTj
            .render(case.new_plan(), RenderLimits::default())
            .expect("new PDF"),
    )
    .expect("write new");
    let report = directory.join("report.json");
    let (output, json) = run_flow(&old, &new, &report);
    assert_eq!(output.status.code(), Some(1));
    let json = json.expect("flow JSON report");
    let change = &json["changes"][0];
    assert_eq!(change["kind"], "move");
    assert_eq!(json["stats"]["content_changes"], 0);
    assert_eq!(json["stats"]["moved_changes"], 1);
    assert_eq!(json["stats"]["detected_changes"], 1);
    assert_eq!(json["exit_basis"], "complete_detected");
    assert_eq!(json["difference_status"], "detected");
    assert_eq!(change["confidence"], "medium");
    assert!(
        change["ambiguity"]
            .as_array()
            .expect("ambiguity")
            .iter()
            .any(|reason| reason == "short_move")
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("short_move"), "{text}");
    assert!(text.contains("detected changes: 1"), "{text}");
}

#[test]
fn default_channels_detected_swap_overrides_unexamined_visual_evidence() {
    let directory = TestDirectory::new();
    let case = reorder_cases()
        .expect("fixtures constructible")
        .into_iter()
        .find(|case| case.id() == "word-order-swap")
        .expect("word-order fixture");
    let old = directory.join("old.pdf");
    let new = directory.join("new.pdf");
    fs::write(
        &old,
        RendererKind::LopdfTj
            .render(case.old(), RenderLimits::default())
            .expect("old PDF"),
    )
    .expect("write old");
    fs::write(
        &new,
        RendererKind::LopdfTj
            .render(case.new_plan(), RenderLimits::default())
            .expect("new PDF"),
    )
    .expect("write new");
    let report = directory.join("default.json");
    let output = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(&old)
        .arg(&new)
        .args(["-j", report.to_str().expect("UTF-8 path")])
        .output()
        .expect("default-channel comparison");
    let json: Value =
        serde_json::from_str(&fs::read_to_string(&report).expect("default report readable"))
            .expect("default report is JSON");
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["engine"], "flow");
    assert_eq!(json["comparison_complete"], false);
    assert_eq!(json["difference_status"], "detected");
    assert_eq!(json["exit_basis"], "detected_with_unexamined_evidence");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(
            "difference detected, but some evidence was not examined (non-text paint or visual evidence)"
        ),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        json["coverage"]
            .as_array()
            .expect("coverage")
            .iter()
            .any(|entry| entry["channel"] == "visual" && entry["complete"] == false)
    );
    // The text channel reports flow's own examination, and shared-evidence
    // text obligations are recorded as superseded instead of deciding the exit.
    let coverage = json["coverage"].as_array().expect("coverage");
    let text = coverage
        .iter()
        .find(|entry| entry["channel"] == "text")
        .expect("text coverage");
    assert_eq!(text["complete"], true);
    assert_eq!(text["old_compared_sources"], text["old_discovered_sources"]);
    assert_eq!(text["new_compared_sources"], text["new_discovered_sources"]);
    assert!(
        text["old_compared_sources"]
            .as_u64()
            .expect("flow token count")
            > 0
    );
    assert!(
        json["flow_owned_text"]["comparisons"]
            .as_u64()
            .expect("superseded comparison count")
            >= 1
    );
    assert!(
        json["flow_owned_text"]["components"]
            .as_u64()
            .expect("superseded component count")
            >= 1
    );
}

#[test]
fn font_size_formatting_is_reported_without_selecting_exit_one() {
    let directory = TestDirectory::new();
    for case in formatting_cases().expect("fixtures constructible") {
        let old = directory.join(&format!("{}-old.pdf", case.id()));
        let new = directory.join(&format!("{}-new.pdf", case.id()));
        fs::write(&old, case.old_pdf()).expect("write old");
        fs::write(&new, case.new_pdf()).expect("write new");
        let report = directory.join(&format!("{}-report.json", case.id()));
        let (output, json) = run_flow(&old, &new, &report);
        let json = json.expect("flow JSON report");
        assert_eq!(content_changes(&json), 0, "{}", case.id());
        assert_eq!(moves(&json), 0, "{}", case.id());
        let expected_exit = if case.id() == "formatting-underline" {
            // The added underline is non-text paint. The flow text engine
            // cannot rule out text drawn as paths, so it must report an
            // incomplete comparison rather than a complete no-change result.
            Some(3)
        } else {
            Some(0)
        };
        assert_eq!(
            output.status.code(),
            expected_exit,
            "{}: {}",
            case.id(),
            String::from_utf8_lossy(&output.stderr)
        );
        if case.id() == "formatting-underline" {
            assert!(
                json["old"]["issues"]
                    .as_array()
                    .expect("old issues")
                    .iter()
                    .chain(json["new"]["issues"].as_array().expect("new issues"))
                    .any(|issue| issue["description"]
                        .as_str()
                        .is_some_and(|description| description.contains("non-text paint differs")))
            );
        }
        let formatting = json["formatting_only"]
            .as_array()
            .expect("formatting array");
        if case.id() == "formatting-larger-size" {
            assert!(
                formatting.iter().any(|entry| {
                    entry["reasons"]
                        .as_array()
                        .is_some_and(|reasons| reasons.iter().any(|reason| reason == "font_size"))
                        && entry["old_size"] == 10.0
                        && entry["new_size"] == 14.0
                }),
                "{}: {formatting:?}",
                case.id()
            );
        } else {
            assert!(formatting.is_empty(), "{}: {formatting:?}", case.id());
        }
    }
}

fn run_main_flow(old: &Path, new: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .args([
            "--engine",
            "flow",
            "--native-text-only",
            old.to_str().expect("UTF-8 path"),
            new.to_str().expect("UTF-8 path"),
            "-q",
        ])
        .output()
        .expect("main command with --engine flow should run")
}

#[test]
fn reorder_fixtures_exit_the_same_through_the_main_command() {
    let directory = TestDirectory::new();
    for case in reorder_cases().expect("fixtures constructible") {
        let old = directory.join(&format!("{}-main-old.pdf", case.id()));
        let new = directory.join(&format!("{}-main-new.pdf", case.id()));
        fs::write(
            &old,
            RendererKind::LopdfTj
                .render(case.old(), RenderLimits::default())
                .expect("old PDF"),
        )
        .expect("write old");
        fs::write(
            &new,
            RendererKind::LopdfTj
                .render(case.new_plan(), RenderLimits::default())
                .expect("new PDF"),
        )
        .expect("write new");
        let expected = match case.expectation() {
            ReorderExpectation::NoContentChange => {
                i32::from(case.id() == "control-two-column-reorder")
            }
            ReorderExpectation::MeaningChanged | ReorderExpectation::LegitimateMove => 1,
        };
        let output = run_main_flow(&old, &new);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}: {}",
            case.id(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
