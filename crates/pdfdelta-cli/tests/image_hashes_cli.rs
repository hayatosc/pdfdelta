#![cfg(any(target_os = "linux", target_os = "macos"))]

use lopdf::{Document, Object, Stream, dictionary};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdfdelta-images-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("directory");
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn pdf(
    path: &Path,
    colors: &[[u8; 3]],
    compressed: bool,
    nested: bool,
    text: &str,
    unsupported: bool,
) {
    let mut pdf = Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let font = pdf.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );
    let mut xobjects = lopdf::Dictionary::new();
    let mut content = format!("BT /F1 12 Tf 10 180 Td ({text}) Tj ET\n");
    for (i, color) in colors.iter().enumerate() {
        let mut stream = Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8, "BitsPerComponent" => 8, "ColorSpace" => "DeviceRGB" },
            color.repeat(64),
        );
        if compressed {
            stream.compress().expect("compress pixels");
        }
        if unsupported {
            stream.dict.set("Filter", "UnsupportedImageFilter");
        }
        let image = pdf.add_object(stream);
        xobjects.set(format!("I{i}"), image);
        content.push_str(&format!("q 30 0 0 30 {} 30 cm /I{i} Do Q\n", 10 + i * 40));
    }
    let resources = dictionary! { "XObject" => xobjects, "Font" => dictionary! { "F1" => font } };
    let (resources, content) = if nested {
        let form = pdf.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 200.into(), Object::from(200)], "Resources" => resources }, content.into_bytes()));
        (
            dictionary! { "XObject" => dictionary! { "Form" => form } },
            b"/Form Do".to_vec(),
        )
    } else {
        (resources, content.into_bytes())
    };
    let contents = pdf.add_object(Stream::new(dictionary! {}, content));
    let page = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), Object::from(200)], "Resources" => resources, "Contents" => contents });
    pdf.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }));
    let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    pdf.trailer.set("Root", catalog);
    pdf.save(path).expect("PDF fixture");
}

