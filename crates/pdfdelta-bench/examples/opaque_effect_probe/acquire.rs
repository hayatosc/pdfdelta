//! Neutral parser facade acquisition; backend operation types stay in this adapter.

use super::profile::{Closure, Entry, Issue, Operation, PROFILE, Page, Value};
use pdfdelta_core::pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_VISITS: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_OPERATIONS: usize = 100_000;

type Checked<T> = Result<T, Issue>;

#[derive(Serialize)]
pub struct Capture {
    pub input_sha256: String,
    pub issues: Vec<Issue>,
    pub pages: Vec<Page>,
}

fn issue(dependency: &str, location: &str, reason: impl ToString) -> Issue {
    Issue {
        dependency: dependency.into(),
        location: location.into(),
        reason: reason.to_string(),
    }
}

struct Reader<'a> {
    pdf: &'a dyn ParsedPdf,
    bytes: usize,
    visits: usize,
    active: BTreeSet<(u32, u16)>,
    sources: Vec<ObjectRef>,
}

impl Reader<'_> {
    fn charge(&mut self, bytes: usize, location: &str) -> Checked<()> {
        self.bytes = self.bytes.saturating_add(bytes);
        self.visits = self.visits.saturating_add(1);
        if self.bytes > MAX_BYTES || self.visits > MAX_VISITS {
            return Err(issue(
                "resource_limit",
                location,
                "expanded closure byte/work limit",
            ));
        }
        Ok(())
    }

    fn expand(&mut self, object: &PdfObject, depth: usize, location: &str) -> Checked<Value> {
        self.charge(0, location)?;
        if depth > MAX_DEPTH {
            return Err(issue("resource_limit", location, "dependency depth limit"));
        }
        Ok(match object {
            PdfObject::Null => Value::Null,
            PdfObject::Boolean(value) => Value::Boolean(*value),
            PdfObject::Integer(value) if value.unsigned_abs() <= 16_777_216 => {
                Value::Integer(*value)
            }
            PdfObject::Real(_) => {
                return Err(issue(
                    "exact_numeric_dependency",
                    location,
                    "real-valued dictionaries lack lexical numeric provenance in this facade; the profile only admits bounded integers",
                ));
            }
            PdfObject::Name(value) => {
                self.charge(value.len(), location)?;
                Value::Name(value.clone())
            }
            PdfObject::String(value) => {
                self.charge(value.len(), location)?;
                Value::String(value.clone())
            }
            PdfObject::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| self.expand(value, depth + 1, location))
                    .collect::<Checked<_>>()?,
            ),
            PdfObject::Dictionary(values) => {
                Value::Dictionary(self.dictionary(values, depth + 1, location)?)
            }
            PdfObject::Reference(reference) => {
                let key = (reference.object_number, reference.generation);
                if !self.active.insert(key) {
                    return Err(issue(
                        "resource_cycle",
                        location,
                        "cyclic resource dependency",
                    ));
                }
                self.sources.push(*reference);
                let resolved = self
                    .pdf
                    .resolve_with_terminal(*reference)
                    .map_err(|error| issue("resolved_resources", location, error))?;
                self.sources.push(resolved.reference);
                let result = if matches!(resolved.object, PdfObject::Stream(_)) {
                    let decoded = self
                        .pdf
                        .decoded_stream(resolved.reference)
                        .map_err(|error| issue("resolved_stream", location, error))?;
                    self.charge(decoded.bytes.len(), location)?;
                    Value::Stream {
                        dictionary: self.dictionary(&decoded.dictionary, depth + 1, location)?,
                        bytes: decoded.bytes,
                    }
                } else {
                    self.expand(&resolved.object, depth + 1, location)?
                };
                self.active.remove(&key);
                result
            }
            _ => {
                return Err(issue(
                    "resolved_resources",
                    location,
                    "out-of-profile numeric value or direct stream without source reference",
                ));
            }
        })
    }

    fn dictionary(
        &mut self,
        dictionary: &PdfDict,
        depth: usize,
        location: &str,
    ) -> Checked<BTreeMap<String, Value>> {
        dictionary
            .iter()
            .map(|(key, value)| {
                self.charge(key.len(), location)?;
                let key = String::from_utf8(key.clone()).map_err(|_| {
                    issue(
                        "resource_name",
                        location,
                        "non-UTF-8 dictionary key outside profile",
                    )
                })?;
                Ok((
                    key.clone(),
                    self.expand(value, depth, &format!("{location}/{key}"))?,
                ))
            })
            .collect()
    }

    fn contents(&mut self, object: &PdfObject, depth: usize, bytes: &mut Vec<u8>) -> Checked<()> {
        self.charge(0, "page contents")?;
        if depth > MAX_DEPTH {
            return Err(issue(
                "resource_limit",
                "page contents",
                "content depth limit",
            ));
        }
        match object {
            PdfObject::Array(items) => {
                for item in items {
                    self.contents(item, depth + 1, bytes)?;
                }
            }
            PdfObject::Reference(reference) => {
                let key = (reference.object_number, reference.generation);
                if !self.active.insert(key) {
                    return Err(issue(
                        "resource_cycle",
                        "page contents",
                        "cyclic content reference",
                    ));
                }
                self.sources.push(*reference);
                let resolved = self
                    .pdf
                    .resolve_with_terminal(*reference)
                    .map_err(|error| issue("commands", "page contents", error))?;
                self.sources.push(resolved.reference);
                if matches!(resolved.object, PdfObject::Stream(_)) {
                    let decoded = self
                        .pdf
                        .decoded_stream(resolved.reference)
                        .map_err(|error| issue("commands", "page contents", error))?;
                    self.charge(decoded.bytes.len().saturating_add(1), "page contents")?;
                    bytes.extend(decoded.bytes);
                    bytes.push(b'\n');
                } else {
                    self.contents(&resolved.object, depth + 1, bytes)?;
                }
                self.active.remove(&key);
            }
            _ => {
                return Err(issue(
                    "commands",
                    "page contents",
                    "missing or unsupported content source",
                ));
            }
        }
        Ok(())
    }
}

