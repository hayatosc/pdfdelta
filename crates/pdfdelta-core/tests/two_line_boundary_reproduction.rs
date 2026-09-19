//! Reproduction: a two-line page whose closed parent domain spans both blocks
//! must still resolve the line-level exact edits.
//!
//! The fixture renders an SE-style line (a year replacement plus an appended
//! date) under an independent short line, so the closed domain covers two
//! blocks. Before the line-level exact-displacement proof the line stayed
//! ambiguous even though the page-level correspondence was closed.

use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, Stream, dictionary};
use pdfdelta_core::{
    Result,
    diff::ChangeKind,
    model::{Document, Glyph},
    pdf::{LopdfParser, ParseLimits, PdfParser},
    pipeline::{PipelineOptions, compare_glyph_documents},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};

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

fn widths() -> std::collections::HashMap<char, f64> {
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

fn font(
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

fn install_page(document: &mut LopdfDocument, contents: lopdf::ObjectId, font: lopdf::ObjectId) {
    let resources = dictionary! {
        "Font" => dictionary! { "T1" => Object::Reference(font) },
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

fn extract(mut document: LopdfDocument) -> Result<Document<Glyph>> {
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).expect("serialize");
    let pdf = LopdfParser.parse(Arc::from(bytes), ParseLimits::default())?;
    ContentStreamGlyphExtractor.extract(pdf.as_ref(), ExtractionLimits::default())
}

fn two_line_pair() -> Result<(Document<Glyph>, Document<Glyph>)> {
    let widths = widths();
    let old_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 475.383 200.003 Tm ({}) Tj ET",
        escape("Form 1040 2024"),
        escape(SE_OLD)
    );
    let new_text = format!(
        "BT /T1 1 Tf 7 0 0 7 475.383 232.003 Tm ({}) Tj ET\nBT /T1 1 Tf 7 0 0 7 426.244 200.003 Tm ({}) Tj ET",
        escape("Form 1040 2025"),
        escape(SE_NEW)
    );
    let mut old_document = LopdfDocument::with_version("1.7");
    let old_font = font(&mut old_document, &widths);
    let old_contents = old_document.add_object(Stream::new(dictionary! {}, old_text.into_bytes()));
    install_page(&mut old_document, old_contents, old_font);
    let mut new_document = LopdfDocument::with_version("1.7");
    let new_font = font(&mut new_document, &widths);
    let new_contents = new_document.add_object(Stream::new(dictionary! {}, new_text.into_bytes()));
    install_page(&mut new_document, new_contents, new_font);
    Ok((extract(old_document)?, extract(new_document)?))
}

#[test]
fn two_line_page_resolves_the_line_exact_edits() -> Result<()> {
    let (old, new) = two_line_pair()?;
    let comparison = compare_glyph_documents(&old, &new, PipelineOptions::default())?;
    assert!(
        comparison.unresolved_regions.is_empty(),
        "the closed page must leave no unresolved line: {comparison:#?}"
    );
    assert!(
        comparison.change_candidates.is_empty(),
        "the closed page must leave no candidates: {comparison:#?}"
    );
    let exact = comparison.changes.iter().any(|change| {
        change.kind == ChangeKind::Replacement
            && change.occurrences.iter().any(|occurrence| {
                occurrence.old_span.as_ref().is_some_and(|span| {
                    span.comparable_range.start == 27 && span.comparable_range.end == 28
                }) && occurrence.new_span.as_ref().is_some_and(|span| {
                    span.comparable_range.start == 27 && span.comparable_range.end == 28
                })
            })
            && change.occurrences.iter().any(|occurrence| {
                occurrence.new_span.as_ref().is_some_and(|span| {
                    span.comparable_range.start == 29 && span.comparable_range.end == 44
                })
            })
    });
    assert!(
        exact,
        "the year change and the insertion stay exact: {comparison:#?}"
    );
    let summary = pdfdelta_core::report::summarize(
        &comparison,
        &pdfdelta_core::report::ExtractionStatus::default(),
    )?;
    assert!(summary.comparison_complete, "{summary:#?}");
    assert_eq!(summary.comparison_coverage, Some(1.0));
    Ok(())
}
