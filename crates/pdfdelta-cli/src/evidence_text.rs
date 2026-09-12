//! Bounded, terminal-safe presentation of typed changes and unresolved evidence.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

use pdfdelta_core::document::{
    DocumentGraph, DocumentViewComparison, EdgeKind, EvidenceIssue, FieldValue, GraphNode,
    InterpretationStatus, NodeId, NodeKind, TypedOperation,
};

const MAX_ENTRIES: usize = 100;
const MAX_PREVIEW_CHARS: usize = 160;

fn escaped(value: &str) -> (String, bool) {
    let mut chars = value.chars();
    let mut output = String::new();
    for ch in chars.by_ref().take(MAX_PREVIEW_CHARS) {
        if is_bidi_control(ch) {
            output.extend(ch.escape_unicode());
        } else {
            output.extend(ch.escape_debug());
        }
    }
    (output, chars.next().is_some())
}

pub(crate) fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

/// Renders untrusted bytes for a terminal by escaping control characters and
/// Unicode bidi controls; every other character is preserved as-is.
pub(crate) fn escape_terminal_controls(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() || is_bidi_control(ch) {
            output.extend(ch.escape_unicode());
        } else {
            output.push(ch);
        }
    }
    output
}

fn preview(value: &str) -> String {
    let (mut text, truncated) = escaped(value);
    if truncated {
        text.push_str(" [truncated]");
    }
    text
}

fn quoted(value: &str) -> String {
    let (text, truncated) = escaped(value);
    format!("\"{text}\"{}", if truncated { " [truncated]" } else { "" })
}

fn field_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => quoted(text),
        FieldValue::Name(bytes) => {
            if let Ok(text) = std::str::from_utf8(bytes) {
                format!("name {}", quoted(text))
            } else {
                let mut text = String::from("name bytes: ");
                for byte in bytes.iter().take(64) {
                    let _ = write!(text, "{byte:02x}");
                }
                if bytes.len() > 64 {
                    text.push_str(" [truncated]");
                }
                text
            }
        }
        FieldValue::Selected(selected) => if *selected {
            "selected"
        } else {
            "not selected"
        }
        .into(),
        FieldValue::Choices(choices) => {
            let mut text = choices
                .iter()
                .take(6)
                .map(|choice| quoted(choice))
                .collect::<Vec<_>>()
                .join(", ");
            if choices.len() > 6 {
                let _ = write!(text, " (+{} choices)", choices.len() - 6);
            }
            format!("[{text}]")
        }
        FieldValue::Empty => "(empty)".into(),
        FieldValue::Unresolved { reason, .. } => format!("(unresolved: {})", preview(reason)),
    }
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Document => "Document",
        NodeKind::Page => "Page",
        NodeKind::Section => "Section",
        NodeKind::Paragraph => "Paragraph",
        NodeKind::Header => "Header",
        NodeKind::Footer => "Footer",
        NodeKind::List => "List",
        NodeKind::ListItem => "List item",
        NodeKind::Table => "Table",
        NodeKind::Row => "Row",
        NodeKind::Column => "Column",
        NodeKind::Cell => "Cell",
        NodeKind::Form => "Form",
        NodeKind::Field => "Field",
        NodeKind::Figure => "Figure",
        NodeKind::Caption => "Caption",
        NodeKind::Code => "Code",
        NodeKind::Formula => "Formula",
        NodeKind::Annotation => "Annotation",
        NodeKind::Unknown => "Content",
    }
}

fn relation_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "membership",
        EdgeKind::Precedes => "order",
        EdgeKind::RowMember => "row membership",
        EdgeKind::ColumnMember => "column membership",
        EdgeKind::LabelFor => "label association",
        EdgeKind::CaptionFor => "caption association",
        EdgeKind::RefersTo => "reference",
        EdgeKind::AppearanceFor => "appearance association",
    }
}

