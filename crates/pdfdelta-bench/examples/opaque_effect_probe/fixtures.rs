//! Independently varied generated dependencies. No pixels are used as proof.

use super::{acquire, digest, profile};

fn integers(values: &[i32]) -> Object {
    Object::Array(values.iter().copied().map(Object::from).collect())
}
use lopdf::{Document, Object, Stream, dictionary};
use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
use serde_json::{Value, json};
use std::sync::Arc;

fn fixture(mutation: &str) -> Vec<u8> {
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let image = pdf.add_object(Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Image", "Width" => 1,
            "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
        },
        match mutation {
            "image_bytes" => vec![0, 0, 255],
            "missing_image_samples" => vec![255, 0],
            _ => vec![255, 0, 0],
        },
    ));
    let mut form = Stream::new(
        dictionary! {
            "Type" => "XObject", "Subtype" => "Form", "BBox" => integers(&[0, 0, 64, 64]),
            "Resources" => dictionary! { "XObject" => dictionary! { "Im1" => image } },
        },
        b"q 24 0 0 24 8 8 cm /Im1 Do Q".to_vec(),
    );
    if mutation == "form_transform" {
        form.dict.set("Matrix", integers(&[1, 0, 0, 1, 2, 0]));
    }
    if mutation == "form_clip" {
        form.dict.set("BBox", integers(&[0, 0, 16, 16]));
    }
    if mutation == "visibility" {
        form.dict.set(
            "OC",
            dictionary! { "Type" => "OCG", "Name" => Object::string_literal("hidden") },
        );
    }
    if mutation == "nested_resource" {
        let nested = pdf.add_object(Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form", "BBox" => integers(&[0, 0, 1, 1]),
                "Resources" => dictionary! { "XObject" => dictionary! { "Im2" => image } },
            },
            b"/Im2 Do".to_vec(),
        ));
        form.dict.set(
            "Resources",
            dictionary! { "XObject" => dictionary! { "Im1" => nested } },
        );
    }
    let form = pdf.add_object(form);
    if mutation == "resource_cycle" {
        let stream = pdf
            .objects
            .get_mut(&form)
            .expect("generated Form exists")
            .as_stream_mut()
            .expect("generated Form is a stream");
        stream.dict.set(
            "Resources",
            dictionary! { "XObject" => dictionary! { "Im1" => form } },
        );
    }
    if matches!(mutation, "unused_nested_cycle" | "nested_default_space") {
        let stream = pdf
            .objects
            .get_mut(&form)
            .expect("Form")
            .as_stream_mut()
            .expect("stream");
        let mut resources =
            dictionary! { "XObject" => dictionary! { "Im1" => image, "Unused" => form } };
        if mutation == "nested_default_space" {
            resources.set("ColorSpace", dictionary! { "DefaultGray" => "DeviceRGB" });
        }
        stream.dict.set("Resources", resources);
    }
    let state = pdf.add_object(dictionary! {
        "Type" => "ExtGState", "ca" => if mutation == "opacity" { 0.5_f32 } else { 1.0_f32 },
        "CA" => 1, "BM" => if mutation == "blend" { "Multiply" } else { "Normal" },
        "SMask" => if mutation == "mask" { Object::Dictionary(dictionary! { "S" => "Alpha", "G" => form }) } else { Object::Name(b"None".to_vec()) },
    });
    let mut content = match mutation {
        "caller_transform" => "q 4 4 40 40 re W n /Gs gs 1 0 0 1 4 0 cm /Fm1 Do Q",
        "caller_clip" => "q 4 4 12 12 re W n /Gs gs 1 0 0 1 0 0 cm /Fm1 Do Q",
        "missing_resource" => "q 4 4 40 40 re W n /Gs gs 1 0 0 1 0 0 cm /Absent Do Q",
        "unbalanced_state" => "q 4 4 40 40 re W n /Gs gs 1 0 0 1 0 0 cm /Fm1 Do",
        _ => "q 4 4 40 40 re W n /Gs gs 1 0 0 1 0 0 cm /Fm1 Do Q",
    }
    .to_owned();
    if mutation == "background" {
        content.insert_str(0, "0.5 g 0 0 64 64 re f ");
    }
    if mutation == "decimal_a" {
        content.insert_str(0, "0.100000001 g ");
    }
    if mutation == "decimal_b" {
        content.insert_str(0, "0.100000002 g ");
    }
    if mutation == "overlap" {
        content.push_str(" 0 g 8 8 24 24 re f");
    }
    if mutation == "text_state" {
        content.insert_str(0, "BT ");
        content.push_str(" ET");
    }
    let content = pdf.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let mut resources = dictionary! {
        "XObject" => dictionary! { "Fm1" => form },
        "ExtGState" => dictionary! { "Gs" => state },
    };
    match mutation {
        "unused_font" => resources.set(
            "Font",
            dictionary! { "Unused" => Object::Reference((999, 0)) },
        ),
        "unused_image" => resources.set(
            "XObject",
            dictionary! { "Fm1" => form, "Unused" => Object::Reference((999, 0)) },
        ),
        "unused_state" => resources.set(
            "ExtGState",
            dictionary! { "Gs" => state, "Unused" => Object::Reference((999, 0)) },
        ),
        "default_space" => {
            resources.set("ColorSpace", dictionary! { "DefaultRGB" => "DeviceGray" })
        }
        _ => {}
    }
    let page = pdf.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => integers(&[0, 0, if mutation == "footprint" { 65 } else { 64 }, 64]),
        "Resources" => resources,
        "Contents" => content,
    });
    pdf.objects.insert(
        pages,
        dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }.into(),
    );
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages };
    if mutation == "visibility" {
        catalog.set(
            "OCProperties",
            dictionary! { "D" => dictionary! { "BaseState" => "OFF" } },
        );
    }
    let catalog = pdf.add_object(catalog);
    pdf.trailer.set("Root", catalog);
    if mutation == "metadata_only" {
        let info = pdf.add_object(
            dictionary! { "Title" => Object::string_literal("same paint, changed metadata") },
        );
        pdf.trailer.set("Info", info);
    }
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes)
        .expect("bounded generated PDF serialization");
    assert!(bytes.len() < 16384, "generated fixture byte bound");
    bytes
}

