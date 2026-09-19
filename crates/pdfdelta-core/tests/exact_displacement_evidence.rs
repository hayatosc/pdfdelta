//! Raw glyph displacement evidence through the real parser and normalization.

use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, Stream, dictionary};
use pdfdelta_core::{
    Result,
    diff::{ChangeKind, ComparisonAssumption},
    layout::{reconstruct_blocks, reconstruct_lines},
    model::{Document, Glyph},
    normalize::{normalize_blocks, token_displacements},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::{PipelineOptions, compare_glyph_documents},
    source::ExtractionOutcome,
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};

fn fixture_font(document: &mut LopdfDocument) -> lopdf::ObjectId {
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Helvetica",
            "Ascent" => 800,
            "Descent" => -200,
            "MissingWidth" => 500,
        },
    })
}

fn fixture_pdf(content: &str) -> LopdfDocument {
    let mut document = LopdfDocument::with_version("1.7");
    let font = fixture_font(&mut document);
    let stream = Stream::new(dictionary! {}, content.as_bytes().to_vec());
    let contents = document.add_object(stream);
    let resources = dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font) } };
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => Object::Reference(contents),
        "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
    });
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page)],
            "Count" => 1,
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);
    document
}

fn extract(content: &str) -> Result<Document<Glyph>> {
    let mut document = fixture_pdf(content);
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract(pdf.as_ref(), ExtractionLimits::default())
}

fn runs(document: &Document<Glyph>) -> Vec<u32> {
    let mut ids = document
        .displacements()
        .iter()
        .map(|entry| entry.run)
        .collect::<Vec<_>>();
    ids.dedup();
    ids
}

#[test]
fn tj_split_and_same_size_tf_keep_one_run() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (ab) Tj /F1 12 Tf (cd) Tj ET")?;
    assert_eq!(document.displacements().len(), 4);
    assert_eq!(
        runs(&document).len(),
        1,
        "Tj splits and a same-size Tf keep the run"
    );
    for (index, entry) in document.displacements().iter().enumerate() {
        assert_eq!(entry.glyph.0, index as u64);
        assert_eq!(entry.page.0, 0);
        assert_eq!(entry.width_1000_em, 500.0);
        assert_eq!(entry.font_size, 12.0);
        assert!(entry.horizontal);
        assert_eq!(entry.raw_code, vec![b'a' + index as u8]);
        assert!(entry.page_transform.iter().all(|value| value.is_finite()));
    }
    Ok(())
}

#[test]
fn size_change_and_position_resets_break_the_run() -> Result<()> {
    let size = extract("BT /F1 12 Tf 10 20 Td (ab) Tj /F1 13 Tf (cd) Tj ET")?;
    assert_eq!(runs(&size).len(), 2, "a different Tf size breaks the run");
    let moved = extract("BT /F1 12 Tf 10 20 Td (ab) Tj 10 10 Td (cd) Tj ET")?;
    assert_eq!(runs(&moved).len(), 2, "a Td reset breaks the run");
    let matrix = extract("BT /F1 12 Tf 1 0 0 1 10 20 Tm (ab) Tj 1 0 0 1 10 10 Tm (cd) Tj ET")?;
    assert_eq!(runs(&matrix).len(), 2, "a Tm reset breaks the run");
    Ok(())
}

#[test]
fn graphics_state_and_ctm_changes_break_the_run() -> Result<()> {
    let q_q = extract("q BT /F1 12 Tf 10 20 Td (ab) Tj ET Q BT /F1 12 Tf 10 20 Td (cd) Tj ET")?;
    assert_eq!(runs(&q_q).len(), 2, "a Q/BT boundary breaks the run");
    let cm = extract("BT /F1 12 Tf 10 20 Td (ab) Tj 1 0 0 1 5 0 cm (cd) Tj ET")?;
    assert_eq!(runs(&cm).len(), 2, "a cm change breaks the run");
    Ok(())
}

#[test]
fn document_new_has_no_displacements() {
    let document = Document::<Glyph>::new(Vec::new());
    assert!(document.displacements().is_empty());
}

#[test]
fn token_displacements_map_only_unique_glyphs() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (abcd) Tj ET")?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    let normalized = normalize_blocks(&document, &lines, &blocks)?;
    let mapped = token_displacements(&document, &normalized, &mut 64)?;
    let evidence = mapped
        .iter()
        .flat_map(|block| block.tokens.iter())
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(
        evidence.len(),
        4,
        "every unique-glyph token carries evidence"
    );

    let filtered = document
        .displacements()
        .iter()
        .filter(|entry| entry.glyph.0 != 1)
        .cloned()
        .collect::<Vec<_>>();
    let document = document.with_displacements(filtered);
    let mapped = token_displacements(&document, &normalized, &mut 64)?;
    let missing = mapped
        .iter()
        .flat_map(|block| block.tokens.iter())
        .filter(|entry| entry.is_none())
        .count();
    assert_eq!(
        missing, 1,
        "a missing record leaves exactly one token without evidence"
    );
    Ok(())
}

