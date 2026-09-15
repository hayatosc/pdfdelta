//! Observe font-width resources requested by native extraction without changing limits.
//! Resource access is not proof that every declared font or width is used by a glyph.

use pdfdelta_core::{
    pdf::{
        DecodedStream, LopdfParser, ObjectRef, PageRef, ParseLimits, ParsedPage, ParsedPdf,
        PdfDict, PdfIssue, PdfObject, PdfParser, PdfVersion, RawStream,
    },
    source::{ContentStreamGlyphExtractor, ExtractionLimits, GlyphExtractor},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    env,
    fs::File,
    io::Read,
    sync::{Arc, Mutex},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Probe {
    inner: Box<dyn ParsedPdf>,
    seen: Mutex<HashSet<ObjectRef>>,
    fonts: Mutex<Vec<Value>>,
}

impl Probe {
    fn resolved(&self, mut value: PdfObject) -> Option<PdfObject> {
        for _ in 0..128 {
            match value {
                PdfObject::Reference(reference) => value = self.inner.resolve(reference).ok()?,
                _ => return Some(value),
            }
        }
        None
    }

    fn observe(&self, reference: ObjectRef, object: &PdfObject) {
        let PdfObject::Dictionary(dict) = object else {
            return;
        };
        let Some(PdfObject::Name(kind)) = dict.get(b"Subtype".as_slice()) else {
            return;
        };
        if !matches!(
            kind.as_slice(),
            b"Type0" | b"CIDFontType0" | b"CIDFontType2"
        ) {
            return;
        }
        let mut seen = self.seen.lock().expect("single-thread diagnostic");
        if seen.len() >= 4096 || !seen.insert(reference) {
            return;
        }
        let mut row = json!({"reference": reference, "subtype": String::from_utf8_lossy(kind)});
        if let Some(PdfObject::Array(children)) = dict
            .get(b"DescendantFonts".as_slice())
            .cloned()
            .and_then(|v| self.resolved(v))
        {
            row["descendants"] = json!(
                children
                    .iter()
                    .filter_map(|child| match child {
                        PdfObject::Reference(reference) => Some(reference),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            );
        }
        if let Some(value) = dict.get(b"W".as_slice()) {
            row["width_reference"] = match value {
                PdfObject::Reference(reference) => json!(reference),
                _ => Value::Null,
            };
            if let Some(PdfObject::Array(widths)) = self.resolved(value.clone()) {
                row["program_items"] = json!(widths.len());
                let mut cursor = 0;
                let mut expanded = 0u64;
                let mut groups = 0u64;
                let mut uniform = 0u64;
                while cursor < widths.len() && groups < 65536 {
                    let Some(PdfObject::Integer(start)) = self.resolved(widths[cursor].clone())
                    else {
                        break;
                    };
                    let Some(next) = widths
                        .get(cursor + 1)
                        .cloned()
                        .and_then(|v| self.resolved(v))
                    else {
                        break;
                    };
                    match next {
                        PdfObject::Array(values) => {
                            expanded += values.len() as u64;
                            cursor += 2;
                        }
                        PdfObject::Integer(end)
                            if end >= start
                                && (0..=65535).contains(&start)
                                && end <= 65535
                                && cursor + 2 < widths.len() =>
                        {
                            expanded += (end - start + 1) as u64;
                            uniform += (end - start + 1) as u64;
                            cursor += 3;
                        }
                        _ => break,
                    }
                    groups += 1;
                }
                row["expanded_cids"] = json!(expanded);
                row["uniform_range_cids"] = json!(uniform);
                row["groups"] = json!(groups);
                row["program_count_finished"] = json!(cursor == widths.len());
            }
        }
        self.fonts
            .lock()
            .expect("single-thread diagnostic")
            .push(row);
    }
}

impl ParsedPdf for Probe {
    fn version(&self) -> PdfVersion {
        self.inner.version()
    }
    fn trailer(&self) -> pdfdelta_core::Result<PdfDict> {
        self.inner.trailer()
    }
    fn resolve(&self, reference: ObjectRef) -> pdfdelta_core::Result<PdfObject> {
        let object = self.inner.resolve(reference)?;
        self.observe(reference, &object);
        Ok(object)
    }
    fn terminal_reference(&self, reference: ObjectRef) -> pdfdelta_core::Result<ObjectRef> {
        self.inner.terminal_reference(reference)
    }
    fn pages(&self) -> pdfdelta_core::Result<Vec<PageRef>> {
        self.inner.pages()
    }
    fn page_dict(&self, page: PageRef) -> pdfdelta_core::Result<PdfDict> {
        self.inner.page_dict(page)
    }
    fn page_snapshot(&self, page: PageRef) -> pdfdelta_core::Result<ParsedPage> {
        self.inner.page_snapshot(page)
    }
    fn raw_stream(&self, reference: ObjectRef) -> pdfdelta_core::Result<RawStream> {
        self.inner.raw_stream(reference)
    }
    fn decoded_stream(&self, reference: ObjectRef) -> pdfdelta_core::Result<DecodedStream> {
        self.inner.decoded_stream(reference)
    }
    fn issues(&self) -> &[PdfIssue] {
        self.inner.issues()
    }
}

fn main() -> Result<()> {
    let input = env::args().nth(1).ok_or("usage: cid_width_probe PDF")?;
    let limits = ParseLimits::default();
    let mut bytes = Vec::new();
    File::open(&input)?
        .take(limits.max_input_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limits.max_input_bytes {
        return Err("input limit exceeded".into());
    }
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let probe = Probe {
        inner: LopdfParser.parse(Arc::from(bytes), limits)?,
        seen: Mutex::new(HashSet::new()),
        fonts: Mutex::new(Vec::new()),
    };
    let result = ContentStreamGlyphExtractor.extract_outcome(&probe, ExtractionLimits::default());
    let outcome = match result {
        Ok(value) => {
            json!({"glyphs": value.document().items().len(), "complete": value.is_complete(), "issues": format!("{:?}", value.issues())})
        }
        Err(error) => json!({"error": error.to_string()}),
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"input": input,
        "sha256": digest, "outcome": outcome, "fonts": probe.fonts.into_inner().expect("single-thread diagnostic")}))?
    );
    Ok(())
}
