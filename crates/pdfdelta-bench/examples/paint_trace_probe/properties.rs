//! Reachable declarations are metadata observations, not executed-paint claims.

use std::collections::BTreeSet;

use pdfdelta_core::pdf::{ObjectRef, ParsedPdf, PdfObject};
use serde_json::{Value, json};

use super::{MAX_PAGE_BYTES, MAX_RESOURCE_VISITS, ProbeResult, trace};

struct Reader<'a> {
    pdf: &'a dyn ParsedPdf,
    visited: BTreeSet<(u32, u16)>,
    remaining: usize,
    bytes: usize,
    declarations: Vec<Value>,
    forms: Vec<Value>,
}

impl Reader<'_> {
    fn charge(&mut self, bytes: usize) -> ProbeResult<()> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("metadata visit limit")?;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_PAGE_BYTES {
            return Err("metadata/form byte limit".into());
        }
        Ok(())
    }

    fn visit(
        &mut self,
        value: &PdfObject,
        owner: Option<ObjectRef>,
        path: &str,
        depth: usize,
    ) -> ProbeResult<()> {
        if depth > 32 {
            return Err("metadata traversal depth limit".into());
        }
        self.charge(0)?;
        match value {
            PdfObject::Reference(reference) => {
                if !self
                    .visited
                    .insert((reference.object_number, reference.generation))
                {
                    return Ok(());
                }
                let resolved = self.pdf.resolve_with_terminal(*reference)?;
                if resolved.reference != *reference
                    && !self.visited.insert((
                        resolved.reference.object_number,
                        resolved.reference.generation,
                    ))
                {
                    return Ok(());
                }
                if let PdfObject::Stream(dictionary) = &resolved.object
                    && matches!(dictionary.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name == b"Form")
                {
                    let stream = self.pdf.decoded_stream(resolved.reference)?;
                    self.charge(stream.bytes.len())?;
                    let observed = trace(&stream.bytes, false)?;
                    for count in observed["operation_counts"]
                        .as_object()
                        .ok_or("missing operator census")?
                        .values()
                    {
                        let count =
                            usize::try_from(count.as_u64().ok_or("invalid operator count")?)?;
                        self.remaining = self
                            .remaining
                            .checked_sub(count)
                            .ok_or("form operator limit")?;
                    }
                    self.forms
                        .push(json!({"object": resolved.reference, "observed": observed}));
                }
                // Dictionary back-references are visited once. This says nothing
                // about Form invocation recursion or visibility of a resource.
                self.visit(
                    &resolved.object,
                    Some(resolved.reference),
                    "object",
                    depth + 1,
                )?;
            }
            PdfObject::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    self.visit(value, owner, &format!("{path}[{index}]"), depth + 1)?;
                }
            }
            PdfObject::Dictionary(dictionary) | PdfObject::Stream(dictionary) => {
                for (key, value) in dictionary {
                    self.charge(key.len())?;
                    let path = format!("{path}/{key:?}");
                    self.visit(value, owner, &path, depth + 1)?;
                    if matches!(key.as_slice(), b"ActualText" | b"Alt") {
                        self.declarations.push(json!({
                            "object": owner, "path": path,
                            "key": String::from_utf8_lossy(key),
                            "value": format!("{value:?}"),
                            "direct_string": matches!(value, PdfObject::String(_)),
                        }));
                    }
                }
            }
            PdfObject::Name(bytes) | PdfObject::String(bytes) => self.charge(bytes.len())?,
            _ => {}
        }
        Ok(())
    }
}

pub(super) fn scan(pdf: &dyn ParsedPdf) -> Value {
    let mut reader = Reader {
        pdf,
        visited: BTreeSet::new(),
        remaining: MAX_RESOURCE_VISITS,
        bytes: 0,
        declarations: Vec::new(),
        forms: Vec::new(),
    };
    let result = pdf
        .trailer()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
        .and_then(|trailer| reader.visit(&PdfObject::Dictionary(trailer), None, "trailer", 0));
    json!({
        "status": if result.is_ok() { "scanned" } else { "unresolved" },
        "reason": result.err().map(|error| error.to_string()),
        "certifies_text_inventory": false,
        "execution_binding": "not established; reachable resources may be unused",
        "scope": "trailer-reachable dictionaries and streams declared as Form; other stream programs are not decoded",
        "visited_objects": reader.visited.len(), "charged_bytes": reader.bytes,
        "declarations": reader.declarations, "form_programs": reader.forms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Document, Object, Stream, dictionary};
    use pdfdelta_core::pdf::{LopdfParser, ParseLimits, PdfParser};
    use std::sync::Arc;

    fn fixture(bad_filter: bool) -> Vec<u8> {
        let mut pdf = Document::with_version("1.7");
        let root = pdf.new_object_id();
        let pages = pdf.add_object(
            dictionary! {"Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0},
        );
        let element = pdf.add_object(dictionary! {"Type" => "StructElem", "S" => "P", "ActualText" => Object::string_literal("declared"), "P" => root});
        let mut dictionary = dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()]};
        if bad_filter {
            dictionary.set("Filter", "UnknownFilter");
        }
        let form = pdf.add_object(Stream::new(
            dictionary,
            b"/Span << /ActualText (form) >> BDC 0 0 1 1 re f EMC".to_vec(),
        ));
        pdf.objects.insert(root, dictionary! {"Type" => "Catalog", "Pages" => pages, "StructTreeRoot" => element, "UninvokedForm" => form}.into());
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn declarations_and_uninvoked_forms_do_not_certify_text() {
        let pdf = LopdfParser
            .parse(Arc::from(fixture(false)), ParseLimits::default())
            .unwrap();
        let result = scan(pdf.as_ref());
        assert_eq!(result["status"], "scanned");
        assert_eq!(result["declarations"].as_array().unwrap().len(), 1);
        assert_eq!(result["form_programs"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["form_programs"][0]["observed"]["inline_actual_text_declarations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(result["certifies_text_inventory"], false);
        let pdf = LopdfParser
            .parse(Arc::from(fixture(true)), ParseLimits::default())
            .unwrap();
        assert_eq!(scan(pdf.as_ref())["status"], "unresolved");
    }
}
