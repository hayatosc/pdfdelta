//! Reachable declarations are metadata observations, not executed-paint claims.

use std::collections::{BTreeSet, VecDeque};

use pdfdelta_core::pdf::{ObjectRef, ParsedPdf, PdfObject};
use serde_json::{Value, json};

use super::{MAX_PAGE_BYTES, MAX_RESOURCE_VISITS, ProbeResult, trace};

fn key(reference: ObjectRef) -> (u32, u16) {
    (reference.object_number, reference.generation)
}

struct Reader<'a> {
    pdf: &'a dyn ParsedPdf,
    /// References already encountered or reached as a terminal.
    visited: BTreeSet<(u32, u16)>,
    /// Terminal references whose object contents were already processed. An
    /// alias chain and the terminal itself are only expanded once.
    queued: BTreeSet<(u32, u16)>,
    remaining: usize,
    bytes: usize,
    byte_limit: usize,
    declarations: Vec<Value>,
    forms: Vec<Value>,
    /// Worklist of references only: large decoded objects are resolved and
    /// charged during `drain`, never retained uncharged.
    pending: VecDeque<ObjectRef>,
}

impl Reader<'_> {
    fn charge(&mut self, bytes: usize) -> ProbeResult<()> {
        self.remaining = self
            .remaining
            .checked_sub(1)
            .ok_or("metadata visit limit")?;
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > self.byte_limit {
            return Err("metadata/form byte limit".into());
        }
        Ok(())
    }

    /// Visits one value. Inline Array/Dictionary nesting still charges the
    /// direct depth limit; a reference is only queued, so reference chains
    /// cannot exhaust depth and no object is cloned into the worklist.
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
                if self.visited.insert(key(*reference)) {
                    self.pending.push_back(*reference);
                }
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

    /// Processes queued references in encounter order until the worklist
    /// drains. Resolution and charging happen here, so the worklist holds only
    /// references. An alias and its terminal expand once; breadth-first order
    /// finds shallow declarations before deeper failures, and the first limit
    /// or decode error retains every observation already collected.
    fn drain(&mut self) -> ProbeResult<()> {
        while let Some(reference) = self.pending.pop_front() {
            let resolved = self.pdf.resolve_with_terminal(reference)?;
            self.visited.insert(key(resolved.reference));
            if !self.queued.insert(key(resolved.reference)) {
                continue;
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
                    let count = usize::try_from(count.as_u64().ok_or("invalid operator count")?)?;
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
            self.visit(&resolved.object, Some(resolved.reference), "object", 0)?;
        }
        Ok(())
    }
}

pub(super) fn scan(pdf: &dyn ParsedPdf) -> Value {
    scan_with_limits(pdf, MAX_RESOURCE_VISITS, MAX_PAGE_BYTES)
}