fn dictionary(value: &Value) -> Option<&BTreeMap<String, Value>> {
    match value {
        Value::Dictionary(value) => Some(value),
        _ => None,
    }
}

fn name(value: Option<&Value>, expected: &[u8]) -> bool {
    matches!(value, Some(Value::Name(value)) if value == expected)
}

fn number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::Real(value) if value.is_finite() => Some(*value),
        _ => None,
    }
}

fn numbers(values: &[Value], count: usize) -> bool {
    values.len() == count && values.iter().all(|value| number(value).is_some())
}

fn allowed_keys(values: &BTreeMap<String, Value>, allowed: &[&str], location: &str) -> Checked<()> {
    if let Some(key) = values.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(issue(
            "unsupported_dependency",
            location,
            format!("/{key} is outside the profile"),
        ));
    }
    Ok(())
}

fn array(values: &BTreeMap<String, Value>, key: &str, count: usize, location: &str) -> Checked<()> {
    if !matches!(values.get(key), Some(Value::Array(values)) if numbers(values, count)) {
        return Err(issue(
            "geometry",
            location,
            format!("/{key} requires {count} finite numbers"),
        ));
    }
    Ok(())
}

fn resources(value: &Value, location: &str, depth: usize, operations: &mut usize) -> Checked<()> {
    if depth > MAX_DEPTH {
        return Err(issue(
            "resource_limit",
            location,
            "nested Form validation depth",
        ));
    }
    let values = dictionary(value).ok_or_else(|| {
        issue(
            "resolved_resources",
            location,
            "resources must be a dictionary",
        )
    })?;
    allowed_keys(values, &["XObject", "ExtGState", "ProcSet"], location)?;
    if let Some(states) = values.get("ExtGState") {
        let states = dictionary(states)
            .ok_or_else(|| issue("graphics_state", location, "invalid ExtGState dictionary"))?;
        for (key, state) in states {
            let location = format!("{location}/ExtGState/{key}");
            let state = dictionary(state)
                .ok_or_else(|| issue("graphics_state", &location, "invalid state"))?;
            allowed_keys(state, &["Type", "ca", "CA", "BM", "SMask"], &location)?;
            for key in ["ca", "CA"] {
                if state.get(key).is_some_and(|value| {
                    number(value).is_none_or(|value| !(0.0..=1.0).contains(&value))
                }) {
                    return Err(issue(
                        "opacity",
                        &location,
                        "alpha must be a known constant in [0, 1]",
                    ));
                }
            }
            if state.contains_key("BM") && !name(state.get("BM"), b"Normal") {
                return Err(issue("blend", &location, "only Normal blend is supported"));
            }
            if state.contains_key("SMask") && !name(state.get("SMask"), b"None") {
                return Err(issue(
                    "mask",
                    &location,
                    "soft masks require an unsupported dependency closure",
                ));
            }
        }
    }
    if let Some(objects) = values.get("XObject") {
        let objects = dictionary(objects)
            .ok_or_else(|| issue("resolved_resources", location, "invalid XObject dictionary"))?;
        for (key, value) in objects {
            let location = format!("{location}/XObject/{key}");
            let Value::Stream {
                dictionary: values,
                bytes,
            } = value
            else {
                return Err(issue(
                    "resolved_resources",
                    &location,
                    "XObject is not a resolved stream",
                ));
            };
            if name(values.get("Subtype"), b"Image") {
                allowed_keys(
                    values,
                    &[
                        "Type",
                        "Subtype",
                        "Width",
                        "Height",
                        "ColorSpace",
                        "BitsPerComponent",
                        "Interpolate",
                        "Length",
                        "Filter",
                        "DecodeParms",
                    ],
                    &location,
                )?;
                if !name(values.get("ColorSpace"), b"DeviceRGB")
                    || values.get("BitsPerComponent") != Some(&Value::Integer(8))
                {
                    return Err(issue(
                        "image_samples",
                        &location,
                        "only explicit 8-bit DeviceRGB samples are supported",
                    ));
                }
                let size = |key| match values.get(key) {
                    Some(Value::Integer(value)) if *value > 0 => usize::try_from(*value).ok(),
                    _ => None,
                };
                let expected = size("Width")
                    .and_then(|width| size("Height").and_then(|height| width.checked_mul(height)))
                    .and_then(|pixels| pixels.checked_mul(3));
                if expected != Some(bytes.len()) {
                    return Err(issue(
                        "image_samples",
                        &location,
                        "sample dimensions do not exactly cover decoded bytes",
                    ));
                }
                if values
                    .get("Interpolate")
                    .is_some_and(|value| !matches!(value, Value::Boolean(_)))
                {
                    return Err(issue(
                        "image_samples",
                        &location,
                        "invalid interpolation state",
                    ));
                }
            } else if name(values.get("Subtype"), b"Form") {
                allowed_keys(
                    values,
                    &[
                        "Type",
                        "Subtype",
                        "FormType",
                        "BBox",
                        "Matrix",
                        "Resources",
                        "Length",
                        "Filter",
                        "DecodeParms",
                    ],
                    &location,
                )?;
                array(values, "BBox", 4, &location)?;
                if values.contains_key("Matrix") {
                    array(values, "Matrix", 6, &location)?;
                }
                if values
                    .get("FormType")
                    .is_some_and(|value| value != &Value::Integer(1))
                {
                    return Err(issue("form", &location, "unsupported FormType"));
                }
                let nested = values.get("Resources").ok_or_else(|| {
                    issue(
                        "nested_invocation",
                        &location,
                        "Form must declare its complete resources explicitly",
                    )
                })?;
                resources(nested, &location, depth + 1, operations)?;
                program(bytes, nested, &location, operations)?;
            } else {
                return Err(issue(
                    "resolved_resources",
                    &location,
                    "unsupported XObject subtype",
                ));
            }
        }
    }
    Ok(())
}

