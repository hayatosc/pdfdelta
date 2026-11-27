//! Regression: re-subsetted fonts must not turn unmapped glyphs into changes.
//!
//! Two revisions of the same document can re-embed the same named font with
//! different program bytes. An unmapped glyph is identified by its
//! subset-stripped `BaseFont` and glyph name, so the two program hashes must not
//! matter. A different glyph name still identifies a different glyph.

use std::sync::Arc;

use lopdf::{Document as LopdfDocument, Object, Stream, dictionary};
use pdfdelta_core::{
    model::{DecodedText, Document, Glyph, GlyphIdentitySource},
    pdf::{LopdfParser, ParseLimits},
    source::{ContentStreamGlyphExtractor, ExtractionLimits, ParserBackedGlyphSource},
};
use pdfdelta_diff::{candidates_owned, diff, order_from_candidates};

fn install_page(document: &mut LopdfDocument, font: Object, content: Object) {
    let pages = document.new_object_id();
    let page = document.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages,
        "Contents" => content,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
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
}

/// Builds a one-glyph PDF. `program` bytes carry a trailing marker so the two
/// subsets hash differently without changing the declared glyph name.
fn subset_pdf(base_font: &str, program: &[u8], glyph_name: &str) -> Vec<u8> {
    let mut document = LopdfDocument::with_version("1.7");
    let program = document.add_object(Stream::new(dictionary! {}, program.to_vec()));
    let descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => base_font,
        "Ascent" => 800,
        "Descent" => -200,
        "MissingWidth" => 500,
        "Flags" => 32,
        "FontFile" => program,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => base_font,
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "Encoding" => dictionary! {
            "Type" => "Encoding",
            "Differences" => vec![
                Object::Integer(2),
                Object::Name(glyph_name.as_bytes().to_vec()),
            ],
        },
        "FontDescriptor" => descriptor,
    });
    let content = document.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 10 Tf 1 0 0 1 20 30 Tm (\x02) Tj ET".to_vec(),
    ));
    install_page(&mut document, font.into(), content.into());
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("fixture PDF should serialize");
    bytes
}

fn extract(bytes: Vec<u8>) -> Document<Glyph> {
    let source = ParserBackedGlyphSource::new(LopdfParser, ContentStreamGlyphExtractor);
    source
        .extract(
            Arc::from(bytes),
            ParseLimits::default(),
            ExtractionLimits::default(),
        )
        .expect("fixture extraction")
}

fn unmapped_identity(document: &Document<Glyph>) -> (Vec<u8>, u16) {
    let glyphs = document.items();
    assert_eq!(glyphs.len(), 1);
    let DecodedText::Unmapped {
        font_hash,
        glyph_id,
    } = &glyphs[0].text
    else {
        panic!("fixture glyph must stay unmapped");
    };
    assert_eq!(font_hash.identity_source(), GlyphIdentitySource::Name);
    assert_eq!(*glyph_id, 0);
    (font_hash.0.clone(), *glyph_id)
}

fn diff_changes(old: Document<Glyph>, new: Document<Glyph>) -> Vec<pdfdelta_diff::Change> {
    let old = candidates_owned(old).expect("old candidates");
    let new = candidates_owned(new).expect("new candidates");
    let pair = order_from_candidates(old, new).expect("order selection");
    diff(&pair.old.tokens, &pair.new.tokens)
        .expect("verified diff")
        .changes
}

#[test]
fn two_subsets_with_equal_names_and_different_programs_have_no_changes() {
    let first = subset_pdf(
        "AAAAAA+Fixture",
        b"%!PS-AdobeFont-1.0: Fixture\n/Encoding 256 array\neexec\nfirst\n",
        "TeXprime",
    );
    let second = subset_pdf(
        "BBBBBB+Fixture",
        b"%!PS-AdobeFont-1.0: Fixture\n/Encoding 256 array\neexec\nsecond\n",
        "TeXprime",
    );
    let old = extract(first.clone());
    let new = extract(second.clone());
    let old_identity = unmapped_identity(&old);
    let new_identity = unmapped_identity(&new);
    assert_eq!(old_identity, new_identity);
    assert!(
        diff_changes(old, new).is_empty(),
        "re-subsetting must not change a name-identified placeholder"
    );
}

#[test]
fn different_glyph_names_remain_a_change() {
    let first = subset_pdf(
        "AAAAAA+Fixture",
        b"%!PS-AdobeFont-1.0: Fixture\n/Encoding 256 array\neexec\nfirst\n",
        "TeXprime",
    );
    let different = subset_pdf(
        "BBBBBB+Fixture",
        b"%!PS-AdobeFont-1.0: Fixture\n/Encoding 256 array\neexec\nsecond\n",
        "TeXintegral",
    );
    let old = extract(first);
    let new = extract(different);
    assert_ne!(unmapped_identity(&old), unmapped_identity(&new));
    assert!(
        !diff_changes(old, new).is_empty(),
        "different names must remain an observable placeholder change"
    );
}