/// The same bounded traversal used by every probe mode. Limits are explicit so
/// tests can confirm that a stop preserves partial observations.
fn scan_with_limits(pdf: &dyn ParsedPdf, visits: usize, bytes: usize) -> Value {
    let mut reader = Reader {
        pdf,
        visited: BTreeSet::new(),
        queued: BTreeSet::new(),
        remaining: visits,
        bytes: 0,
        byte_limit: bytes,
        declarations: Vec::new(),
        forms: Vec::new(),
        pending: VecDeque::new(),
    };
    let result = pdf
        .trailer()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
        .and_then(|trailer| {
            reader.visit(&PdfObject::Dictionary(trailer), None, "trailer", 0)?;
            reader.drain()
        });
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

    fn chain_fixture(links: usize) -> Vec<u8> {
        let mut pdf = Document::with_version("1.7");
        let root = pdf.new_object_id();
        let pages = pdf.add_object(
            dictionary! {"Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0},
        );
        let mut current = pdf.add_object(dictionary! {"Type" => "StructElem", "S" => "P", "ActualText" => Object::string_literal("terminal"), "P" => root});
        for _ in 0..links {
            current = pdf.add_object(dictionary! {"Next" => current});
        }
        pdf.objects.insert(
            root,
            dictionary! {"Type" => "Catalog", "Pages" => pages, "StructTreeRoot" => current}.into(),
        );
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        bytes
    }

    fn scan_bytes(bytes: Vec<u8>) -> Value {
        let pdf = LopdfParser
            .parse(Arc::from(bytes), ParseLimits::default())
            .unwrap();
        scan(pdf.as_ref())
    }

    #[test]
    fn declarations_and_uninvoked_forms_do_not_certify_text() {
        let result = scan_bytes(fixture(false));
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
        let unresolved = scan_bytes(fixture(true));
        assert_eq!(unresolved["status"], "unresolved");
        assert_eq!(
            unresolved["declarations"].as_array().unwrap().len(),
            1,
            "partial declarations survive a failed Form decode"
        );
    }

    #[test]
    fn long_indirect_chains_reach_terminal_declarations() {
        let result = scan_bytes(chain_fixture(60));
        assert_eq!(result["status"], "scanned", "{result}");
        let declarations = result["declarations"].as_array().unwrap();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["direct_string"], true);
        assert_eq!(
            declarations[0]["value"],
            format!("{:?}", PdfObject::String(b"terminal".to_vec()))
        );
    }

    #[test]
    fn directly_deep_nesting_is_still_rejected() {
        let mut pdf = Document::with_version("1.7");
        let root = pdf.new_object_id();
        let pages = pdf.add_object(
            dictionary! {"Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0},
        );
        let mut nested = Object::Null;
        for _ in 0..40 {
            nested = Object::Array(vec![nested]);
        }
        pdf.objects.insert(
            root,
            dictionary! {"Type" => "Catalog", "Pages" => pages, "Deep" => nested}.into(),
        );
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        let result = scan_bytes(bytes);
        assert_eq!(result["status"], "unresolved");
        assert_eq!(result["reason"], "metadata traversal depth limit");
    }

    #[test]
    fn aliases_and_cycles_are_visited_once() {
        let mut pdf = Document::with_version("1.7");
        let root = pdf.new_object_id();
        let pages = pdf.add_object(
            dictionary! {"Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0},
        );
        let shared = pdf.add_object(
            dictionary! {"Type" => "StructElem", "S" => "P", "ActualText" => Object::string_literal("shared")},
        );
        let cycle = pdf.add_object(dictionary! {});
        pdf.objects.insert(
            cycle,
            dictionary! {"Type" => "StructElem", "S" => "P", "Self" => cycle}.into(),
        );
        pdf.objects.insert(
            root,
            dictionary! {"Type" => "Catalog", "Pages" => pages, "AliasA" => shared, "AliasB" => shared, "Cycle" => cycle}.into(),
        );
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        let result = scan_bytes(bytes);
        assert_eq!(result["status"], "scanned", "{result}");
        assert_eq!(result["declarations"].as_array().unwrap().len(), 1);
    }

    /// An alias object whose own content is a reference, and the terminal it
    /// names, are both queued; the terminal dictionary and Form must expand
    /// exactly once in either encounter order.
    fn alias_fixture(alias_first: bool) -> Vec<u8> {
        let mut pdf = Document::with_version("1.7");
        let root = pdf.new_object_id();
        let pages = pdf.add_object(
            dictionary! {"Type" => "Pages", "Kids" => Object::Array(vec![]), "Count" => 0},
        );
        let shared = pdf.add_object(
            dictionary! {"Type" => "StructElem", "S" => "P", "ActualText" => Object::string_literal("shared")},
        );
        let alias = pdf.add_object(Object::Reference(shared));
        let form = pdf.add_object(Stream::new(
            dictionary! {"Type" => "XObject", "Subtype" => "Form", "BBox" => vec![0.into(), 0.into(), 10.into(), 10.into()]},
            b"0 0 1 1 re f".to_vec(),
        ));
        let form_alias = pdf.add_object(Object::Reference(form));
        let mut catalog = dictionary! {"Type" => "Catalog", "Pages" => pages};
        if alias_first {
            catalog.set("A_alias", alias);
            catalog.set("B_shared", shared);
            catalog.set("C_form_alias", form_alias);
            catalog.set("D_form", form);
        } else {
            catalog.set("A_shared", shared);
            catalog.set("B_alias", alias);
            catalog.set("C_form", form);
            catalog.set("D_form_alias", form_alias);
        }
        pdf.objects.insert(root, catalog.into());
        pdf.trailer.set("Root", root);
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn alias_objects_and_their_terminals_expand_once_in_both_orders() {
        for alias_first in [true, false] {
            let result = scan_bytes(alias_fixture(alias_first));
            assert_eq!(result["status"], "scanned", "{result}");
            assert_eq!(
                result["declarations"].as_array().unwrap().len(),
                1,
                "alias_first={alias_first}"
            );
            assert_eq!(
                result["form_programs"].as_array().unwrap().len(),
                1,
                "alias_first={alias_first}"
            );
        }
    }

    #[test]
    fn explicit_limits_stop_the_same_traversal_with_partial_state() {
        let pdf = LopdfParser
            .parse(Arc::from(fixture(false)), ParseLimits::default())
            .unwrap();
        let visits = scan_with_limits(pdf.as_ref(), 2, MAX_PAGE_BYTES);
        assert_eq!(visits["status"], "unresolved");
        assert_eq!(visits["reason"], "metadata visit limit");
        let bytes = scan_with_limits(pdf.as_ref(), MAX_RESOURCE_VISITS, 4);
        assert_eq!(bytes["status"], "unresolved");
        assert_eq!(bytes["reason"], "metadata/form byte limit");
    }
}
