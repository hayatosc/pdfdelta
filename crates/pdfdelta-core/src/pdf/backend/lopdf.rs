use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use lopdf::{Dictionary, Document, LoadOptions, Object};

use crate::{
    Error, Result,
    pdf::{
        DecodedStream, ObjectRef, PageRef, ParseLimits, ParsedPage, ParsedPdf, PdfDict, PdfIssue,
        PdfObject, PdfParser, PdfVersion, RawStream, ResolvedObject,
    },
};

const INHERITABLE_PAGE_KEYS: [&[u8]; 3] = [b"MediaBox", b"CropBox", b"Rotate"];

static LOPDF_PARSE_LOCK: Mutex<()> = Mutex::new(());
static LOAD_OBJECT_BUDGET: Mutex<Option<ObjectBudget>> = Mutex::new(None);

struct ObjectBudget {
    limits: ParseLimits,
    remaining_objects: usize,
    remaining_decoded_bytes: usize,
    reserved_embedded: HashMap<lopdf::ObjectId, usize>,
    error: Option<Error>,
}

struct ValidatedObjectStream {
    embedded: Vec<lopdf::ObjectId>,
    decoded_bytes: usize,
}

/// A [`PdfParser`] backed by `lopdf`.
///
/// Parsing is serialized process-wide. The backend's load filter is a bare
/// function pointer without per-call state, so this parser installs the active
/// object budget globally and holds a global lock for the entire load. The
/// parser remains `Send + Sync`, but concurrent parse calls do not load PDFs in
/// parallel.
///
/// Object-stream-heavy inputs also pay for repeated decompression passes: this
/// parser first decodes each object stream to validate and charge its index,
/// then the backend decodes it again while materializing objects, and every
/// `FlateDecode` stage is additionally re-validated strictly before either
/// decode is accepted.
#[derive(Clone, Copy, Debug, Default)]
pub struct LopdfParser;

impl LopdfParser {
    pub const NAME: &str = "lopdf";

    fn parse_inner(
        &self,
        pdf: Arc<[u8]>,
        limits: ParseLimits,
        password: Option<&str>,
    ) -> Result<Box<dyn ParsedPdf>> {
        validate_limits(limits)?;
        if pdf.len() > limits.max_input_bytes {
            return Err(limit_error("PDF input bytes", limits.max_input_bytes));
        }

        let parser_lock = lock_unpoisoned(&LOPDF_PARSE_LOCK);
        let budget = ObjectBudgetGuard::install(limits);
        let mut options = LoadOptions::with_max_decompressed_size(limits.max_decoded_stream_bytes);
        options.password = password.map(str::to_owned);
        options.filter = Some(limit_loaded_objects);
        let loaded = Document::load_mem_with_options(&pdf, options);
        let budget_error = budget.error();
        drop(budget);
        drop(parser_lock);
        if let Some(error) = budget_error {
            return Err(error);
        }
        let document = loaded.map_err(|error| map_lopdf_error(error, "parsing PDF", limits))?;

        if document.reference_table.entries.len() > limits.max_objects {
            return Err(limit_error(
                "PDF cross-reference entries",
                limits.max_objects,
            ));
        }
        if document.is_encrypted() {
            return Err(Error::Unsupported(
                "PDF password is missing, invalid, or unsupported".into(),
            ));
        }
        if document.objects.len() > limits.max_objects {
            return Err(limit_error("PDF object count", limits.max_objects));
        }

        let version = effective_version(&document, limits)?;
        let page_tree = collect_pages(&document, limits)?;
        let page_index = page_tree.pages.iter().copied().collect();

        Ok(Box::new(LopdfParsedPdf {
            document,
            limits,
            pages: page_tree.pages,
            page_index,
            page_parents: page_tree.parents,
            issues: page_tree.issues,
            resource_cache: Mutex::new(HashMap::new()),
            version,
        }))
    }
}

impl PdfParser for LopdfParser {
    fn parse(&self, pdf: Arc<[u8]>, limits: ParseLimits) -> Result<Box<dyn ParsedPdf>> {
        self.parse_inner(pdf, limits, None)
    }

