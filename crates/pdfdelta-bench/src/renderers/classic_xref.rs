use std::{fmt::Write as _, io::Write as _};

use super::{PlanStats, RenderLimits, escape_pdf_literal, line_y, render_error};
use crate::{BenchError, Result, mutation::RenderPlan};

const NAME: &str = "classic-xref-tj";
const CATALOG_ID: usize = 1;
const PAGES_ID: usize = 2;
const FONT_ID: usize = 3;
const FONT_DESCRIPTOR_ID: usize = 4;

pub(super) fn render(plan: &RenderPlan, limits: RenderLimits, stats: PlanStats) -> Result<Vec<u8>> {
    let object_count = plan
        .pages()
        .len()
        .checked_mul(2)
        .and_then(|count| count.checked_add(FONT_DESCRIPTOR_ID))
        .ok_or_else(|| render_error(NAME, "object count overflowed"))?;
    let estimated_size = stats
        .total_text_bytes
        .checked_mul(3)
        .and_then(|size| size.checked_add(plan.pages().len().saturating_mul(2_048)))
        .and_then(|size| size.checked_add(8_192))
        .ok_or_else(|| render_error(NAME, "PDF size estimate overflowed"))?;
    if estimated_size > limits.max_pdf_bytes {
        return Err(render_error(
            NAME,
            format!(
                "estimated PDF exceeds the {}-byte limit",
                limits.max_pdf_bytes
            ),
        ));
    }

    let mut output = Vec::with_capacity(estimated_size);
    output.extend_from_slice(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n");
    let mut offsets = Vec::with_capacity(object_count);

    emit_object(
        &mut output,
        CATALOG_ID,
        b"<< /Type /Catalog /Pages 2 0 R >>\n",
        &mut offsets,
    )?;

    let mut kids = String::new();
    for page_index in 0..plan.pages().len() {
        write!(kids, "{} 0 R ", page_object_id(page_index)?)
            .map_err(|error| render_error(NAME, error.to_string()))?;
    }
    let pages_body = format!(
        "<< /Type /Pages /Kids [{kids}] /Count {} >>\n",
        plan.pages().len()
    );
    emit_object(&mut output, PAGES_ID, pages_body.as_bytes(), &mut offsets)?;

    let widths = (0..256).map(|_| "500").collect::<Vec<_>>().join(" ");
    let font_body = format!(
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 0 /LastChar 255 /Widths [{widths}] /FontDescriptor {FONT_DESCRIPTOR_ID} 0 R >>\n"
    );
    emit_object(&mut output, FONT_ID, font_body.as_bytes(), &mut offsets)?;
    emit_object(
        &mut output,
        FONT_DESCRIPTOR_ID,
        b"<< /Type /FontDescriptor /FontName /Helvetica /Flags 32 /FontBBox [-166 -225 1000 931] /Ascent 800 /Descent -200 /CapHeight 700 /ItalicAngle 0 /StemV 80 /MissingWidth 500 >>\n",
        &mut offsets,
    )?;

    for (page_index, lines) in plan.pages().iter().enumerate() {
        let page_id = page_object_id(page_index)?;
        let content_id = content_object_id(page_index)?;
        let page_body = format!(
            "<< /Type /Page /Parent {PAGES_ID} 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 {FONT_ID} 0 R >> >> /Contents {content_id} 0 R >>\n",
            plan.page_width(),
            plan.page_height()
        );
        emit_object(&mut output, page_id, page_body.as_bytes(), &mut offsets)?;

        let content = positioned_content(lines, plan.line_gap(), plan.margin(), plan.font_size())?;
        let mut content_body = Vec::with_capacity(content.len().saturating_add(64));
        write!(content_body, "<< /Length {} >>\nstream\n", content.len())
            .map_err(|error| render_error(NAME, error.to_string()))?;
        content_body.extend_from_slice(&content);
        content_body.extend_from_slice(b"endstream\n");
        emit_object(&mut output, content_id, &content_body, &mut offsets)?;
    }

    if offsets.len() != object_count {
        return Err(render_error(NAME, "object table is incomplete"));
    }
    let xref_offset = output.len();
    write!(output, "xref\n0 {}\n", object_count + 1)
        .map_err(|error| render_error(NAME, error.to_string()))?;
    output.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        writeln!(output, "{offset:010} 00000 n ")
            .map_err(|error| render_error(NAME, error.to_string()))?;
    }
    write!(
        output,
        "trailer\n<< /Size {} /Root {CATALOG_ID} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
        object_count + 1
    )
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

fn positioned_content(
    lines: &[String],
    line_gap: u16,
    margin: u16,
    font_size: u16,
) -> Result<Vec<u8>> {
    let text_bytes = lines.iter().map(String::len).sum::<usize>();
    let mut content = Vec::with_capacity(text_bytes.saturating_mul(2).saturating_add(256));
    for (line_index, line) in lines.iter().enumerate() {
        write!(
            content,
            "BT /F1 {} Tf 1 0 0 1 {} {} Tm [",
            font_size,
            margin,
            line_y(line_index, line_gap)?
        )
        .map_err(|error| render_error(NAME, error.to_string()))?;
        for (word_index, word) in line.split(' ').enumerate() {
            if word_index > 0 {
                content.extend_from_slice(b" -500 ");
            }
            write!(content, "({})", escape_pdf_literal(word))
                .map_err(|error| render_error(NAME, error.to_string()))?;
        }
        content.extend_from_slice(b"] TJ ET\n");
    }
    Ok(content)
}

fn emit_object(
    output: &mut Vec<u8>,
    object_id: usize,
    body: &[u8],
    offsets: &mut Vec<usize>,
) -> Result<()> {
    if object_id != offsets.len() + 1 {
        return Err(render_error(
            NAME,
            "objects must be emitted in numeric order",
        ));
    }
    offsets.push(output.len());
    writeln!(output, "{object_id} 0 obj").map_err(|error| render_error(NAME, error.to_string()))?;
    output.extend_from_slice(body);
    if !body.ends_with(b"\n") {
        output.push(b'\n');
    }
    output.extend_from_slice(b"endobj\n");
    Ok(())
}

fn page_object_id(page_index: usize) -> Result<usize> {
    dynamic_object_id(page_index, 1)
}

fn content_object_id(page_index: usize) -> Result<usize> {
    dynamic_object_id(page_index, 2)
}

fn dynamic_object_id(page_index: usize, offset: usize) -> Result<usize> {
    page_index
        .checked_mul(2)
        .and_then(|id| id.checked_add(FONT_DESCRIPTOR_ID + offset))
        .ok_or_else(|| BenchError::Render {
            renderer: NAME,
            message: "object id overflowed".to_owned(),
        })
}
