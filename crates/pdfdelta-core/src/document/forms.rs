use std::{collections::HashSet, sync::Arc};

use crate::{
    Error, Result,
    pdf::{ObjectRef, ParsedPdf, PdfDict, PdfObject, decode_text_string},
};

use super::{
    Channel, ChannelInventory, EvidenceFailure, EvidenceIssue, FieldValue, SourceRef,
    StructuredEvidence, StructuredValue,
};

#[derive(Clone, Copy, Debug)]
pub struct FormLimits {
    pub max_nodes: usize,
    pub max_depth: usize,
    pub max_text_bytes: usize,
}

impl Default for FormLimits {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_depth: 64,
            max_text_bytes: 16 * 1024 * 1024,
        }
    }
}

pub struct FormEvidence {
    pub fields: Vec<StructuredEvidence>,
    pub issues: Vec<EvidenceIssue>,
    pub inventory: ChannelInventory,
}

struct PendingField {
    object: PdfObject,
    prefix: Arc<str>,
    field_type: Option<Arc<Vec<u8>>>,
    value: Option<Arc<PdfObject>>,
    depth: usize,
    name_known: bool,
}

/// Reads saved `AcroForm` values through the neutral parser facade, including
/// fields without widgets. Names and values are inherited along the field tree;
/// button export names remain names rather than being collapsed into booleans.
/// Widget appearances and XFA stay explicit unresolved obligations.
///
/// # Errors
/// Rejects an unreadable form root. Descendant failures retain partial evidence
/// and an incomplete inventory; traversal, nesting, and text budgets are bounded.
pub fn extract_form_evidence(
    pdf: &dyn ParsedPdf,
    backend: usize,
    first_id: u64,
    limits: FormLimits,
) -> Result<FormEvidence> {
    let mut result = FormEvidence {
        fields: Vec::new(),
        issues: Vec::new(),
        inventory: ChannelInventory {
            page: None,
            channel: Channel::Forms,
            backend,
            sources: Vec::new(),
            complete: true,
        },
    };
    let trailer = pdf.trailer()?;
    let root = trailer
        .get(b"Root".as_slice())
        .ok_or_else(|| Error::Unresolved("PDF catalog is missing".into()))?;
    let (catalog, _) = dictionary(pdf, root.clone())?;
    match has_page_widgets(pdf, limits.max_nodes) {
        Ok(true) => failure(
            &mut result,
            EvidenceFailure::Unresolved,
            "page widget appearances require separate comparison".into(),
            Vec::new(),
        ),
        Ok(false) => {}
        Err(error) => failure(&mut result, classify(&error), error.to_string(), Vec::new()),
    }
    let Some(form) = catalog.get(b"AcroForm".as_slice()) else {
        return Ok(result);
    };
    let (form, _) = dictionary(pdf, form.clone())?;
    if form.contains_key(b"XFA".as_slice()) {
        failure(
            &mut result,
            EvidenceFailure::Unsupported,
            "XFA form content remains unexamined".into(),
            Vec::new(),
        );
    }
    let fields = match form.get(b"Fields".as_slice()) {
        Some(fields) => array(pdf, fields.clone())?,
        None => return Err(Error::Unresolved("AcroForm has no field inventory".into())),
    };
    if fields.len() > limits.max_nodes {
        return Err(Error::LimitExceeded {
            resource: "form field nodes",
            limit: limits.max_nodes,
        });
    }
    let widget_context = match super::widgets::WidgetContext::new(pdf, limits.max_nodes) {
        Ok(context) => Some(context),
        Err(error) => {
            failure(&mut result, classify(&error), error.to_string(), Vec::new());
            None
        }
    };
    let locate_widget = |dictionary: &PdfDict, object| {
        widget_context.as_ref().map_or_else(
            || super::FormWidget {
                object,
                page: None,
                bounds: None,
                normal_appearance: None,
                crop: None,
                unresolved: Some("widget location inventory is unavailable".into()),
            },
            |context| context.widget(pdf, dictionary, object),
        )
    };
    let mut pending: Vec<_> = fields
        .into_iter()
        .rev()
        .map(|object| PendingField {
            object,
            prefix: Arc::from(""),
            field_type: None,
            value: None,
            depth: 0,
            name_known: true,
        })
        .collect();
    let mut visited = HashSet::new();
    let mut nodes = 0usize;
    let mut text_bytes = 0usize;
    while let Some(field) = pending.pop() {
        if nodes >= limits.max_nodes || field.depth > limits.max_depth {
            failure(
                &mut result,
                EvidenceFailure::ResourceLimit,
                "form traversal budget exhausted".into(),
                Vec::new(),
            );
            break;
        }
        nodes += 1;
        let (dictionary, reference) = match dictionary(pdf, field.object) {
            Ok(value) => value,
            Err(error) => {
                failure(
                    &mut result,
                    EvidenceFailure::Unresolved,
                    error.to_string(),
                    Vec::new(),
                );
                continue;
            }
        };
        if reference.is_some_and(|reference| !visited.insert(reference)) {
            failure(
                &mut result,
                EvidenceFailure::Unresolved,
                "cyclic or repeated form field reference".into(),
                Vec::new(),
            );
            continue;
        }
        let mut name_known = field.name_known;
        let name = match dictionary.get(b"T".as_slice()) {
            Some(value) => resolve(pdf, value.clone()).and_then(|resolved| match resolved.0 {
                PdfObject::String(bytes) => {
                    let name = decode_text_string(
                        &bytes,
                        limits.max_text_bytes.saturating_sub(text_bytes),
                    )?;
                    if name.contains('.') {
                        return Err(Error::Unresolved(
                            "form partial name contains a hierarchy separator".into(),
                        ));
                    }
                    Ok(name)
                }
                _ => Err(Error::Unresolved(
                    "form partial name is not a text string".into(),
                )),
            }),
            None => Ok(String::new()),
        };
        let mut name = match name {
            Ok(name) if field.prefix.is_empty() => name,
            Ok(name) if name.is_empty() => field.prefix.to_string(),
            Ok(name) => format!("{}.{}", field.prefix, name),
            Err(error) => {
                name_known = false;
                failure(
                    &mut result,
                    EvidenceFailure::Unresolved,
                    error.to_string(),
                    Vec::new(),
                );
                String::new()
            }
        };
        if !name_known {
            name.clear();
        }
        text_bytes = text_bytes.saturating_add(name.len());
        if text_bytes > limits.max_text_bytes {
            failure(
                &mut result,
                EvidenceFailure::ResourceLimit,
                "form name byte budget exhausted".into(),
                Vec::new(),
            );
            break;
        }
        let field_type = match dictionary.get(b"FT".as_slice()) {
            Some(value) => {
                if let Ok((PdfObject::Name(name), _)) = resolve(pdf, value.clone()) {
                    Some(Arc::new(name))
                } else {
                    failure(
                        &mut result,
                        EvidenceFailure::Unresolved,
                        "invalid form field type".into(),
                        Vec::new(),
                    );
                    None
                }
            }
            None => field.field_type,
        };
        text_bytes = text_bytes.saturating_add(field_type.as_ref().map_or(0, |name| name.len()));
        if text_bytes > limits.max_text_bytes {
            failure(
                &mut result,
                EvidenceFailure::ResourceLimit,
                "form field type byte budget exhausted".into(),
                Vec::new(),
            );
            break;
        }
        let value = dictionary
            .get(b"V".as_slice())
            .cloned()
            .map(Arc::new)
            .or(field.value);
        let mut children = Vec::new();
        let mut children_unknown = false;
        let mut widgets = matches!(dictionary.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name == b"Widget");
        let button = field_type.as_deref().map(Vec::as_slice) == Some(b"Btn");
        let mut widget_evidence = Vec::new();
        if widgets {
            widget_evidence.push(locate_widget(&dictionary, reference));
        }
        let mut states = Vec::new();
        if widgets && button {
            states.push((
                reference,
                button_state(pdf, &dictionary, &mut text_bytes, limits.max_text_bytes),
            ));
        }
        if let Some(kids) = dictionary.get(b"Kids".as_slice()) {
            let kids = match array(pdf, kids.clone()) {
                Ok(kids) => kids,
                Err(error) => {
                    failure(
                        &mut result,
                        EvidenceFailure::Unresolved,
                        error.to_string(),
                        Vec::new(),
                    );
                    children_unknown = true;
                    Vec::new()
                }
            };
            if kids.len() > limits.max_nodes.saturating_sub(nodes) {
                failure(
                    &mut result,
                    EvidenceFailure::ResourceLimit,
                    "form widget/child count exceeds budget".into(),
                    Vec::new(),
                );
                break;
            }
            for kid in kids {
                let (child, child_reference) = match dictionary_ref(pdf, &kid) {
                    Ok(child) => child,
                    Err(error) => {
                        failure(
                            &mut result,
                            EvidenceFailure::Unresolved,
                            error.to_string(),
                            Vec::new(),
                        );
                        children_unknown = true;
                        continue;
                    }
                };
                let widget = matches!(child.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name == b"Widget")
                    && !child.contains_key(b"T".as_slice())
                    && !child.contains_key(b"FT".as_slice());
                if widget {
                    nodes += 1;
                    widgets = true;
                    widget_evidence.push(locate_widget(&child, child_reference));
                    if button {
                        states.push((
                            child_reference,
                            button_state(pdf, &child, &mut text_bytes, limits.max_text_bytes),
                        ));
                    }
                } else {
                    children.push(kid);
                }
            }
        }
        if !children.is_empty() {
            if pending
                .len()
                .saturating_add(children.len())
                .saturating_add(nodes)
                > limits.max_nodes
            {
                failure(
                    &mut result,
                    EvidenceFailure::ResourceLimit,
                    "form child inventory exceeds node budget".into(),
                    Vec::new(),
                );
                break;
            }
            let prefix: Arc<str> = name.into();
            pending.extend(children.into_iter().rev().map(|object| PendingField {
                object,
                prefix: Arc::clone(&prefix),
                field_type: field_type.clone(),
                value: value.clone(),
                depth: field.depth + 1,
                name_known,
            }));
            continue;
        }
        let value_object = value
            .as_ref()
            .map(|value| resolve(pdf, (**value).clone()).map(|resolved| resolved.0))
            .transpose();
        let (value_object, decoded) = match value_object {
            Ok(value) => {
                let decoded = if children_unknown {
                    Err(Error::Unresolved("field children are unresolved".into()))
                } else {
                    decode_value(
                        value.as_ref(),
                        field_type.as_deref().map(Vec::as_slice),
                        limits.max_text_bytes.saturating_sub(text_bytes),
                        limits.max_nodes,
                    )
                };
                (value, decoded)
            }
            Err(error) => (None, Err(error)),
        };
        let id = first_id
            .checked_add(result.fields.len() as u64)
            .ok_or(Error::LimitExceeded {
                resource: "form evidence identifiers",
                limit: usize::MAX,
            })?;
        let source = SourceRef::Structured { element: id };
        let value = match decoded {
            Ok((value, bytes)) => {
                text_bytes = text_bytes.saturating_add(bytes);
                value
            }
            Err(error) => {
                let raw_bytes = match &value_object {
                    Some(PdfObject::String(bytes) | PdfObject::Name(bytes))
                        if bytes.len() <= limits.max_text_bytes.saturating_sub(text_bytes) =>
                    {
                        Some(bytes.clone())
                    }
                    _ => None,
                };
                text_bytes = text_bytes.saturating_add(raw_bytes.as_ref().map_or(0, Vec::len));
                let reason = error.to_string();
                failure(&mut result, classify(&error), reason.clone(), vec![source]);
                FieldValue::Unresolved { raw_bytes, reason }
            }
        };
        // Export-option mappings require a separate interpretation of the value.
        if let FieldValue::Name(saved) = &value
            && !dictionary.contains_key(b"Opt".as_slice())
        {
            let mut active_matches = false;
            let mut mismatched = false;
            let mut unknown = children_unknown;
            for (_, state) in &states {
                match state {
                    Ok(state) => {
                        if state != b"Off" {
                            active_matches |= state == saved;
                            mismatched |= state != saved;
                        }
                    }
                    Err(error) => {
                        unknown = true;
                        failure(
                            &mut result,
                            classify(error),
                            error.to_string(),
                            vec![source],
                        );
                    }
                }
            }
            if mismatched || (!states.is_empty() && !unknown && saved != b"Off" && !active_matches)
            {
                failure(&mut result, EvidenceFailure::Unresolved,
                    "saved button value and declared widget appearance states disagree; neither value was substituted".into(), vec![source]);
            }
        }
        result.fields.push(StructuredEvidence {
            id,
            page: None,
            bounds: None,
            object: reference,
            backend,
            value: StructuredValue::FormField {
                name,
                field_type: field_type.as_deref().cloned(),
                value,
                widgets: widget_evidence,
                button_states: states
                    .into_iter()
                    .map(|(widget, state)| super::ButtonAppearanceState {
                        widget,
                        name: state.ok(),
                    })
                    .collect(),
            },
        });
        result.inventory.sources.push(source);
        if widgets || dictionary.contains_key(b"RV".as_slice()) {
            failure(
                &mut result,
                EvidenceFailure::Unresolved,
                "form appearance or rich-text value requires separate comparison".into(),
                vec![source],
            );
        }
    }
    Ok(result)
}