    fn parse_with_password(
        &self,
        pdf: Arc<[u8]>,
        limits: ParseLimits,
        password: &str,
    ) -> Result<Box<dyn ParsedPdf>> {
        self.parse_inner(pdf, limits, Some(password))
    }
}

struct LopdfParsedPdf {
    document: Document,
    limits: ParseLimits,
    pages: Vec<PageRef>,
    page_index: HashSet<PageRef>,
    page_parents: HashMap<lopdf::ObjectId, Option<lopdf::ObjectId>>,
    issues: Vec<PdfIssue>,
    resource_cache: Mutex<HashMap<lopdf::ObjectId, Arc<PdfObject>>>,
    version: PdfVersion,
}

impl LopdfParsedPdf {
    fn resolve_object(&self, reference: ObjectRef) -> Result<&Object> {
        self.resolve_object_with_id(reference)
            .map(|(_, object)| object)
    }

    fn resolve_object_with_id(&self, reference: ObjectRef) -> Result<(lopdf::ObjectId, &Object)> {
        let mut current = to_lopdf_id(reference);
        let mut seen = HashSet::new();

        for _ in 0..=self.limits.max_recursion_depth {
            if !seen.insert(current) {
                return Err(Error::Backend(format!(
                    "resolving object {} {}: reference cycle",
                    reference.object_number, reference.generation
                )));
            }

            let object = self.document.objects.get(&current).ok_or_else(|| {
                Error::Backend(format!(
                    "resolving object {} {}: object not found",
                    current.0, current.1
                ))
            })?;
            match object {
                Object::Reference(next) => current = *next,
                _ => return Ok((current, object)),
            }
        }

        Err(limit_error(
            "PDF reference depth",
            self.limits.max_recursion_depth,
        ))
    }

    fn object_dictionary(&self, reference: ObjectRef, context: &str) -> Result<&Dictionary> {
        self.resolve_object(reference)?
            .as_dict()
            .map_err(|error| map_lopdf_error(error, context, self.limits))
    }

    fn convert_dictionary(&self, dictionary: &Dictionary) -> Result<PdfDict> {
        convert_dictionary(dictionary, 0, self.limits.max_recursion_depth)
    }

    fn cached_resource(&self, owner: lopdf::ObjectId, resource: &Object) -> Result<Arc<PdfObject>> {
        let mut cache = lock_unpoisoned(&self.resource_cache);
        if let Some(cached) = cache.get(&owner) {
            return Ok(Arc::clone(cached));
        }

        let converted = Arc::new(convert_object(
            resource,
            1,
            self.limits.max_recursion_depth,
        )?);
        cache.insert(owner, Arc::clone(&converted));
        Ok(converted)
    }

    fn page_snapshot_with_inheritance(&self, page: PageRef) -> Result<ParsedPage> {
        if !self.page_index.contains(&page) {
            return Err(Error::Backend(format!(
                "reading page object {} {}: page is not in the page tree",
                page.0.object_number, page.0.generation
            )));
        }

        let page_dictionary = self.object_dictionary(page.0, "reading page dictionary")?;
        let mut converted = page_dictionary
            .iter()
            .filter(|(key, _)| key.as_slice() != b"Resources")
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    convert_object(value, 1, self.limits.max_recursion_depth)?,
                ))
            })
            .collect::<Result<PdfDict>>()?;
        let mut resources = page_dictionary
            .get(b"Resources")
            .ok()
            .map(|resource| self.cached_resource(to_lopdf_id(page.0), resource))
            .transpose()?;
        let mut parent = self
            .page_parents
            .get(&to_lopdf_id(page.0))
            .copied()
            .ok_or_else(|| Error::Backend("reading page parent: page tree entry missing".into()))?;

        for _ in 0..self.limits.max_recursion_depth {
            let Some(parent_reference) = parent else {
                return Ok(ParsedPage {
                    dictionary: converted,
                    resources,
                });
            };

            let parent_dictionary =
                self.object_dictionary(from_lopdf_id(parent_reference), "reading page parent")?;
            if resources.is_none()
                && let Ok(resource) = parent_dictionary.get(b"Resources")
            {
                resources = Some(self.cached_resource(parent_reference, resource)?);
            }
            for key in INHERITABLE_PAGE_KEYS {
                if converted.contains_key(key) {
                    continue;
                }
                if let Ok(value) = parent_dictionary.get(key) {
                    converted.insert(
                        key.to_vec(),
                        convert_object(value, 1, self.limits.max_recursion_depth)?,
                    );
                }
            }
            parent = self
                .page_parents
                .get(&parent_reference)
                .copied()
                .ok_or_else(|| {
                    Error::Backend(format!(
                        "reading page parent: page tree object {} {} is missing",
                        parent_reference.0, parent_reference.1
                    ))
                })?;
        }

        if parent.is_some() {
            return Err(limit_error(
                "PDF page inheritance depth",
                self.limits.max_recursion_depth,
            ));
        }
        Ok(ParsedPage {
            dictionary: converted,
            resources,
        })
    }
}