/// Backend content syntax is decoded by the pinned parser, never a custom PDF
/// object parser. The whitelist refuses text and stateful execution extensions.
fn program(
    bytes: &[u8],
    resources: &Value,
    location: &str,
    total: &mut usize,
) -> Checked<Vec<Operation>> {
    let content = lopdf::content::Content::decode_strict(bytes)
        .map_err(|error| issue("commands", location, error))?;
    *total = total.saturating_add(content.operations.len());
    if *total > MAX_OPERATIONS {
        return Err(issue("resource_limit", location, "operator limit"));
    }
    let resources = dictionary(resources)
        .ok_or_else(|| issue("resolved_resources", location, "missing resources"))?;
    let mut stack = 0usize;
    let mut path = false;
    let mut clip = false;
    let mut result = Vec::new();
    for (index, operation) in content.operations.into_iter().enumerate() {
        let location = format!("{location}/operator[{index}] {}", operation.operator);
        let operands: Vec<_> = operation
            .operands
            .iter()
            .map(|operand| match operand {
                lopdf::Object::Integer(value) => Ok(Value::Integer(*value)),
                lopdf::Object::Real(value) if value.is_finite() => {
                    Ok(Value::Real(f64::from(*value)))
                }
                lopdf::Object::Name(value) => Ok(Value::Name(value.clone())),
                _ => Err(issue(
                    "operands",
                    &location,
                    "operand type outside primitive profile",
                )),
            })
            .collect::<Checked<_>>()?;
        let valid = match operation.operator.as_str() {
            "q" if !path && operands.is_empty() => {
                stack += 1;
                stack <= 64
            }
            "Q" if !path && operands.is_empty() && stack > 0 => {
                stack -= 1;
                true
            }
            "cm" => numbers(&operands, 6) && !path,
            "rg" => {
                numbers(&operands, 3)
                    && operands.iter().all(|value| {
                        number(value).is_some_and(|value| (0.0..=1.0).contains(&value))
                    })
            }
            "g" => {
                numbers(&operands, 1)
                    && number(&operands[0]).is_some_and(|value| (0.0..=1.0).contains(&value))
            }
            "re" if !clip => {
                let valid = numbers(&operands, 4);
                path |= valid;
                valid
            }
            "W" | "W*" if path && !clip && operands.is_empty() => {
                clip = true;
                true
            }
            "n" | "f" | "f*" if path && operands.is_empty() => {
                path = false;
                clip = false;
                true
            }
            "Do" | "gs" if !path => {
                let [Value::Name(key)] = operands.as_slice() else {
                    return Err(issue(
                        "operands",
                        &location,
                        "resource command needs one name",
                    ));
                };
                let group = if operation.operator == "Do" {
                    "XObject"
                } else {
                    "ExtGState"
                };
                let key = std::str::from_utf8(key)
                    .map_err(|_| issue("resource_name", &location, "non-UTF-8 resource name"))?;
                resources
                    .get(group)
                    .and_then(dictionary)
                    .is_some_and(|values| values.contains_key(key))
            }
            _ => false,
        };
        if !valid {
            return Err(issue(
                "commands_and_entry_state",
                &location,
                "unsupported operator, missing resource, invalid operands or state transition",
            ));
        }
        result.push(Operation {
            operator: operation.operator,
            operands,
        });
    }
    if stack != 0 || path || clip {
        return Err(issue(
            "commands_and_exit_state",
            location,
            "unbalanced graphics state or unfinished path",
        ));
    }
    Ok(result)
}