#[test]
fn duplicate_sidecar_records_hold_the_mapping() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (ab) Tj ET")?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    let normalized = normalize_blocks(&document, &lines, &blocks)?;
    let mut duplicated = document.displacements().to_vec();
    let first = duplicated[0].clone();
    duplicated.push(first);
    let document = document.with_displacements(duplicated);
    let mapped = token_displacements(&document, &normalized, &mut 64)?;
    let evidence = mapped
        .iter()
        .flat_map(|block| block.tokens.iter())
        .flatten()
        .count();
    assert_eq!(evidence, 1, "only the non-duplicated glyph keeps evidence");
    Ok(())
}

#[test]
fn mapping_respects_the_record_limit() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (abcd) Tj ET")?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    let normalized = normalize_blocks(&document, &lines, &blocks)?;
    assert!(
        token_displacements(&document, &normalized, &mut 3).is_err(),
        "the work limit must fail the mapping"
    );
    Ok(())
}

#[test]
fn tj_adjustments_split_the_run_only_when_nonzero() -> Result<()> {
    let zero = extract("BT /F1 12 Tf 10 20 Td [(ab) 0] TJ (cd) Tj ET")?;
    assert_eq!(runs(&zero).len(), 1, "a zero TJ adjustment keeps the run");
    let nonzero = extract("BT /F1 12 Tf 10 20 Td [(ab) -50] TJ (cd) Tj ET")?;
    assert_eq!(
        runs(&nonzero).len(),
        2,
        "a nonzero TJ adjustment breaks the run"
    );
    Ok(())
}

#[test]
fn form_entry_and_exit_are_separate_runs() -> Result<()> {
    // A Form that shows text without BT must not share the caller's run, and
    // the caller must not resume the Form's run after the invocation.
    let outcome = form_outcome(
        "/Fm1 Do /Fm1 Do BT /F1 12 Tf 12 20 Td (cd) Tj ET",
        "BT /F1 12 Tf 12 20 Td (ab) Tj ET",
        false,
    )?;
    let document = outcome.document();
    let ids = runs(document);
    assert!(
        ids.len() >= 3,
        "entry, exit and repetition separate the runs: {ids:?}"
    );
    let caller = document
        .displacements()
        .iter()
        .find(|entry| entry.raw_code == b"c")
        .expect("caller glyph");
    let form = document
        .displacements()
        .iter()
        .find(|entry| entry.raw_code == b"b")
        .expect("form glyph");
    assert_ne!(caller.run, form.run);
    Ok(())
}

#[test]
fn nested_forms_are_separate_runs() -> Result<()> {
    let outcome = form_outcome(
        "/Fm1 Do BT /F1 12 Tf 12 20 Td (cd) Tj ET",
        "BT /F1 12 Tf 12 20 Td (ab) Tj ET",
        true,
    )?;
    let document = outcome.document();
    let ids = runs(document);
    assert!(
        ids.len() >= 2,
        "nested form content separates the runs: {ids:?}"
    );
    Ok(())
}

#[test]
fn failed_form_rolls_back_its_displacements() -> Result<()> {
    let outcome = form_outcome(
        "/Fm1 Do BT /F1 12 Tf 12 20 Td (cd) Tj ET",
        "BT /F1 12 Tf 12 20 Td (xy) Tj ET Q",
        false,
    )?;
    let document = outcome.document();
    assert!(
        !outcome.issues().is_empty(),
        "the failed invocation is recorded"
    );
    let codes = document
        .displacements()
        .iter()
        .map(|entry| entry.raw_code.clone())
        .collect::<Vec<_>>();
    assert!(
        codes.iter().all(|code| code != b"x" && code != b"y"),
        "a failed invocation leaves no sidecar record: {codes:?}"
    );
    Ok(())
}

#[test]
fn failed_page_rolls_back_its_displacements() -> Result<()> {
    let mut document = LopdfDocument::with_version("1.7");
    let font = fixture_font(&mut document);
    let good = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf 10 20 Td (ab) Tj ET".to_vec(),
    ));
    let bad = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 12 Tf (cd) Tj /Missing 1 Tf ET".to_vec(),
    ));
    let resources = dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font) } };
    let pages = document.new_object_id();
    let page_ids = [good, bad]
        .into_iter()
        .map(|contents| {
            document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages,
                "Contents" => Object::Reference(contents),
                "Resources" => resources.clone(),
                "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
            })
        })
        .collect::<Vec<_>>();
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => 2,
        }),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    let outcome =
        ContentStreamGlyphExtractor.extract_outcome(pdf.as_ref(), ExtractionLimits::default())?;
    let document = outcome.document();
    assert_eq!(
        document.displacements().len(),
        2,
        "only the good page keeps evidence"
    );
    assert!(
        document
            .displacements()
            .iter()
            .all(|entry| entry.page.0 == 0)
    );
    assert!(
        !outcome.issues().is_empty(),
        "the failed page is recorded as an issue"
    );
    Ok(())
}