impl ParsedPdf for LopdfParsedPdf {
    fn version(&self) -> PdfVersion {
        self.version
    }

    fn trailer(&self) -> Result<PdfDict> {
        self.convert_dictionary(&self.document.trailer)
    }

    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject> {
        convert_object(
            self.resolve_object(reference)?,
            0,
            self.limits.max_recursion_depth,
        )
    }

    fn terminal_reference(&self, reference: ObjectRef) -> Result<ObjectRef> {
        self.resolve_object_with_id(reference)
            .map(|(terminal, _)| from_lopdf_id(terminal))
    }

    fn resolve_with_terminal(&self, reference: ObjectRef) -> Result<ResolvedObject> {
        let (terminal, object) = self.resolve_object_with_id(reference)?;
        Ok(ResolvedObject {
            reference: from_lopdf_id(terminal),
            object: convert_object(object, 0, self.limits.max_recursion_depth)?,
        })
    }

    fn pages(&self) -> Result<Vec<PageRef>> {
        Ok(self.pages.clone())
    }

    fn page_dict(&self, page: PageRef) -> Result<PdfDict> {
        let snapshot = self.page_snapshot_with_inheritance(page)?;
        let mut dictionary = snapshot.dictionary;
        if let Some(resources) = snapshot.resources {
            dictionary.insert(b"Resources".to_vec(), resources.as_ref().clone());
        }
        Ok(dictionary)
    }

    fn page_snapshot(&self, page: PageRef) -> Result<ParsedPage> {
        self.page_snapshot_with_inheritance(page)
    }

    fn raw_stream(&self, reference: ObjectRef) -> Result<RawStream> {
        let stream = self
            .resolve_object(reference)?
            .as_stream()
            .map_err(|error| map_lopdf_error(error, "reading raw stream", self.limits))?;
        Ok(RawStream {
            dictionary: self.convert_dictionary(&stream.dict)?,
            bytes: stream.content.clone(),
        })
    }

    fn decoded_stream(&self, reference: ObjectRef) -> Result<DecodedStream> {
        let stream = self
            .resolve_object(reference)?
            .as_stream()
            .map_err(|error| map_lopdf_error(error, "reading decoded stream", self.limits))?;
        let context = format!("{} {} R", reference.object_number, reference.generation);
        super::strict_flate::validate_stream_flate_stages(stream, self.limits, &context)?;
        let bytes = stream
            .get_plain_content_with_limit(self.limits.max_decoded_stream_bytes)
            .map_err(|error| map_lopdf_error(error, "decoding stream", self.limits))?;
        Ok(DecodedStream {
            dictionary: self.convert_dictionary(&stream.dict)?,
            bytes,
        })
    }

    fn issues(&self) -> &[PdfIssue] {
        &self.issues
    }
}

struct ObjectBudgetGuard {
    previous: Option<ObjectBudget>,
}

impl ObjectBudgetGuard {
    fn install(limits: ParseLimits) -> Self {
        let mut budget = lock_unpoisoned(&LOAD_OBJECT_BUDGET);
        let previous = budget.replace(ObjectBudget {
            limits,
            remaining_objects: limits.max_objects,
            remaining_decoded_bytes: limits.max_total_object_stream_bytes,
            reserved_embedded: HashMap::new(),
            error: None,
        });
        Self { previous }
    }

    fn error(&self) -> Option<Error> {
        lock_unpoisoned(&LOAD_OBJECT_BUDGET)
            .as_ref()
            .and_then(|state| state.error.clone())
    }
}