fn label(ids: &[NodeId], nodes: &BTreeMap<NodeId, &GraphNode>) -> String {
    let Some(node) = ids.first().and_then(|id| nodes.get(id)) else {
        return "unlocated content".into();
    };
    let mut text = kind_name(node.kind).to_owned();
    if ids.len() == 1
        && matches!(
            node.kind,
            NodeKind::Field | NodeKind::Row | NodeKind::Column | NodeKind::Cell | NodeKind::Section
        )
        && let Some(key) = &node.identity
    {
        let _ = write!(text, " {}", quoted(&key.value));
    }
    let pages: BTreeSet<_> = ids
        .iter()
        .filter_map(|id| nodes.get(id))
        .flat_map(|node| node.pages.iter().map(|page| u64::from(page.0) + 1))
        .collect();
    if !pages.is_empty() {
        let numbers = pages
            .iter()
            .take(8)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            text,
            " ({} {numbers}{})",
            if pages.len() == 1 { "page" } else { "pages" },
            if pages.len() > 8 { ", ..." } else { "" }
        );
    }
    text
}

fn status(interpretation: InterpretationStatus) -> &'static str {
    match interpretation {
        InterpretationStatus::ConditionalOnCorrespondence => "Change (conditional correspondence)",
        InterpretationStatus::Inferred => "Inferred change",
    }
}

fn entry(seen: &mut usize) -> bool {
    *seen += 1;
    *seen <= MAX_ENTRIES
}