#[test]
fn glyph_limit_fails_the_extraction() -> Result<()> {
    let mut document = fixture_pdf("BT /F1 12 Tf 10 20 Td (abcd) Tj ET");
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    let limits = ExtractionLimits {
        max_glyphs: 1,
        ..ExtractionLimits::default()
    };
    assert!(
        ContentStreamGlyphExtractor
            .extract(pdf.as_ref(), limits)
            .is_err()
    );
    Ok(())
}

#[test]
fn state_changes_break_the_run_and_keep_the_raw_values() -> Result<()> {
    for operator in ["1 Tc", "2 Tw", "50 Tz", "3 Ts"] {
        let content = format!("BT /F1 12 Tf 10 20 Td (ab) Tj {operator} (cd) Tj ET");
        let document = extract(&content)?;
        assert_eq!(runs(&document).len(), 2, "{operator} must break the run");
    }
    let document = extract("BT /F1 12 Tf 10 20 Td (ab) Tj 50 Tz (cd) Tj ET")?;
    let percent = document
        .displacements()
        .last()
        .map(|entry| entry.horizontal_scale_percent);
    assert_eq!(percent, Some(50.0), "the raw Tz percent is retained");
    Ok(())
}

#[test]
fn q_and_q_break_the_run_within_one_bt() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (ab) Tj q (cd) Tj Q ET")?;
    assert_eq!(
        runs(&document).len(),
        2,
        "q and Q break the run inside one BT"
    );
    Ok(())
}

fn form_outcome(page_content: &str, form_content: &str, nested: bool) -> Result<ExtractionOutcome> {
    let mut document = LopdfDocument::with_version("1.7");
    let font = fixture_font(&mut document);
    let form_fonts = dictionary! { "F1" => Object::Reference(font) };
    let inner = if nested {
        let inner_stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
                "Resources" => dictionary! { "Font" => form_fonts.clone() },
            },
            form_content.as_bytes().to_vec(),
        );
        let inner = document.add_object(inner_stream);
        let wrapper = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
                "Resources" => dictionary! {
                    "Font" => form_fonts.clone(),
                    "XObject" => dictionary! { "Fm2" => Object::Reference(inner) },
                },
            },
            b"/Fm2 Do".to_vec(),
        );
        document.add_object(wrapper)
    } else {
        let stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
                "Resources" => dictionary! { "Font" => form_fonts.clone() },
            },
            form_content.as_bytes().to_vec(),
        );
        document.add_object(stream)
    };
    let contents = document.add_object(Stream::new(
        dictionary! {},
        page_content.as_bytes().to_vec(),
    ));
    let resources = dictionary! {
        "Font" => form_fonts,
        "XObject" => dictionary! { "Fm1" => Object::Reference(inner) },
    };
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => Object::Reference(contents),
        "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
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
    document.save_to(&mut bytes).expect("serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract_outcome(pdf.as_ref(), ExtractionLimits::default())
}

#[test]
fn filtered_displacements_follow_items_and_break_on_dropped_glyphs() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (abcd) Tj ET")?;
    let mut reordered = document.displacements().to_vec();
    reordered.reverse();
    let document = document.with_displacements(reordered);
    let filtered = document.filtered_displacements(&[true, false, true, true]);
    let codes = filtered
        .iter()
        .map(|entry| entry.raw_code.clone())
        .collect::<Vec<_>>();
    assert_eq!(codes, vec![b"a".to_vec(), b"c".to_vec(), b"d".to_vec()]);
    let runs = filtered.iter().map(|entry| entry.run).collect::<Vec<_>>();
    assert_ne!(runs[0], runs[1], "a dropped glyph breaks the run");
    assert_eq!(runs[1], runs[2], "adjacent kept glyphs keep the run");
    Ok(())
}

#[test]
fn filtered_displacements_reject_disagreeing_records() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (ab) Tj ET")?;
    let mut tampered = document.displacements().to_vec();
    tampered[0].raw_code = b"z".to_vec();
    let document = document.with_displacements(tampered);
    let filtered = document.filtered_displacements(&[true, true]);
    assert_eq!(filtered.len(), 1, "the disagreeing record is dropped");
    assert_eq!(filtered[0].raw_code, b"b".to_vec());
    Ok(())
}