fn page(mutation: &str) -> profile::Page {
    let bytes = fixture(mutation);
    let hash = digest(&bytes);
    let pdf = LopdfParser
        .parse(Arc::from(bytes), ParseLimits::default())
        .expect("valid generated PDF objects");
    acquire::capture(pdf.as_ref(), &hash)
        .pages
        .pop()
        .expect("generated single page")
}

pub fn experiment() -> Value {
    let old = page("baseline");
    let mut cases = Vec::new();
    for mutation in [
        "metadata_only",
        "unused_font",
        "unused_image",
        "unused_state",
        "unused_nested_cycle",
        "default_space",
        "nested_default_space",
        "image_bytes",
        "caller_transform",
        "caller_clip",
        "opacity",
        "background",
        "visibility",
        "form_transform",
        "form_clip",
        "nested_resource",
        "overlap",
        "missing_resource",
        "missing_image_samples",
        "mask",
        "blend",
        "footprint",
        "resource_cycle",
        "text_state",
        "unbalanced_state",
    ] {
        let new = page(mutation);
        let comparison = profile::compare(&old, &new);
        let passed = if matches!(
            mutation,
            "metadata_only"
                | "unused_font"
                | "unused_image"
                | "unused_state"
                | "unused_nested_cycle"
        ) {
            matches!(comparison, profile::Comparison::Equivalent { .. })
        } else {
            !matches!(comparison, profile::Comparison::Equivalent { .. })
        };
        cases.push(
            json!({ "mutation": mutation, "old_input_sha256": old.input_sha256,
            "new": new, "comparison": comparison, "passed": passed }),
        );
    }
    for mutation in [
        "entry_state",
        "backdrop",
        "profile_identity",
        "backend_identity",
        "missing_closure",
    ] {
        let mut new = old.clone();
        if mutation == "missing_closure" {
            new.closure = None;
        } else {
            let closure = new.closure.as_mut().expect("baseline closure is complete");
            match mutation {
                "entry_state" => closure.entry.graphics_state = "unknown inherited state".into(),
                "backdrop" => closure.entry.backdrop_rgb = [0.0; 3],
                "profile_identity" => closure.profile = "different-profile".into(),
                "backend_identity" => closure.backend = "different-backend".into(),
                _ => unreachable!(),
            }
        }
        let comparison = profile::compare(&old, &new);
        cases.push(json!({ "mutation": mutation, "injected_dependency_control": true,
            "comparison": comparison, "passed": !matches!(comparison, profile::Comparison::Equivalent { .. }) }));
    }
    json!({ "version": 1, "profile": profile::PROFILE,
        "interpretation": "PDF mutations are parsed from source bytes. Entry/profile controls inject one declared environment dependency; they are not real-world recall or source acquisitions.",
        "baseline": old, "cases": cases })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_positive_and_each_independent_dependency_mutation() {
        let result = experiment();
        assert!(
            result["baseline"]["issues"]
                .as_array()
                .expect("issues array")
                .is_empty(),
            "{result}"
        );
        for case in result["cases"].as_array().expect("case array") {
            assert_eq!(case["passed"], true, "{case}");
        }
        let positive = &result["cases"][0];
        assert_ne!(
            positive["old_input_sha256"],
            positive["new"]["input_sha256"]
        );
    }

    #[test]
    fn unchanged_commands_cannot_hide_changed_image_bytes() {
        let old = page("baseline");
        let new = page("image_bytes");
        let (a, b) = (
            old.closure.as_ref().expect("old closure"),
            new.closure.as_ref().expect("new closure"),
        );
        assert_eq!(a.commands, b.commands);
        assert_ne!(a.resources, b.resources);
        assert!(matches!(
            profile::compare(&old, &new),
            profile::Comparison::Rejected { .. }
        ));
    }

    #[test]
    fn rounded_operands_cannot_replace_exact_command_bytes() {
        let old = page("decimal_a");
        let new = page("decimal_b");
        let (a, b) = (
            old.closure.as_ref().expect("old closure"),
            new.closure.as_ref().expect("new closure"),
        );
        assert_eq!(
            a.commands, b.commands,
            "the pinned syntax adapter rounds these decimals alike"
        );
        assert_ne!(a.command_bytes, b.command_bytes);
        assert!(matches!(
            profile::compare(&old, &new),
            profile::Comparison::Rejected { .. }
        ));
    }
}