pub(super) fn append_details(
    text: &mut String,
    comparison: &DocumentViewComparison,
    old: &DocumentGraph,
    new: &DocumentGraph,
    old_issues: &[EvidenceIssue],
    new_issues: &[EvidenceIssue],
) {
    let old_nodes = old.nodes.iter().map(|node| (node.id, node)).collect();
    let new_nodes = new.nodes.iter().map(|node| (node.id, node)).collect();
    let mut seen = 0;
    for pair in comparison.comparisons() {
        let Some(operation) = &pair.operation else {
            continue;
        };
        if !entry(&mut seen) {
            continue;
        }
        let _ = writeln!(
            text,
            "\n{}: {} -> {}",
            status(pair.interpretation),
            label(&pair.old, &old_nodes),
            label(&pair.new, &new_nodes)
        );
        match operation {
            TypedOperation::TextChanged { old, new } => {
                let _ = writeln!(
                    text,
                    "  - {}\n  + {}",
                    old.as_deref()
                        .map_or_else(|| "(text unresolved)".into(), quoted),
                    new.as_deref()
                        .map_or_else(|| "(text unresolved)".into(), quoted)
                );
            }
            TypedOperation::ValueChanged { old, new } => {
                let _ = writeln!(text, "  - {}\n  + {}", field_value(old), field_value(new));
            }
            TypedOperation::RenderedRegionChanged => {
                let _ = writeln!(text, "  Rendered content changed.");
            }
            TypedOperation::PageRenderingChanged => {
                let _ = writeln!(text, "  Page rendering changed.");
            }
        }
        if let Some(mask) = &pair.text_mask {
            let _ = writeln!(
                text,
                "  Mandatory changed positions: old {}, new {}. Displayed values include context.",
                mask.old.len(),
                mask.new.len()
            );
        }
        if let Some(mask) = &pair.pixel_mask {
            let _ = writeln!(
                text,
                "  Changed pixels: {} ({} x {} grid).",
                mask.changed_pixels, mask.width, mask.height
            );
        }
        for reason in pair.unresolved.iter().take(3) {
            let _ = writeln!(text, "  Unresolved: {}", preview(reason));
        }
        if pair.unresolved.len() > 3 {
            let _ = writeln!(
                text,
                "  {} additional unresolved reasons.",
                pair.unresolved.len() - 3
            );
        }
    }
    for relation in comparison.relations().filter(|relation| relation.changed()) {
        if !entry(&mut seen) {
            continue;
        }
        let _ = writeln!(
            text,
            "\n{}: {}",
            status(relation.interpretation),
            relation_name(relation.kind)
        );
        let _ = writeln!(
            text,
            "  - {} -> {}: {}",
            label(&relation.old[0], &old_nodes),
            label(&relation.old[1], &old_nodes),
            if relation.old_present {
                "present"
            } else {
                "absent"
            }
        );
        let _ = writeln!(
            text,
            "  + {} -> {}: {}",
            label(&relation.new[0], &new_nodes),
            label(&relation.new[1], &new_nodes),
            if relation.new_present {
                "present"
            } else {
                "absent"
            }
        );
    }
    for pair in comparison
        .comparisons()
        .filter(|pair| pair.operation.is_none() && !pair.unresolved.is_empty())
    {
        for reason in &pair.unresolved {
            if entry(&mut seen) {
                let _ = writeln!(
                    text,
                    "\nUnresolved {} -> {}: {}",
                    label(&pair.old, &old_nodes),
                    label(&pair.new, &new_nodes),
                    preview(reason)
                );
            }
        }
    }
    for scope in &comparison.scopes {
        for reason in &scope.result.unresolved {
            if entry(&mut seen) {
                let _ = writeln!(
                    text,
                    "\nUnresolved comparison for {}: {}",
                    label(&[scope.result.matching.scope.old], &old_nodes),
                    preview(reason)
                );
            }
        }
    }
    for reason in &comparison.relation_unresolved {
        if entry(&mut seen) {
            let _ = writeln!(text, "\nUnresolved relationships: {}", preview(reason));
        }
    }
    for (side, issues) in [("old", old_issues), ("new", new_issues)] {
        for issue in issues {
            if !entry(&mut seen) {
                continue;
            }
            let page = issue
                .page
                .map(|page| format!(" page {}", u64::from(page.0) + 1))
                .unwrap_or_default();
            let _ = writeln!(
                text,
                "\nUnresolved {side} {:?}{page}: {}",
                issue.channel,
                preview(&issue.reason)
            );
        }
    }
    if seen > MAX_ENTRIES {
        let _ = writeln!(
            text,
            "\n{} additional report entries omitted; use --json PATH for full results.",
            seen - MAX_ENTRIES
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_escape_terminal_controls_and_bound_unicode_input() {
        let text = preview("value\n\x1b]0;forged title\x07\u{202e}suffix");
        assert!(!text.contains(['\n', '\x1b', '\x07', '\u{202e}']));
        assert!(text.contains("\\n"));
        assert!(preview(&"界".repeat(MAX_PREVIEW_CHARS + 1)).ends_with(" [truncated]"));
        assert_eq!(preview("日本語"), "日本語");
    }

    #[test]
    fn terminal_escaping_covers_line_and_paragraph_separators() {
        assert_eq!(escape_terminal_controls("\u{2028}"), "\\u{2028}");
        assert_eq!(escape_terminal_controls("\u{2029}"), "\\u{2029}");
    }

    #[test]
    fn detail_limits_leave_the_full_comparison_intact() {
        let comparison = DocumentViewComparison {
            scopes: Vec::new(),
            relations: Vec::new(),
            relation_unresolved: vec!["unmatched endpoint".into(); MAX_ENTRIES + 5],
        };
        let mut text = String::new();
        append_details(
            &mut text,
            &comparison,
            &DocumentGraph::default(),
            &DocumentGraph::default(),
            &[],
            &[],
        );
        assert_eq!(
            text.matches("Unresolved relationships:").count(),
            MAX_ENTRIES
        );
        assert!(text.contains("5 additional report entries omitted"));
        assert_eq!(comparison.relation_unresolved.len(), MAX_ENTRIES + 5);
        assert_eq!(quoted("literal [truncated]"), "\"literal [truncated]\"");
        assert!(quoted(&"x".repeat(MAX_PREVIEW_CHARS + 1)).ends_with("\" [truncated]"));
    }

    #[test]
    fn field_displays_preserve_value_types_and_raw_names() {
        assert_ne!(
            field_value(&FieldValue::Empty),
            field_value(&FieldValue::Text(String::new()))
        );
        assert_eq!(
            field_value(&FieldValue::Name(vec![0xff, 0x00])),
            "name bytes: ff00"
        );
        assert_eq!(field_value(&FieldValue::Selected(false)), "not selected");
        assert!(field_value(&FieldValue::Choices(vec!["x".into(); 9])).contains("+3 choices"));
    }
}