#[test]
fn mapping_fails_before_cloning_sources_on_a_tiny_budget() -> Result<()> {
    let document = extract("BT /F1 12 Tf 10 20 Td (abcd) Tj ET")?;
    let options = PipelineOptions::default();
    let lines = reconstruct_lines(&document, options.line)?;
    let blocks = reconstruct_blocks(&document, &lines, options.block)?;
    let normalized = normalize_blocks(&document, &lines, &blocks)?;
    let mut budget = 1;
    assert!(
        token_displacements(&document, &normalized, &mut budget).is_err(),
        "the source upper bound is charged before any token source is cloned"
    );
    assert_eq!(budget, 0);
    Ok(())
}

const SE_OLD: &str = "Schedule SE (Form 1040) 2024 ";
const SE_NEW: &str = "Schedule SE (Form 1040) 2025 Created 5/7/25 ";
const SE_OLD_WIDTHS: [f64; 29] = [
    649.0, 574.0, 593.0, 574.0, 611.0, 593.0, 141.0, 574.0, 278.0, 649.0, 647.0, 278.0, 296.0,
    574.0, 611.0, 391.0, 901.0, 278.0, 556.0, 556.0, 556.0, 556.0, 296.0, 278.0, 556.0, 556.0,
    556.0, 556.0, 278.0,
];
const SE_NEW_WIDTHS: [f64; 44] = [
    649.0, 574.0, 593.0, 574.0, 611.0, 593.0, 141.0, 574.0, 278.0, 649.0, 647.0, 278.0, 296.0,
    574.0, 611.0, 391.0, 901.0, 278.0, 556.0, 556.0, 556.0, 556.0, 296.0, 278.0, 556.0, 556.0,
    556.0, 556.0, 278.0, 722.0, 333.0, 537.0, 537.0, 315.0, 537.0, 593.0, 278.0, 556.0, 333.0,
    556.0, 333.0, 556.0, 556.0, 278.0,
];

fn se_widths() -> std::collections::HashMap<char, f64> {
    let mut widths = std::collections::HashMap::new();
    for (character, width) in SE_OLD.chars().zip(SE_OLD_WIDTHS) {
        widths.entry(character).or_insert(width);
    }
    for (character, width) in SE_NEW.chars().zip(SE_NEW_WIDTHS) {
        widths.entry(character).or_insert(width);
    }
    widths
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

fn se_font(
    document: &mut LopdfDocument,
    widths: &std::collections::HashMap<char, f64>,
) -> lopdf::ObjectId {
    let mut table = vec![Object::Integer(500); 256];
    for (character, width) in widths {
        table[*character as usize] = Object::Integer(*width as i64);
    }
    document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => table,
        "FontDescriptor" => dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "Helvetica",
            "Ascent" => 800,
            "Descent" => -200,
            "MissingWidth" => 500,
        },
    })
}

fn se_pair() -> Result<(Document<Glyph>, Document<Glyph>)> {
    let widths = se_widths();
    let old_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 200.003 Tm ({}) Tj ET",
        escape(SE_OLD)
    );
    let new_prefix = "Schedule SE (Form 1040) 2025 ";
    let new_text = format!(
        "BT /T1 1 Tf 7 0 0 7 426.244 206.003 Tm ({}) Tj /T2 1 Tf (Created 5/7/25) Tj /T1 1 Tf ( ) Tj ET",
        escape(new_prefix)
    );
    let mut old_document = LopdfDocument::with_version("1.7");
    let old_font = se_font(&mut old_document, &widths);
    let old_contents = old_document.add_object(Stream::new(dictionary! {}, old_text.into_bytes()));
    install_se_page(&mut old_document, old_contents, old_font);
    let mut new_document = LopdfDocument::with_version("1.7");
    let new_font = se_font(&mut new_document, &widths);
    let new_font2 = se_font(&mut new_document, &widths);
    let new_contents = new_document.add_object(Stream::new(dictionary! {}, new_text.into_bytes()));
    install_se_page2(&mut new_document, new_contents, new_font, new_font2);
    Ok((
        extract_document(old_document)?,
        extract_document(new_document)?,
    ))
}

fn install_se_page(document: &mut LopdfDocument, contents: lopdf::ObjectId, font: lopdf::ObjectId) {
    install_se_page2(document, contents, font, font);
}

fn install_se_page2(
    document: &mut LopdfDocument,
    contents: lopdf::ObjectId,
    font: lopdf::ObjectId,
    font2: lopdf::ObjectId,
) {
    let resources = dictionary! {
        "Font" => dictionary! {
            "T1" => Object::Reference(font),
            "T2" => Object::Reference(font2),
        },
    };
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => Object::Reference(contents),
        "Resources" => resources,
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
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
}