impl Drop for ObjectBudgetGuard {
    fn drop(&mut self) {
        *lock_unpoisoned(&LOAD_OBJECT_BUDGET) = self.previous.take();
    }
}

fn limit_loaded_objects(
    id: lopdf::ObjectId,
    object: &mut Object,
) -> Option<(lopdf::ObjectId, Object)> {
    let mut budget = lock_unpoisoned(&LOAD_OBJECT_BUDGET);
    let Some(state) = budget.as_mut() else {
        return Some((id, object.clone()));
    };
    if state.error.is_some() {
        return None;
    }

    if let Ok(stream) = object.as_stream()
        && stream.dict.has_type(b"ObjStm")
    {
        let count = match object_stream_entry_count(stream) {
            Ok(count) => count,
            Err(error) => {
                state.error = Some(error);
                return None;
            }
        };
        let Some(required) = count.checked_add(1) else {
            state.error = Some(limit_error("PDF object count", state.limits.max_objects));
            return None;
        };
        if required > state.remaining_objects {
            state.error = Some(limit_error("PDF object count", state.limits.max_objects));
            return None;
        }

        let context = format!("{} {} R", id.0, id.1);
        let validated = match validate_object_stream_index(stream, count, state.limits, &context) {
            Ok(validated) => validated,
            Err(error) => {
                state.error = Some(error);
                return None;
            }
        };
        if validated.decoded_bytes > state.remaining_decoded_bytes {
            state.error = Some(limit_error(
                "PDF decoded object stream bytes",
                state.limits.max_total_object_stream_bytes,
            ));
            return None;
        }
        state.remaining_objects -= required;
        state.remaining_decoded_bytes -= validated.decoded_bytes;
        for embedded_id in validated.embedded {
            *state.reserved_embedded.entry(embedded_id).or_default() += 1;
        }
        return Some((id, object.clone()));
    }

    if let Some(reservations) = state.reserved_embedded.get_mut(&id) {
        *reservations -= 1;
        if *reservations == 0 {
            state.reserved_embedded.remove(&id);
        }
        return Some((id, object.clone()));
    }
    if state.remaining_objects == 0 {
        state.error = Some(limit_error("PDF object count", state.limits.max_objects));
        return None;
    }

    state.remaining_objects -= 1;
    Some((id, object.clone()))
}

fn object_stream_entry_count(stream: &lopdf::Stream) -> Result<usize> {
    let count = stream
        .dict
        .get(b"N")
        .and_then(Object::as_i64)
        .map_err(|error| Error::Backend(format!("validating object stream N: {error}")))?;
    usize::try_from(count)
        .map_err(|_| Error::Backend("validating object stream: N is negative or too large".into()))
}

fn validate_object_stream_index(
    stream: &lopdf::Stream,
    count: usize,
    limits: ParseLimits,
    context: &str,
) -> Result<ValidatedObjectStream> {
    super::strict_flate::validate_stream_flate_stages(stream, limits, context)?;
    let first = stream
        .dict
        .get(b"First")
        .and_then(Object::as_i64)
        .map_err(|error| map_lopdf_error(error, "validating object stream First", limits))?;
    let first = usize::try_from(first).map_err(|_| {
        Error::Backend("validating object stream: First is negative or too large".into())
    })?;
    let content = stream
        .get_plain_content_with_limit(limits.max_decoded_stream_bytes)
        .map_err(|error| map_lopdf_error(error, "validating object stream", limits))?;
    let index = content
        .get(..first)
        .ok_or_else(|| Error::Backend("validating object stream: First is out of bounds".into()))?;
    let index = std::str::from_utf8(index)
        .map_err(|_| Error::Backend("validating object stream: index is not ASCII".into()))?;
    // QDF-style writers annotate the index with `%`-to-EOL comments. Strip
    // them so token pairing stays aligned with the declared `N` entries; the
    // backend drops pairs it cannot resolve, and charging still uses N, which
    // stays conservative whenever entries are dropped.
    let index = strip_comments(index);
    let tokens = index.split_ascii_whitespace().collect::<Vec<_>>();
    let mut embedded = HashSet::with_capacity(count);
    let mut resolved_pairs = 0_usize;
    for pair in tokens.as_chunks::<2>().0 {
        let (Ok(object_number), Ok(offset)) = (
            parse_object_stream_u32(pair[0], "object number"),
            parse_object_stream_u32(pair[1], "offset"),
        ) else {
            continue;
        };
        let Ok(offset) = usize::try_from(offset) else {
            continue;
        };
        let Some(object_offset) = first.checked_add(offset) else {
            continue;
        };
        if object_offset >= content.len() {
            continue;
        }
        if !embedded.insert((object_number, 0)) {
            return Err(Error::Backend(
                "validating object stream: repeated embedded object number".into(),
            ));
        }
        resolved_pairs += 1;
    }
    if resolved_pairs != count {
        return Err(Error::Backend(format!(
            "validating object stream: N declares {count} entries but the index resolves {resolved_pairs}"
        )));
    }

    Ok(ValidatedObjectStream {
        embedded: embedded.into_iter().collect(),
        decoded_bytes: content.len(),
    })
}