fn button_state(
    pdf: &dyn ParsedPdf,
    widget: &PdfDict,
    bytes: &mut usize,
    limit: usize,
) -> Result<Vec<u8>> {
    let state = widget
        .get(b"AS".as_slice())
        .ok_or_else(|| Error::Unresolved("button widget appearance state is missing".into()))?;
    let (PdfObject::Name(state), _) = resolve(pdf, state.clone())? else {
        return Err(Error::Unresolved(
            "button widget appearance state is not a name".into(),
        ));
    };
    if state.len() > limit.saturating_sub(*bytes) {
        return Err(Error::LimitExceeded {
            resource: "form appearance state bytes",
            limit,
        });
    }
    *bytes += state.len();
    Ok(state)
}

fn decode_value(
    value: Option<&PdfObject>,
    field_type: Option<&[u8]>,
    limit: usize,
    max_choices: usize,
) -> Result<(FieldValue, usize)> {
    match (field_type, value) {
        (Some(b"Tx" | b"Ch" | b"Btn"), None | Some(PdfObject::Null)) => Ok((FieldValue::Empty, 0)),
        (Some(b"Tx" | b"Ch"), Some(PdfObject::String(bytes))) => {
            let text = decode_text_string(bytes, limit)?;
            let size = text.len();
            Ok((FieldValue::Text(text), size))
        }
        (Some(b"Btn"), Some(PdfObject::Name(bytes))) if bytes.len() <= limit => {
            Ok((FieldValue::Name(bytes.clone()), bytes.len()))
        }
        (Some(b"Ch"), Some(PdfObject::Array(values))) => {
            if values.len() > max_choices {
                return Err(Error::LimitExceeded {
                    resource: "form choice values",
                    limit: max_choices,
                });
            }
            let mut choices = Vec::new();
            let mut size = 0usize;
            for value in values {
                let PdfObject::String(bytes) = value else {
                    return Err(Error::Unresolved(
                        "choice value is not a text string".into(),
                    ));
                };
                let text = decode_text_string(bytes, limit.saturating_sub(size))?;
                size += text.len();
                choices.push(text);
            }
            Ok((FieldValue::Choices(choices), size))
        }
        _ => Err(Error::Unresolved(
            "unsupported or malformed saved form value".into(),
        )),
    }
}