fn catalog_issues(pdf: &dyn ParsedPdf) -> Checked<()> {
    let trailer = pdf
        .trailer()
        .map_err(|error| issue("catalog", "trailer", error))?;
    let Some(PdfObject::Reference(root)) = trailer.get(b"Root".as_slice()) else {
        return Err(issue("catalog", "trailer", "missing catalog reference"));
    };
    let PdfObject::Dictionary(catalog) = pdf
        .resolve(*root)
        .map_err(|error| issue("catalog", "Root", error))?
    else {
        return Err(issue("catalog", "Root", "invalid catalog"));
    };
    for key in [
        b"OCProperties".as_slice(),
        b"OutputIntents",
        b"AcroForm",
        b"OpenAction",
        b"AA",
        b"Requirements",
        b"Extensions",
    ] {
        if catalog.contains_key(key) {
            return Err(issue(
                "catalog_execution",
                "Root",
                format!(
                    "/{} requires a dependency outside the profile",
                    String::from_utf8_lossy(key)
                ),
            ));
        }
    }
    Ok(())
}

pub fn capture(pdf: &dyn ParsedPdf, input_sha256: &str) -> Capture {
    let mut capture = Capture {
        input_sha256: input_sha256.into(),
        issues: Vec::new(),
        pages: Vec::new(),
    };
    for source in pdf.issues() {
        capture
            .issues
            .push(issue("acquisition", "document", source.description()));
    }
    if let Err(error) = catalog_issues(pdf) {
        capture.issues.push(error);
    }
    let pages = match pdf.pages() {
        Ok(pages) if pages.len() <= 200 => pages,
        Ok(_) => {
            capture
                .issues
                .push(issue("resource_limit", "pages", "200-page probe limit"));
            return capture;
        }
        Err(error) => {
            capture.issues.push(issue("acquisition", "pages", error));
            return capture;
        }
    };
    for (index, reference) in pages.into_iter().enumerate() {
        let mut reader = Reader {
            pdf,
            bytes: 0,
            visits: 0,
            active: BTreeSet::new(),
            sources: vec![reference.0],
        };
        let mut page = Page {
            input_sha256: input_sha256.into(),
            page: index,
            source_object: reference.0,
            sources: Vec::new(),
            issues: capture.issues.clone(),
            expanded_bytes: 0,
            visits: 0,
            closure: None,
            closure_sha256: None,
        };
        let outcome = (|| -> Checked<Closure> {
            let snapshot = pdf
                .page_snapshot(reference)
                .map_err(|error| issue("acquisition", "page snapshot", error))?;
            let resources = reader.expand(
                snapshot
                    .resources
                    .as_deref()
                    .unwrap_or(&PdfObject::Dictionary(BTreeMap::new())),
                0,
                "page resources",
            )?;
            let mut page_dictionary = snapshot.dictionary;
            let contents = page_dictionary
                .remove(b"Contents".as_slice())
                .ok_or_else(|| issue("commands", "page", "missing content acquisition"))?;
            page_dictionary.remove(b"Parent".as_slice());
            let page_dictionary = reader.dictionary(&page_dictionary, 0, "page")?;
            if let Err(error) = allowed_keys(
                &page_dictionary,
                &["Type", "MediaBox", "CropBox", "Rotate", "UserUnit"],
                "page",
            ) {
                page.issues.push(error);
            }
            if let Err(error) = array(&page_dictionary, "MediaBox", 4, "page") {
                page.issues.push(error);
            }
            if page_dictionary.contains_key("CropBox")
                && let Err(error) = array(&page_dictionary, "CropBox", 4, "page")
            {
                page.issues.push(error);
            }
            if page_dictionary
                .get("Rotate")
                .is_some_and(|value| !matches!(value, Value::Integer(value) if value % 90 == 0))
            {
                page.issues.push(issue(
                    "page_transform",
                    "Rotate",
                    "rotation must be an integer multiple of 90",
                ));
            }
            if page_dictionary
                .get("UserUnit")
                .is_some_and(|value| number(value).is_none_or(|value| value <= 0.0))
            {
                page.issues.push(issue(
                    "page_transform",
                    "UserUnit",
                    "user unit must be positive and finite",
                ));
            }
            let mut operations = 0;
            if let Err(error) = self::resources(&resources, "page resources", 0, &mut operations) {
                page.issues.push(error);
            }
            let mut bytes = Vec::new();
            reader.contents(&contents, 0, &mut bytes)?;
            let commands = program(&bytes, &resources, "page contents", &mut operations)?;
            Ok(Closure {
                profile: PROFILE.into(),
                backend:
                    "LopdfParser@1e3d646ca249ebf1a6ff479278c07e9c0f9377a8; strict-content-decoder"
                        .into(),
                entry: Entry::default(),
                page: page_dictionary,
                resources,
                commands,
                command_bytes: bytes,
            })
        })();
        match outcome {
            Ok(closure) if page.issues.is_empty() => match serde_json::to_vec(&closure) {
                Ok(bytes) => {
                    page.closure_sha256 = Some(super::digest(&bytes));
                    page.closure = Some(closure);
                }
                Err(error) => page.issues.push(issue("serialization", "closure", error)),
            },
            Ok(_) => {}
            Err(error) => page.issues.push(error),
        }
        reader
            .sources
            .sort_by_key(|source| (source.object_number, source.generation));
        reader.sources.dedup();
        page.sources = reader.sources;
        page.expanded_bytes = reader.bytes;
        page.visits = reader.visits;
        capture.pages.push(page);
    }
    capture
}
