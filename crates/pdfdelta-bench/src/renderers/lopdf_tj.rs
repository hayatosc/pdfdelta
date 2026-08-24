use std::fmt::Write as _;

use lopdf::{Document, Object, Stream, dictionary};

use super::{RenderLimits, escape_pdf_literal, line_y, render_error};
use crate::{
    Result,
    mutation::{DEFAULT_PAGE_WIDTH, RenderPlan},
};

const NAME: &str = "lopdf-tj";

pub(super) fn render(plan: &RenderPlan, limits: RenderLimits) -> Result<Vec<u8>> {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let font_descriptor = document.add_object(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => "Helvetica",
        "Flags" => 32,
        "Ascent" => 800,
        "Descent" => -200,
        "CapHeight" => 700,
        "FontBBox" => vec![
            Object::Integer(-166),
            Object::Integer(-225),
            Object::Integer(1000),
            Object::Integer(931),
        ],
        "ItalicAngle" => 0,
        "StemV" => 80,
        "MissingWidth" => 500,
    });
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
        "FirstChar" => 0,
        "LastChar" => 255,
        "Widths" => vec![Object::Integer(500); 256],
        "FontDescriptor" => font_descriptor,
    });
    let resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });

    let mut page_ids = Vec::with_capacity(plan.pages().len());
    for lines in plan.pages() {
        let mut content = String::new();
        for (index, line) in lines.iter().enumerate() {
            writeln!(
                content,
                "BT /F1 10 Tf 1 0 0 1 {} {} Tm ({}) Tj ET",
                plan.margin(),
                line_y(index, plan.line_gap())?,
                escape_pdf_literal(line)
            )
            .map_err(|error| render_error(NAME, error.to_string()))?;
        }
        let contents = document.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "Contents" => contents,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), i64::from(DEFAULT_PAGE_WIDTH).into(), 792.into()],
        }));
    }
    let page_count =
        i64::try_from(page_ids.len()).map_err(|_| render_error(NAME, "page count exceeds i64"))?;
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.into_iter().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_count,
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);

    let mut output = Vec::new();
    document
        .save_to(&mut output)
        .map_err(|error| render_error(NAME, error.to_string()))?;
    if output.len() > limits.max_pdf_bytes {
        return Err(render_error(
            NAME,
            format!(
                "generated PDF exceeds the {}-byte limit",
                limits.max_pdf_bytes
            ),
        ));
    }
    Ok(output)
}