fn strip_comments(index: &str) -> String {
    let mut stripped = String::with_capacity(index.len());
    let mut in_comment = false;
    for character in index.chars() {
        match character {
            '%' => in_comment = true,
            '\r' | '\n' => {
                in_comment = false;
                stripped.push(character);
            }
            _ if !in_comment => stripped.push(character),
            _ => {}
        }
    }
    stripped
}

fn parse_object_stream_u32(token: &str, field: &str) -> Result<u32> {
    token.parse().map_err(|_| {
        Error::Backend(format!(
            "validating object stream: invalid {field} {token:?}"
        ))
    })
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn validate_limits(limits: ParseLimits) -> Result<()> {
    let values = [
        ("max_input_bytes", limits.max_input_bytes),
        ("max_objects", limits.max_objects),
        ("max_recursion_depth", limits.max_recursion_depth),
        ("max_decoded_stream_bytes", limits.max_decoded_stream_bytes),
        (
            "max_total_object_stream_bytes",
            limits.max_total_object_stream_bytes,
        ),
        ("max_pages", limits.max_pages),
    ];
    if let Some((name, _)) = values.into_iter().find(|(_, value)| *value == 0) {
        return Err(Error::InvalidConfiguration(format!(
            "{name} must be greater than zero"
        )));
    }
    Ok(())
}

fn parse_version(version: &str) -> Result<PdfVersion> {
    let (major, minor) = version
        .split_once('.')
        .ok_or_else(|| Error::Backend(format!("invalid PDF version {version:?}")))?;
    let major = major
        .parse()
        .map_err(|_| Error::Backend(format!("invalid PDF version {version:?}")))?;
    let minor = minor
        .parse()
        .map_err(|_| Error::Backend(format!("invalid PDF version {version:?}")))?;
    Ok(PdfVersion { major, minor })
}

fn effective_version(document: &Document, limits: ParseLimits) -> Result<PdfVersion> {
    let header = parse_version(&document.version)?;
    let root = required_reference(&document.trailer, b"Root", "reading trailer Root")?;
    let catalog = resolve_document_object(document, root, limits, "reading catalog")?
        .as_dict()
        .map_err(|error| map_lopdf_error(error, "reading catalog", limits))?;
    let catalog_version = match catalog.get(b"Version") {
        Ok(version) => {
            let version = version
                .as_name()
                .map_err(|error| map_lopdf_error(error, "reading catalog Version", limits))?;
            let version = std::str::from_utf8(version)
                .map_err(|_| Error::Backend("catalog Version is not ASCII".into()))?;
            parse_version(version)?
        }
        Err(lopdf::Error::DictKey(_)) => return Ok(header),
        Err(error) => return Err(map_lopdf_error(error, "reading catalog Version", limits)),
    };

    if (catalog_version.major, catalog_version.minor) > (header.major, header.minor) {
        Ok(catalog_version)
    } else {
        Ok(header)
    }
}

struct PageTree {
    pages: Vec<PageRef>,
    parents: HashMap<lopdf::ObjectId, Option<lopdf::ObjectId>>,
    issues: Vec<PdfIssue>,
}

enum PendingPageTreeEntry {
    Node {
        reference: lopdf::ObjectId,
        expected_parent: Option<lopdf::ObjectId>,
        depth: usize,
    },
    MalformedChild {
        parent: lopdf::ObjectId,
    },
}

fn collect_pages(document: &Document, limits: ParseLimits) -> Result<PageTree> {
    let root = required_reference(&document.trailer, b"Root", "reading trailer Root")?;
    let catalog = resolve_document_object(document, root, limits, "reading catalog")?
        .as_dict()
        .map_err(|error| map_lopdf_error(error, "reading catalog", limits))?;
    let pages_root = required_reference(catalog, b"Pages", "reading catalog Pages")?;
    let mut pending = vec![PendingPageTreeEntry::Node {
        reference: pages_root,
        expected_parent: None,
        depth: 0,
    }];
    let mut seen = HashSet::new();
    let mut pages = Vec::new();
    let mut parents = HashMap::new();
    let mut issues = Vec::new();
    let mut declared_root_count = None;
    let max_nodes = limits.max_objects.min(
        limits
            .max_pages
            .saturating_mul(limits.max_recursion_depth)
            .saturating_add(1),
    );
    let mut scheduled_nodes = 1_usize;

    while let Some(entry) = pending.pop() {
        let (reference, expected_parent, depth) = match entry {
            PendingPageTreeEntry::Node {
                reference,
                expected_parent,
                depth,
            } => (reference, expected_parent, depth),
            PendingPageTreeEntry::MalformedChild { parent } => {
                issues.push(PdfIssue::unresolved_page_tree_gap(
                    pages.len(),
                    format!(
                        "walking page tree: skipping non-reference child of object {} {}",
                        parent.0, parent.1
                    ),
                )?);
                continue;
            }
        };
        if depth > limits.max_recursion_depth {
            return Err(limit_error(
                "PDF page tree depth",
                limits.max_recursion_depth,
            ));
        }
        if !seen.insert(reference) {
            issues.push(PdfIssue::unresolved_page_tree_gap(
                pages.len(),
                format!(
                    "walking page tree: repeated object {} {}",
                    reference.0, reference.1
                ),
            )?);
            continue;
        }

        // Broken branches retain their Page Tree position so independently
        // valid branches survive; unsupported branches keep their taxonomy
        // instead of being relabeled or aborting the walk. Password
        // and security-handler failures never reach the walk (they fail at
        // document load), and resource-limit failures stay fatal.
        let resolved =
            match resolve_document_object(document, reference, limits, "walking page tree") {
                Ok(resolved) => resolved,
                Err(error @ (Error::Backend(_) | Error::Unsupported(_))) => {
                    let issue = if matches!(error, Error::Unsupported(_)) {
                        PdfIssue::unsupported_page_tree_gap(
                            pages.len(),
                            format!(
                                "walking page tree: skipping object {} {}: \
                         branch requires unsupported features ({error})",
                                reference.0, reference.1
                            ),
                        )?
                    } else {
                        PdfIssue::unresolved_page_tree_gap(
                            pages.len(),
                            format!(
                                "walking page tree: skipping object {} {}: \
                         branch could not be resolved ({error})",
                                reference.0, reference.1
                            ),
                        )?
                    };
                    issues.push(issue);
                    continue;
                }
                Err(error) => return Err(error),
            };
        let Ok(dictionary) = resolved.as_dict() else {
            issues.push(PdfIssue::unresolved_page_tree_gap(
                pages.len(),
                format!(
                    "walking page tree: skipping object {} {}: node is not a dictionary",
                    reference.0, reference.1
                ),
            )?);
            continue;
        };
        let declared_parent =
            match optional_reference(dictionary, b"Parent", "reading page tree Parent") {
                Ok(declared_parent) => declared_parent,
                Err(error) => {
                    issues.push(PdfIssue::unresolved_page_tree_gap(
                        pages.len(),
                        format!(
                            "walking page tree: skipping object {} {}: {error}",
                            reference.0, reference.1
                        ),
                    )?);
                    continue;
                }
            };
        if declared_parent != expected_parent {
            issues.push(PdfIssue::unresolved_page_tree_gap(
                pages.len(),
                format!(
                    "walking page tree: object {} {} has an inconsistent Parent",
                    reference.0, reference.1
                ),
            )?);
            continue;
        }
        parents.insert(reference, expected_parent);
        let Ok(node_type) = dictionary.get(b"Type").and_then(Object::as_name) else {
            issues.push(PdfIssue::unresolved_page_tree_gap(
                pages.len(),
                format!(
                    "walking page tree: skipping object {} {}: node has no readable /Type",
                    reference.0, reference.1
                ),
            )?);
            continue;
        };

        match node_type {
            b"Page" => {
                if pages.len() == limits.max_pages {
                    return Err(limit_error("PDF page count", limits.max_pages));
                }
                pages.push(PageRef(from_lopdf_id(reference)));
            }
            b"Pages" => {
                let declared_count = match dictionary.get(b"Count").and_then(Object::as_i64) {
                    Ok(declared_count) if declared_count >= 0 => {
                        let declared_count = usize::try_from(declared_count)
                            .map_err(|_| limit_error("PDF page count", limits.max_pages))?;
                        if declared_count > limits.max_pages {
                            return Err(limit_error("PDF page count", limits.max_pages));
                        }
                        Some(declared_count)
                    }
                    Ok(_) => {
                        issues.push(PdfIssue::unresolved(
                            "reading page tree Count: value is negative",
                        )?);
                        None
                    }
                    Err(error) => {
                        let error = map_lopdf_error(error, "reading page tree Count", limits);
                        issues.push(PdfIssue::unresolved(error.to_string())?);
                        None
                    }
                };
                if reference == pages_root {
                    declared_root_count = declared_count;
                }
                let kids = match dictionary.get(b"Kids").and_then(Object::as_array) {
                    Ok(kids) => kids,
                    Err(error) => {
                        let error = map_lopdf_error(error, "reading page tree Kids", limits);
                        issues.push(PdfIssue::unresolved_page_tree_gap(
                            pages.len(),
                            error.to_string(),
                        )?);
                        continue;
                    }
                };
                if kids.len() > max_nodes.saturating_sub(scheduled_nodes) {
                    return Err(limit_error("PDF page tree node count", max_nodes));
                }
                let child_depth = depth.saturating_add(1);
                if !kids.is_empty() && child_depth > limits.max_recursion_depth {
                    return Err(limit_error(
                        "PDF page tree depth",
                        limits.max_recursion_depth,
                    ));
                }
                scheduled_nodes += kids.len();
                for kid in kids.iter().rev() {
                    let entry = match kid.as_reference() {
                        Ok(kid) => PendingPageTreeEntry::Node {
                            reference: kid,
                            expected_parent: Some(reference),
                            depth: child_depth,
                        },
                        Err(_) => PendingPageTreeEntry::MalformedChild { parent: reference },
                    };
                    pending.push(entry);
                }
            }
            other => {
                issues.push(PdfIssue::unresolved_page_tree_gap(
                    pages.len(),
                    format!(
                        "walking page tree: skipping object {} {} with unexpected node type /{}",
                        reference.0,
                        reference.1,
                        String::from_utf8_lossy(other)
                    ),
                )?);
            }
        }
    }

    // Only a localized issue that skipped a branch can explain a root count
    // shortfall. Metadata issues that still traversed /Kids stay document
    // scoped and cannot suppress the mismatch.
    let has_skipped_branch_gaps = issues.iter().any(|issue| {
        matches!(
            issue.location(),
            crate::pdf::PdfIssueLocation::PageTreeGap { .. }
        )
    });
    if let Some(declared) = declared_root_count
        && (declared < pages.len() || declared > pages.len() && !has_skipped_branch_gaps)
    {
        issues.push(PdfIssue::unresolved(format!(
            "walking page tree: root declares {declared} pages but only {} valid pages were recovered",
            pages.len()
        ))?);
    }

    Ok(PageTree {
        pages,
        parents,
        issues,
    })
}

fn resolve_document_object<'a>(
    document: &'a Document,
    reference: lopdf::ObjectId,
    limits: ParseLimits,
    context: &str,
) -> Result<&'a Object> {
    let mut current = reference;
    let mut seen = HashSet::new();

    for _ in 0..=limits.max_recursion_depth {
        if !seen.insert(current) {
            return Err(Error::Backend(format!(
                "{context}: reference cycle at object {} {}",
                current.0, current.1
            )));
        }
        let object = document.objects.get(&current).ok_or_else(|| {
            Error::Backend(format!(
                "{context}: object {} {} not found",
                current.0, current.1
            ))
        })?;
        match object {
            Object::Reference(next) => current = *next,
            _ => return Ok(object),
        }
    }

    Err(limit_error(
        "PDF reference depth",
        limits.max_recursion_depth,
    ))
}