fn compare(dir: &Path, old: &Path, new: &Path, channels: &str) -> (Value, String) {
    let report = dir.join(format!(
        "report-{}.json",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = Command::new(env!("CARGO_BIN_EXE_pdfdelta"))
        .arg(old)
        .arg(new)
        .args(["--channels", channels, "--json"])
        .arg(&report)
        .output()
        .expect("CLI");
    assert!(
        matches!(result.status.code(), Some(0 | 1 | 3)),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    (
        serde_json::from_slice(&fs::read(report).expect("report")).expect("JSON"),
        String::from_utf8(result.stdout).expect("text"),
    )
}

#[test]
fn decoded_hashes_ignore_compression_and_survive_form_xobjects() {
    let dir = Directory::new();
    let (old, new) = (dir.0.join("old.pdf"), dir.0.join("new.pdf"));
    pdf(
        &old,
        &[[255, 0, 0]],
        false,
        false,
        "The total is 100.",
        false,
    );
    pdf(&new, &[[255, 0, 0]], true, true, "The total is 200.", false);
    let (report, text) = compare(&dir.0, &old, &new, "text,visual");
    assert_eq!(
        report["image_diff"]["comparison"]["unchanged"], 1,
        "{report}"
    );
    assert_eq!(
        report["image_diff"]["comparison"]["changes"],
        serde_json::json!([])
    );
    assert_eq!(
        report["image_diff"]["old"]["images"][0]["sha256"],
        report["image_diff"]["new"]["images"][0]["sha256"]
    );
    assert!(text.contains("1 unchanged, 0 changes"));
    assert!(!report.to_string().contains("page_rendering_changed"));
    let (default_channels, _) = compare(&dir.0, &old, &new, "text,visual,forms,relations");
    assert!(
        !default_channels
            .to_string()
            .contains("page_rendering_changed")
    );
    assert_eq!(default_channels["image_diff"]["comparison"]["unchanged"], 1);
    assert!(
        report["inferred_changes"].as_u64().expect("changes")
            + report["typed_changes"].as_u64().expect("changes")
            > 0,
        "native text change survives: {report}"
    );
}

#[test]
fn changed_added_removed_images_are_reported_without_pixel_masks() {
    let dir = Directory::new();
    let (old, new) = (dir.0.join("old.pdf"), dir.0.join("new.pdf"));
    pdf(&old, &[[255, 0, 0]], false, false, "Stable text.", false);
    pdf(
        &new,
        &[[0, 0, 255], [0, 255, 0]],
        true,
        false,
        "Stable text.",
        false,
    );
    let (report, text) = compare(&dir.0, &old, &new, "visual");
    let changes = &report["image_diff"]["comparison"]["changes"];
    assert_eq!(changes[0]["kind"], "changed", "{report}");
    assert_eq!(changes[1]["kind"], "added", "{report}");
    assert!(text.contains("Image Changed: page 1 image 1 -> page 1 image 1"));
    assert!(text.contains("Image Added: absent -> page 1 image 2"));
    assert!(!report.to_string().contains("page_rendering_changed"));
    let (reverse, text) = compare(&dir.0, &new, &old, "visual");
    assert_eq!(
        reverse["image_diff"]["comparison"]["changes"][1]["kind"],
        "removed"
    );
    assert!(text.contains("Image Removed"));
    let (text_only, _) = compare(&dir.0, &old, &new, "text");
    assert!(text_only.get("image_diff").is_none());
}

#[test]
fn unsupported_image_decoding_keeps_the_comparison_unresolved() {
    let dir = Directory::new();
    let (old, new) = (dir.0.join("old.pdf"), dir.0.join("new.pdf"));
    pdf(&old, &[[255, 0, 0]], false, false, "Stable text.", true);
    pdf(&new, &[[0, 0, 255]], false, false, "Stable text.", false);
    let (report, _) = compare(&dir.0, &old, &new, "visual");
    assert_eq!(report["image_diff"]["comparison"]["complete"], false);
    assert_eq!(
        report["image_diff"]["comparison"]["changes"],
        serde_json::json!([])
    );
    assert_eq!(report["comparison_complete"], false);
}

#[test]
fn alpha_changes_are_hashed_and_oversized_images_do_not_erase_native_changes() {
    let dir = Directory::new();
    let (old, new) = (dir.0.join("old.pdf"), dir.0.join("new.pdf"));
    pdf(
        &old,
        &[[255, 0, 0]],
        false,
        false,
        "The total is 100.",
        false,
    );
    let mut changed = Document::load(&old).expect("fixture");
    let image = changed
        .objects
        .iter()
        .find_map(|(id, object)| {
            object
                .as_stream()
                .ok()
                .filter(|stream| {
                    stream
                        .dict
                        .get(b"Subtype")
                        .ok()
                        .and_then(|v| v.as_name().ok())
                        == Some(b"Image")
                })
                .map(|_| *id)
        })
        .expect("image");
    let mask = changed.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 8, "Height" => 8, "BitsPerComponent" => 8, "ColorSpace" => "DeviceGray" }, vec![128; 64]));
    changed
        .get_object_mut(image)
        .expect("image")
        .as_stream_mut()
        .expect("stream")
        .dict
        .set("SMask", mask);
    changed.save(&new).expect("alpha fixture");
    let (report, _) = compare(&dir.0, &old, &new, "visual");
    assert_eq!(
        report["image_diff"]["comparison"]["changes"][0]["kind"], "changed",
        "{report}"
    );

    pdf(
        &new,
        &[[255, 0, 0]],
        false,
        false,
        "The total is 200.",
        false,
    );
    let mut oversized = Document::load(&new).expect("fixture");
    for object in oversized.objects.values_mut() {
        if let Ok(stream) = object.as_stream_mut()
            && stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|v| v.as_name().ok())
                == Some(b"Image")
        {
            stream.dict.set("Width", 10_000);
            stream.dict.set("Height", 10_000);
        }
    }
    oversized.save(&new).expect("oversized fixture");
    let (report, _) = compare(&dir.0, &old, &new, "text,visual");
    assert_eq!(report["image_diff"]["comparison"]["complete"], false);
    assert_eq!(
        report["image_diff"]["comparison"]["changes"],
        serde_json::json!([])
    );
    assert!(
        report["inferred_changes"].as_u64().expect("changes")
            + report["typed_changes"].as_u64().expect("changes")
            > 0
    );
}