fn extract_document(mut document: LopdfDocument) -> Result<Document<Glyph>> {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract(pdf.as_ref(), ExtractionLimits::default())
}

#[test]
fn se_type_line_resolves_through_the_pipeline() -> Result<()> {
    let (old, new) = se_pair()?;
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    assert_eq!(comparison.changes[0].kind, ChangeKind::Replacement);
    let occurrences = &comparison.changes[0].occurrences;
    assert_eq!(
        occurrences.len(),
        2,
        "the year and the insertion: {occurrences:#?}"
    );
    let replacement = &occurrences[0];
    let old_span = replacement.old_span.as_ref().expect("old span");
    let new_span = replacement.new_span.as_ref().expect("new span");
    assert_eq!(
        old_span.comparable_range,
        pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
    );
    assert_eq!(
        new_span.comparable_range,
        pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
    );
    assert_eq!(&SE_OLD[27..28], "4");
    assert_eq!(&SE_NEW[27..28], "5");
    let insertion = &occurrences[1];
    let inserted = insertion.new_span.as_ref().expect("inserted span");
    assert_eq!(
        insertion
            .old_span
            .as_ref()
            .expect("empty old span")
            .comparable_range,
        pdfdelta_core::diff::TokenRange { start: 29, end: 29 }
    );
    assert_eq!(
        inserted.comparable_range,
        pdfdelta_core::diff::TokenRange { start: 29, end: 44 }
    );
    assert_eq!(&SE_NEW[29..44], "Created 5/7/25 ");
    assert!(comparison.change_candidates.is_empty(), "{comparison:#?}");
    assert!(comparison.unresolved_regions.is_empty(), "{comparison:#?}");
    let assessment = comparison.assessment.as_ref().expect("assessment");
    let proven = assessment
        .relations
        .iter()
        .filter(|relation| {
            relation.old_span.as_ref().is_some_and(|span| {
                span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 29 }
            }) && relation.new_span.as_ref().is_some_and(|span| {
                span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 44 }
            })
        })
        .any(|relation| {
            relation
                .assumptions
                .contains(&ComparisonAssumption::ExactTextDisplacement)
        });
    assert!(
        proven,
        "the exact displacement proof must be recorded: {:#?}",
        assessment.relations
    );
    // The dependent replacement relation inherits the proof along its parent
    // chain, not through a coincidentally equal span.
    let subspan = assessment
        .relations
        .iter()
        .find(|relation| {
            relation.old_span.as_ref().is_some_and(|span| {
                span.comparable_range == pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
            }) && relation.new_span.as_ref().is_some_and(|span| {
                span.comparable_range == pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
            })
        })
        .expect("the dependent replacement relation");
    assert!(
        subspan
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement),
        "the dependent relation inherits the proof: {subspan:#?}"
    );
    let mut parent = subspan.parent;
    let mut reached = false;
    while let Some(index) = parent {
        let relation = &assessment.relations[index];
        if relation
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement)
            && relation.old_span.as_ref().is_some_and(|span| {
                span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 29 }
            })
        {
            reached = true;
            break;
        }
        parent = relation.parent;
    }
    assert!(reached, "the parent chain must reach the proven domain");
    assert_eq!(
        pdfdelta_core::report::assumption(ComparisonAssumption::ExactTextDisplacement),
        "exact_text_displacement"
    );
    let summary = pdfdelta_core::report::summarize(
        &comparison,
        &pdfdelta_core::report::ExtractionStatus::default(),
    )?;
    assert!(summary.comparison_complete, "{summary:#?}");
    assert_eq!(summary.unresolved_regions, 0);
    assert_eq!(summary.tentative_candidates, 0);
    assert_eq!(summary.comparison_coverage, Some(1.0));
    Ok(())
}

#[test]
fn se_type_line_without_metadata_stays_ambiguous() -> Result<()> {
    let (old, new) = se_pair()?;
    let old = old.map_items(|glyph| glyph);
    let new = new.map_items(|glyph| glyph);
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert!(
        !comparison.unresolved_regions.is_empty(),
        "without the sidecar the line stays unresolved: {comparison:#?}"
    );
    if let Some(assessment) = comparison.assessment.as_ref() {
        assert!(
            assessment.relations.iter().all(|relation| !relation
                .assumptions
                .contains(&ComparisonAssumption::ExactTextDisplacement)),
            "no exact displacement assumption without metadata"
        );
    }
    Ok(())
}