fn failure(
    result: &mut FormEvidence,
    kind: EvidenceFailure,
    reason: String,
    sources: Vec<SourceRef>,
) {
    result.inventory.complete = false;
    result.issues.push(EvidenceIssue {
        page: None,
        channel: Channel::Forms,
        sources,
        kind,
        reason,
    });
}

pub(super) fn classify(error: &Error) -> EvidenceFailure {
    match error {
        Error::LimitExceeded { .. } => EvidenceFailure::ResourceLimit,
        Error::Unsupported(_) => EvidenceFailure::Unsupported,
        Error::Backend(_) => EvidenceFailure::BackendFailure,
        _ => EvidenceFailure::Unresolved,
    }
}

fn has_page_widgets(pdf: &dyn ParsedPdf, max_nodes: usize) -> Result<bool> {
    let pages = pdf.pages()?;
    if pages.len() > max_nodes {
        return Err(Error::LimitExceeded {
            resource: "form annotation pages",
            limit: max_nodes,
        });
    }
    let mut nodes = 0usize;
    for page in pages {
        let page = pdf.page_dict(page)?;
        if let Some(annotations) = page.get(b"Annots".as_slice()) {
            let annotations = array(pdf, annotations.clone())?;
            nodes = nodes.saturating_add(annotations.len());
            if nodes > max_nodes {
                return Err(Error::LimitExceeded {
                    resource: "form annotations",
                    limit: max_nodes,
                });
            }
            for annotation in annotations {
                let (annotation, _) = dictionary(pdf, annotation)?;
                if matches!(annotation.get(b"Subtype".as_slice()), Some(PdfObject::Name(name)) if name == b"Widget")
                {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

pub(super) fn resolve(
    pdf: &dyn ParsedPdf,
    object: PdfObject,
) -> Result<(PdfObject, Option<ObjectRef>)> {
    match object {
        PdfObject::Reference(reference) => pdf
            .resolve_with_terminal(reference)
            .map(|resolved| (resolved.object, Some(resolved.reference))),
        object => Ok((object, None)),
    }
}

fn dictionary_ref(pdf: &dyn ParsedPdf, object: &PdfObject) -> Result<(PdfDict, Option<ObjectRef>)> {
    dictionary(pdf, object.clone())
}

pub(super) fn dictionary(
    pdf: &dyn ParsedPdf,
    object: PdfObject,
) -> Result<(PdfDict, Option<ObjectRef>)> {
    let (object, reference) = resolve(pdf, object)?;
    match object {
        PdfObject::Dictionary(dictionary) => Ok((dictionary, reference)),
        _ => Err(Error::Unresolved("PDF object is not a dictionary".into())),
    }
}

fn array(pdf: &dyn ParsedPdf, object: PdfObject) -> Result<Vec<PdfObject>> {
    match resolve(pdf, object)?.0 {
        PdfObject::Array(array) => Ok(array),
        _ => Err(Error::Unresolved(
            "form child inventory is not an array".into(),
        )),
    }
}
