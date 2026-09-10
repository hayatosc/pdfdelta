use lopdf::{Document, Object, dictionary};
use pdfdelta_core::{
    document::{FieldValue, FormLimits, StructuredValue, extract_form_evidence},
    pdf::{LopdfParser, ParseLimits, ParsedPdf, PdfParser},
};
use std::sync::Arc;

fn parse(build: impl FnOnce(&mut Document) -> Vec<Object>) -> Box<dyn ParsedPdf> {
    let mut pdf = Document::with_version("1.7");
    let pages = pdf.new_object_id();
    let page = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages,
    "MediaBox" => vec![Object::from(0), Object::from(0), Object::from(72), Object::from(72)] });
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! { "Type" => "Pages",
        "Kids" => vec![Object::Reference(page)], "Count" => 1 }),
    );
    let fields = build(&mut pdf);
    let root = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages,
    "AcroForm" => dictionary! { "Fields" => fields } });
    pdf.trailer.set("Root", root);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).expect("serialize form fixture");
    LopdfParser
        .parse(Arc::from(bytes), ParseLimits::default())
        .expect("parse form fixture")
}

#[test]
fn malformed_text_retains_raw_value_and_independent_fields() {
    let raw = vec![0xfe, 0xff, 0x65];
    let pdf = parse(|pdf| {
        [dictionary! { "FT" => "Tx", "T" => Object::string_literal("broken"), "V" => Object::String(raw.clone(), lopdf::StringFormat::Literal) },
         dictionary! { "FT" => "Tx", "T" => Object::string_literal("valid"), "V" => Object::string_literal("20") }]
            .into_iter().map(|field| Object::Reference(pdf.add_object(field))).collect()
    });
    let result =
        extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default()).expect("partial forms");
    assert!(!result.inventory.complete);
    assert_eq!(result.fields.len(), 2);
    assert!(
        matches!(&result.fields[0].value, StructuredValue::FormField {
        value: FieldValue::Unresolved { raw_bytes: Some(bytes), .. }, .. } if bytes == &raw)
    );
    assert!(
        matches!(&result.fields[1].value, StructuredValue::FormField {
        value: FieldValue::Text(value), .. } if value == "20")
    );
}

#[test]
fn button_value_and_widget_state_disagreements_remain_local() {
    for (saved, states, mismatch) in [
        ("Yes", vec!["Yes"], false),
        ("Yes", vec!["Off"], true),
        ("Off", vec!["Yes"], true),
        ("First", vec!["First", "Off"], false),
        ("First", vec!["Off", "Second"], true),
        ("First", vec!["First", "First"], false),
    ] {
        let pdf = parse(|pdf| {
            let kids: Vec<_> = states
                .iter()
                .map(|state| {
                    Object::Reference(pdf.add_object(dictionary! {
                        "Subtype" => "Widget", "AS" => Object::Name(state.as_bytes().to_vec())
                    }))
                })
                .collect();
            let button = pdf.add_object(
                dictionary! { "FT" => "Btn", "T" => Object::string_literal("choice"),
                "V" => Object::Name(saved.as_bytes().to_vec()), "Kids" => kids },
            );
            let other = pdf.add_object(dictionary! { "FT" => "Tx", "T" => Object::string_literal("independent"), "V" => Object::string_literal("100") });
            vec![Object::Reference(button), Object::Reference(other)]
        });
        let result = extract_form_evidence(pdf.as_ref(), 0, 7, FormLimits::default())
            .expect("button evidence");
        let disagreements: Vec<_> = result
            .issues
            .iter()
            .filter(|issue| issue.reason.contains("states disagree"))
            .collect();
        assert_eq!(!disagreements.is_empty(), mismatch, "{saved}: {states:?}");
        for issue in disagreements {
            assert_eq!(
                issue.sources,
                vec![pdfdelta_core::document::SourceRef::Structured { element: 7 }]
            );
        }
        assert!(
            matches!(&result.fields[0].value, StructuredValue::FormField { value: FieldValue::Name(value), .. } if value == saved.as_bytes())
        );
        assert!(
            matches!(&result.fields[1].value, StructuredValue::FormField { value: FieldValue::Text(value), .. } if value == "100")
        );
        assert!(!result.inventory.complete);
    }
}