#[test]
fn se_type_line_swaps_symmetrically() -> Result<()> {
    let (old, new) = se_pair()?;
    let comparison = compare_glyph_documents(&new, &old, PipelineOptions::default())?;
    assert_eq!(comparison.changes.len(), 1, "{comparison:#?}");
    let occurrences = &comparison.changes[0].occurrences;
    assert_eq!(occurrences.len(), 2);
    let replacement = &occurrences[0];
    assert_eq!(
        replacement
            .old_span
            .as_ref()
            .expect("old span")
            .comparable_range,
        pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
    );
    assert_eq!(
        replacement
            .new_span
            .as_ref()
            .expect("new span")
            .comparable_range,
        pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
    );
    let deletion = &occurrences[1];
    assert_eq!(
        deletion
            .old_span
            .as_ref()
            .expect("deleted span")
            .comparable_range,
        pdfdelta_core::diff::TokenRange { start: 29, end: 44 }
    );
    assert_eq!(
        deletion
            .new_span
            .as_ref()
            .expect("empty new span")
            .comparable_range,
        pdfdelta_core::diff::TokenRange { start: 29, end: 29 }
    );
    assert!(comparison.unresolved_regions.is_empty(), "{comparison:#?}");
    Ok(())
}

#[test]
fn se_type_line_with_changed_ctm_stays_ambiguous() -> Result<()> {
    let (old, new) = se_pair()?;
    let mut records = new.displacements().to_vec();
    records[0].ctm = [2.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let new = new.with_displacements(records);
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert!(
        !comparison.unresolved_regions.is_empty(),
        "a changed CTM must hold the line: {comparison:#?}"
    );
    Ok(())
}

#[test]
fn se_type_line_with_a_tiny_budget_stays_ambiguous() -> Result<()> {
    let (old, new) = se_pair()?;
    let options = PipelineOptions {
        diff: pdfdelta_core::diff::DiffOptions {
            max_assessment_work: 2_000,
            ..pdfdelta_core::diff::DiffOptions::default()
        },
        ..PipelineOptions::default()
    };
    let comparison = compare_glyph_documents(&old, &new, options)?;
    assert!(
        !comparison.unresolved_regions.is_empty(),
        "a tiny budget must hold the line: {comparison:#?}"
    );
    Ok(())
}

fn one_line_document(
    text: &str,
    widths: &std::collections::HashMap<char, f64>,
    x: f64,
) -> Result<Document<Glyph>> {
    let mut document = LopdfDocument::with_version("1.7");
    let font = se_font(&mut document, widths);
    let contents = format!(
        "BT /T1 1 Tf 7 0 0 7 {x} 206.003 Tm ({}) Tj ET",
        escape(text)
    );
    let contents = document.add_object(Stream::new(dictionary! {}, contents.into_bytes()));
    install_se_page(&mut document, contents, font);
    extract_document(document)
}

#[test]
fn se_type_line_with_a_split_run_stays_ambiguous() -> Result<()> {
    let (old, new) = se_pair()?;
    let mut records = new.displacements().to_vec();
    records[30].run = 1;
    let new = new.with_displacements(records);
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert!(
        !comparison.unresolved_regions.is_empty(),
        "a split run must hold the line: {comparison:#?}"
    );
    if let Some(assessment) = comparison.assessment.as_ref() {
        assert!(assessment.relations.iter().all(|relation| {
            !relation
                .assumptions
                .contains(&ComparisonAssumption::ExactTextDisplacement)
        }));
    }
    Ok(())
}

#[test]
fn se_type_line_with_a_zero_advance_duplicate_stays_ambiguous() -> Result<()> {
    // A zero-advance duplicate keeps both maximum matchings at the same
    // displacement, so the exact rule must not claim a unique edit.
    let widths = std::collections::HashMap::from([('a', 600.0), ('b', 600.0), ('x', 0.0)]);
    let old = one_line_document("axb", &widths, 475.383)?;
    let new = one_line_document("axxb", &widths, 475.383)?;
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    let assessment = comparison.assessment.as_ref().expect("assessment");
    assert!(
        assessment.relations.iter().all(|relation| !relation
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement)),
        "a zero-advance duplicate cannot prove a unique edit"
    );
    Ok(())
}

#[test]
fn se_type_line_with_partial_metadata_stays_ambiguous() -> Result<()> {
    let (old, new) = se_pair()?;
    let mut records = new.displacements().to_vec();
    records.remove(35);
    let new = new.with_displacements(records);
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert!(
        !comparison.unresolved_regions.is_empty(),
        "a missing record must hold the line: {comparison:#?}"
    );
    if let Some(assessment) = comparison.assessment.as_ref() {
        assert!(assessment.relations.iter().all(|relation| {
            !relation
                .assumptions
                .contains(&ComparisonAssumption::ExactTextDisplacement)
        }));
    }
    Ok(())
}