fn required_reference(
    dictionary: &Dictionary,
    key: &[u8],
    context: &str,
) -> Result<lopdf::ObjectId> {
    dictionary
        .get(key)
        .and_then(Object::as_reference)
        .map_err(|error| Error::Backend(format!("{context}: {error}")))
}

fn optional_reference(
    dictionary: &Dictionary,
    key: &[u8],
    context: &str,
) -> Result<Option<lopdf::ObjectId>> {
    match dictionary.get(key) {
        Ok(value) => value
            .as_reference()
            .map(Some)
            .map_err(|error| Error::Backend(format!("{context}: {error}"))),
        Err(lopdf::Error::DictKey(_)) => Ok(None),
        Err(error) => Err(Error::Backend(format!("{context}: {error}"))),
    }
}

fn convert_dictionary(dictionary: &Dictionary, depth: usize, max_depth: usize) -> Result<PdfDict> {
    dictionary
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                convert_object(value, child_depth(depth, max_depth)?, max_depth)?,
            ))
        })
        .collect()
}

fn convert_object(object: &Object, depth: usize, max_depth: usize) -> Result<PdfObject> {
    match object {
        Object::Null => Ok(PdfObject::Null),
        Object::Boolean(value) => Ok(PdfObject::Boolean(*value)),
        Object::Integer(value) => Ok(PdfObject::Integer(*value)),
        Object::Real(value) => Ok(PdfObject::Real(f64::from(*value))),
        Object::Name(value) => Ok(PdfObject::Name(value.clone())),
        Object::String(value, _) => Ok(PdfObject::String(value.clone())),
        Object::Array(values) => {
            let next_depth = child_depth(depth, max_depth)?;
            values
                .iter()
                .map(|value| convert_object(value, next_depth, max_depth))
                .collect::<Result<Vec<_>>>()
                .map(PdfObject::Array)
        }
        Object::Dictionary(dictionary) => {
            convert_dictionary(dictionary, depth, max_depth).map(PdfObject::Dictionary)
        }
        Object::Stream(stream) => {
            convert_dictionary(&stream.dict, depth, max_depth).map(PdfObject::Stream)
        }
        Object::Reference(reference) => Ok(PdfObject::Reference(from_lopdf_id(*reference))),
    }
}