#[test]
fn unknown_and_oversized_button_states_do_not_fabricate_disagreements() {
    for state in [
        None,
        Some(Object::Integer(1)),
        Some(Object::Name(vec![b'x'; 40])),
    ] {
        let pdf = parse(|pdf| {
            let mut button = dictionary! { "FT" => "Btn", "Subtype" => "Widget", "T" => Object::string_literal("choice"), "V" => "Yes" };
            if let Some(state) = state {
                button.set("AS", state);
            }
            vec![Object::Reference(pdf.add_object(button))]
        });
        let result = extract_form_evidence(
            pdf.as_ref(),
            0,
            0,
            FormLimits {
                max_text_bytes: 32,
                ..FormLimits::default()
            },
        )
        .expect("partial state evidence");
        assert!(
            result
                .issues
                .iter()
                .any(|issue| issue.reason.contains("appearance state"))
        );
        assert!(
            !result
                .issues
                .iter()
                .any(|issue| issue.reason.contains("states disagree"))
        );
        assert!(
            matches!(&result.fields[0].value, StructuredValue::FormField { value: FieldValue::Name(value), .. } if value == b"Yes")
        );
    }
}

#[test]
fn inherited_type_and_qualified_name_do_not_require_a_widget() {
    let pdf = parse(|pdf| {
        let child = pdf.add_object(dictionary! { "T" => Object::string_literal("revenue"), "V" => Object::string_literal("100") });
        let parent = pdf.add_object(dictionary! { "T" => Object::string_literal("annual"), "FT" => "Tx", "Kids" => vec![Object::Reference(child)] });
        vec![Object::Reference(parent)]
    });
    let result =
        extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default()).expect("inherited form");
    assert!(result.inventory.complete);
    assert!(result.key_inventory.complete);
    assert_eq!(result.fields.len(), 1);
    assert_eq!(result.fields[0].page, None);
    assert!(
        matches!(&result.fields[0].value, StructuredValue::FormField {
        name, value: FieldValue::Text(value), .. } if name == "annual.revenue" && value == "100")
    );
}

#[test]
fn ambiguous_partial_names_cannot_alias_a_qualified_field() {
    let pdf = parse(|pdf| {
        let child = pdf.add_object(dictionary! { "T" => Object::string_literal("revenue"), "V" => Object::string_literal("100") });
        let parent = pdf.add_object(dictionary! { "T" => Object::string_literal("annual"), "FT" => "Tx", "Kids" => vec![Object::Reference(child)] });
        let alias = pdf.add_object(dictionary! { "T" => Object::string_literal("annual.revenue"), "FT" => "Tx", "V" => Object::string_literal("20") });
        vec![Object::Reference(parent), Object::Reference(alias)]
    });
    let result = extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default())
        .expect("retain ambiguous field evidence");
    assert!(!result.inventory.complete);
    assert!(!result.key_inventory.complete);
    assert_eq!(result.fields.len(), 2);
    assert!(
        matches!(&result.fields[0].value, StructuredValue::FormField {
        name, .. } if name == "annual.revenue")
    );
    assert!(
        matches!(&result.fields[1].value, StructuredValue::FormField {
        name, value: FieldValue::Text(value), .. } if name.is_empty() && value == "20")
    );
    assert!(result.fields[1].object.is_some());
}

#[test]
fn cycles_and_limits_cannot_be_reported_as_a_complete_empty_inventory() {
    let pdf = parse(|pdf| {
        let cyclic = pdf.new_object_id();
        pdf.objects.insert(
            cyclic,
            Object::Dictionary(dictionary! { "T" => Object::string_literal("loop"),
            "Kids" => vec![Object::Reference(cyclic)] }),
        );
        vec![Object::Reference(cyclic)]
    });
    let result = extract_form_evidence(pdf.as_ref(), 0, 0, FormLimits::default())
        .expect("cycle retained as incomplete");
    assert!(!result.inventory.complete);
    assert!(!result.issues.is_empty());
    assert!(
        extract_form_evidence(
            pdf.as_ref(),
            0,
            0,
            FormLimits {
                max_nodes: 0,
                ..FormLimits::default()
            }
        )
        .is_err()
    );
}
