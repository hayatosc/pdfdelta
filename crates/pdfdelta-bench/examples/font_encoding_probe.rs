//! Inventory Type 0 font encoding prerequisites through bounded PDF objects.

use pdfdelta_core::pdf::{LopdfParser, ParseLimits, ParsedPdf, PdfDict, PdfObject, PdfParser};
use serde_json::{Value, json};
use std::{collections::BTreeSet, env, fs::File, io::Read, sync::Arc};

fn field(value: Option<&PdfObject>) -> Value {
    match value {
        None => Value::Null,
        Some(PdfObject::Name(bytes) | PdfObject::String(bytes)) => {
            json!({"bytes": bytes, "display": String::from_utf8_lossy(bytes)})
        }
        Some(PdfObject::Integer(value)) => json!(value),
        Some(PdfObject::Reference(value)) => {
            json!({"object": value.object_number, "generation": value.generation})
        }
        Some(_) => json!({"non_scalar": true}),
    }
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn resolved(pdf: &dyn ParsedPdf, object: &PdfObject) -> Result<PdfObject> {
    match object {
        PdfObject::Reference(reference) => Ok(pdf.resolve_with_terminal(*reference)?.object),
        other => Ok(other.clone()),
    }
}

fn dictionary(pdf: &dyn ParsedPdf, object: &PdfObject) -> Result<PdfDict> {
    match resolved(pdf, object)? {
        PdfObject::Dictionary(value) | PdfObject::Stream(value) => Ok(value),
        _ => Err("expected resource dictionary".into()),
    }
}

fn walk(
    pdf: &dyn ParsedPdf,
    resources: &PdfObject,
    seen: &mut BTreeSet<(u32, u16)>,
    rows: &mut Vec<Value>,
    visits: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > 32 || *visits >= 100_000 {
        return Err("font resource traversal limit".into());
    }
    *visits += 1;
    let resources = dictionary(pdf, resources)?;
    if let Some(fonts) = resources.get(b"Font".as_slice()) {
        for font in dictionary(pdf, fonts)?.values() {
            *visits += 1;
            if *visits > 100_000 {
                return Err("font resource traversal limit".into());
            }
            if let PdfObject::Reference(reference) = font
                && !seen.insert((reference.object_number, reference.generation))
            {
                continue;
            }
            let data = dictionary(pdf, font)?;
            if data.get(b"Subtype".as_slice()) != Some(&PdfObject::Name(b"Type0".to_vec())) {
                continue;
            }
            let descendants = resolved(
                pdf,
                data.get(b"DescendantFonts".as_slice())
                    .ok_or("missing descendants")?,
            )?;
            let PdfObject::Array(descendants) = descendants else {
                return Err("invalid descendants".into());
            };
            let mut cid = Vec::new();
            for descendant in descendants {
                let d = dictionary(pdf, &descendant)?;
                let system = d
                    .get(b"CIDSystemInfo".as_slice())
                    .map(|x| dictionary(pdf, x))
                    .transpose()?;
                let descriptor = d
                    .get(b"FontDescriptor".as_slice())
                    .map(|x| dictionary(pdf, x))
                    .transpose()?;
                cid.push(json!({
                    "subtype": field(d.get(b"Subtype".as_slice())),
                    "system_info": system.map(|d| json!({
                        "registry": field(d.get(b"Registry".as_slice())),
                        "ordering": field(d.get(b"Ordering".as_slice())),
                        "supplement": field(d.get(b"Supplement".as_slice())),
                    })),
                    "font_programs": descriptor.map(|d| json!({
                        "font_file": field(d.get(b"FontFile".as_slice())),
                        "font_file2": field(d.get(b"FontFile2".as_slice())),
                        "font_file3": field(d.get(b"FontFile3".as_slice())),
                    })),
                }));
            }
            rows.push(json!({
                "font": field(Some(font)),
                "encoding": field(data.get(b"Encoding".as_slice())),
                "base_font": field(data.get(b"BaseFont".as_slice())),
                "to_unicode": field(data.get(b"ToUnicode".as_slice())),
                "descendants": cid,
            }));
        }
    }
    if let Some(objects) = resources.get(b"XObject".as_slice()) {
        for object in dictionary(pdf, objects)?.values() {
            *visits += 1;
            if *visits > 100_000 {
                return Err("font resource traversal limit".into());
            }
            if let PdfObject::Reference(reference) = object
                && !seen.insert((reference.object_number, reference.generation))
            {
                continue;
            }
            let data = dictionary(pdf, object)?;
            if data.get(b"Subtype".as_slice()) == Some(&PdfObject::Name(b"Form".to_vec()))
                && let Some(nested) = data.get(b"Resources".as_slice())
            {
                walk(pdf, nested, seen, rows, visits, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let path = env::args().nth(1).ok_or("expected PDF path")?;
    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("PDF input byte limit".into());
    }
    let pdf = LopdfParser.parse(Arc::from(bytes), limits)?;
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    let mut visits = 0;
    let mut pages_without_explicit_resources = 0;
    for page in pdf.pages()? {
        if let Some(resources) = pdf.page_dict(page)?.get(b"Resources".as_slice()) {
            walk(
                pdf.as_ref(),
                resources,
                &mut seen,
                &mut rows,
                &mut visits,
                0,
            )?;
        } else {
            pages_without_explicit_resources += 1;
        }
    }
    println!(
        "{}",
        serde_json::to_string(&json!({
            "font_records": rows,
            "resource_visits": visits,
            "pages_without_explicit_resources": pages_without_explicit_resources,
            "scope": "Explicit page and nested Form resources; inherited resources are not traversed",
        }))?
    );
    Ok(())
}