fn child_depth(depth: usize, max_depth: usize) -> Result<usize> {
    let next = depth.saturating_add(1);
    if next > max_depth {
        return Err(limit_error("PDF object nesting depth", max_depth));
    }
    Ok(next)
}

pub(super) fn map_lopdf_error(error: lopdf::Error, context: &str, limits: ParseLimits) -> Error {
    match &error {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded { .. }) => {
            limit_error("PDF decoded stream bytes", limits.max_decoded_stream_bytes)
        }
        lopdf::Error::Unimplemented(feature) => Error::Unsupported(format!("{context}: {feature}")),
        lopdf::Error::InvalidPassword
        | lopdf::Error::AlreadyEncrypted
        | lopdf::Error::Decryption(_)
        | lopdf::Error::UnsupportedSecurityHandler(_) => {
            Error::Unsupported("PDF password is missing, invalid, or unsupported".into())
        }
        _ => Error::Backend(format!("{context}: {error}")),
    }
}

fn limit_error(resource: &'static str, limit: usize) -> Error {
    Error::LimitExceeded { resource, limit }
}

fn to_lopdf_id(reference: ObjectRef) -> lopdf::ObjectId {
    (reference.object_number, reference.generation)
}

fn from_lopdf_id(reference: lopdf::ObjectId) -> ObjectRef {
    ObjectRef {
        object_number: reference.0,
        generation: reference.1,
    }
}