fn two_line_pair() -> Result<(Document<Glyph>, Document<Glyph>)> {
    let widths = se_widths();
    let top_old = "Form 1040 2024";
    let top_new = "Form 1040 2025";
    let old_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 475.383 200.003 Tm ({}) Tj ET",
        escape(top_old),
        escape(SE_OLD)
    );
    let new_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 426.244 206.003 Tm ({}) Tj ET",
        escape(top_new),
        escape(SE_NEW)
    );
    let mut old_document = LopdfDocument::with_version("1.7");
    let old_font = se_font(&mut old_document, &widths);
    let old_contents = old_document.add_object(Stream::new(dictionary! {}, old_text.into_bytes()));
    install_se_page(&mut old_document, old_contents, old_font);
    let mut new_document = LopdfDocument::with_version("1.7");
    let new_font = se_font(&mut new_document, &widths);
    let new_contents = new_document.add_object(Stream::new(dictionary! {}, new_text.into_bytes()));
    install_se_page(&mut new_document, new_contents, new_font);
    Ok((
        extract_document(old_document)?,
        extract_document(new_document)?,
    ))
}

#[test]
fn two_line_domain_proves_the_se_line_through_its_boundary() -> Result<()> {
    let (old, new) = two_line_pair()?;
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    let assessment = comparison.assessment.as_ref().expect("assessment");
    let multi = assessment.relations.iter().any(|relation| {
        relation
            .old_span
            .as_ref()
            .is_some_and(|span| span.blocks.len() > 1)
            && relation
                .new_span
                .as_ref()
                .is_some_and(|span| span.blocks.len() > 1)
            && relation.outcome == pdfdelta_core::diff::RelationOutcome::Established
    });
    assert!(
        multi,
        "the parent domain must close over both lines: {:#?}",
        assessment.relations
    );
    let proven = assessment.relations.iter().any(|relation| {
        relation.old_span.as_ref().is_some_and(|span| {
            span.blocks.len() == 1
                && span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 29 }
        }) && relation.new_span.as_ref().is_some_and(|span| {
            span.blocks.len() == 1
                && span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 44 }
        }) && relation
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement)
    });
    assert!(
        proven,
        "the line must be proven through the boundary cut: {:#?}",
        assessment.relations
    );
    let exact = comparison.changes.iter().any(|change| {
        change.kind == ChangeKind::Replacement
            && change.occurrences.iter().any(|occurrence| {
                occurrence.old_span.as_ref().is_some_and(|span| {
                    span.comparable_range == pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
                }) && occurrence.new_span.as_ref().is_some_and(|span| {
                    span.comparable_range == pdfdelta_core::diff::TokenRange { start: 27, end: 28 }
                })
            })
            && change.occurrences.iter().any(|occurrence| {
                occurrence.new_span.as_ref().is_some_and(|span| {
                    span.comparable_range == pdfdelta_core::diff::TokenRange { start: 29, end: 44 }
                })
            })
    });
    assert!(
        exact,
        "the year change and the insertion must stay exact: {:#?}",
        comparison.changes
    );
    let summary = pdfdelta_core::report::summarize(
        &comparison,
        &pdfdelta_core::report::ExtractionStatus::default(),
    )?;
    assert!(summary.comparison_complete, "{summary:#?}");
    assert_eq!(summary.unresolved_regions, 0);
    assert_eq!(summary.comparison_coverage, Some(1.0));
    Ok(())
}

fn two_line_pair_with(top_old: &str, top_new: &str) -> Result<(Document<Glyph>, Document<Glyph>)> {
    let widths = se_widths();
    let old_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 475.383 200.003 Tm ({}) Tj ET",
        escape(top_old),
        escape(SE_OLD)
    );
    let new_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 426.244 200.003 Tm ({}) Tj ET",
        escape(top_new),
        escape(SE_NEW)
    );
    let mut old_document = LopdfDocument::with_version("1.7");
    let old_font = se_font(&mut old_document, &widths);
    let old_contents = old_document.add_object(Stream::new(dictionary! {}, old_text.into_bytes()));
    install_se_page(&mut old_document, old_contents, old_font);
    let mut new_document = LopdfDocument::with_version("1.7");
    let new_font = se_font(&mut new_document, &widths);
    let new_contents = new_document.add_object(Stream::new(dictionary! {}, new_text.into_bytes()));
    install_se_page(&mut new_document, new_contents, new_font);
    Ok((
        extract_document(old_document)?,
        extract_document(new_document)?,
    ))
}

fn se_line_proven(assessment: &pdfdelta_core::diff::ComparisonAssessment) -> bool {
    assessment.relations.iter().any(|relation| {
        relation.old_span.as_ref().is_some_and(|span| {
            span.blocks.len() == 1
                && span.comparable_range == pdfdelta_core::diff::TokenRange { start: 0, end: 29 }
        }) && relation
            .assumptions
            .contains(&ComparisonAssumption::ExactTextDisplacement)
    })
}

#[test]
fn repeated_tokens_that_compete_across_the_cut_stay_ambiguous() -> Result<()> {
    // The top line appends the same "Schedule" token the SE line starts
    // with, so an optimal script can pair the appended token across the cut.
    let (old, new) = two_line_pair_with("Schedule", "Schedule Schedule")?;
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    let assessment = comparison.assessment.as_ref().expect("assessment");
    assert!(
        !se_line_proven(assessment),
        "a competing cut cannot prove the line: {:#?}",
        assessment.relations
    );
    Ok(())
}

#[test]
fn a_line_without_whole_block_correspondence_stays_ambiguous() -> Result<()> {
    // The new side emits the SE line as two blocks, so the child proposal is
    // not a whole single block on both sides.
    let widths = se_widths();
    let old_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 475.383 200.003 Tm ({}) Tj ET",
        escape("Form 1040 2024"),
        escape(SE_OLD)
    );
    let new_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 426.244 208.003 Tm ({}) Tj ET\nBT /T2 1 Tf 7 0 0 7 500.0 200.003 Tm ({}) Tj ET",
        escape("Form 1040 2025"),
        escape("Schedule SE (Form 1040) 2025 "),
        escape("Created 5/7/25 ")
    );
    let mut old_document = LopdfDocument::with_version("1.7");
    let old_font = se_font(&mut old_document, &widths);
    let old_contents = old_document.add_object(Stream::new(dictionary! {}, old_text.into_bytes()));
    install_se_page(&mut old_document, old_contents, old_font);
    let mut new_document = LopdfDocument::with_version("1.7");
    let new_font = se_font(&mut new_document, &widths);
    let new_font2 = se_font(&mut new_document, &widths);
    let new_contents = new_document.add_object(Stream::new(dictionary! {}, new_text.into_bytes()));
    install_se_page2(&mut new_document, new_contents, new_font, new_font2);
    let (old, new) = (
        extract_document(old_document)?,
        extract_document(new_document)?,
    );
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    let assessment = comparison.assessment.as_ref().expect("assessment");
    assert!(
        !se_line_proven(assessment),
        "a one-sided line boundary cannot prove the line: {:#?}",
        assessment.relations
    );
    Ok(())
}

#[test]
fn a_budget_cut_inside_the_boundary_proof_stays_incomplete() -> Result<()> {
    let (old, new) = two_line_pair()?;
    let options = PipelineOptions {
        diff: pdfdelta_core::diff::DiffOptions {
            max_assessment_work: 120_000,
            ..pdfdelta_core::diff::DiffOptions::default()
        },
        ..PipelineOptions::default()
    };
    let comparison = compare_glyph_documents(&old, &new, options)?;
    if let Some(assessment) = comparison.assessment.as_ref() {
        assert!(
            !se_line_proven(assessment),
            "an exhausted proof cannot prove the line: {:#?}",
            assessment.relations
        );
        for relation in &assessment.relations {
            if relation.old_span.as_ref().is_some_and(|span| {
                span.blocks.len() == 1
                    && span.comparable_range
                        == pdfdelta_core::diff::TokenRange { start: 0, end: 29 }
            }) && relation.search == pdfdelta_core::diff::SearchCompleteness::Incomplete
            {
                assert!(
                    relation
                        .reasons
                        .contains(&pdfdelta_core::diff::AssessmentReason::WorkLimit),
                    "incomplete boundary work reports the limit: {relation:#?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn a_range_limit_never_publishes_a_partial_boundary_proof() -> Result<()> {
    let (old, new) = two_line_pair()?;
    let options = PipelineOptions {
        diff: pdfdelta_core::diff::DiffOptions {
            max_assessment_ranges: 4,
            ..pdfdelta_core::diff::DiffOptions::default()
        },
        ..PipelineOptions::default()
    };
    let comparison = compare_glyph_documents(&old, &new, options)?;
    let assessment = comparison.assessment.as_ref().expect("assessment");
    assert!(
        !se_line_proven(assessment),
        "an output stop cannot publish a partial proof: {:#?}",
        assessment.relations
    );
    assert!(
        assessment.candidates_truncated,
        "the range limit must truncate candidates"
    );
    let summary = pdfdelta_core::report::summarize(
        &comparison,
        &pdfdelta_core::report::ExtractionStatus::default(),
    )?;
    assert!(
        !summary.comparison_complete,
        "a truncated assessment is not complete: {summary:#?}"
    );
    Ok(())
}
