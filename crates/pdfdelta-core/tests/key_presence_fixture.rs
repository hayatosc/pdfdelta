use std::collections::BTreeSet;

use pdfdelta_core::{
    document::{
        BackendIdentity, BackendKind, EvidenceLimits, EvidenceStore, FieldValue, KeyCounterpart,
        KeyDomain, KeyInventory, KeyPresenceLimits, MissingKeyEvidence, PresenceSide,
        StructuredEvidence, StructuredValue, compare_document_keys,
    },
    model::Document,
};

fn fields(revision: &str, names: &[&str], complete: bool) -> EvidenceStore {
    let store = EvidenceStore {
        revision: revision.into(),
        backends: vec![BackendIdentity {
            kind: BackendKind::NativeParser,
            name: "fixture".into(),
            version: "1".into(),
            profile: "raw-keys".into(),
            model: None,
        }],
        pages: Vec::new(),
        native: Document::new(Vec::new()),
        rendered: Vec::new(),
        structured: names
            .iter()
            .enumerate()
            .map(|(index, name)| StructuredEvidence {
                id: index as u64,
                page: None,
                bounds: None,
                object: None,
                backend: 0,
                value: StructuredValue::FormField {
                    name: (*name).into(),
                    field_type: Some(b"Tx".to_vec()),
                    value: FieldValue::Empty,
                    widgets: Vec::new(),
                    button_states: Vec::new(),
                },
            })
            .collect(),
        inventories: Vec::new(),
        issues: Vec::new(),
        key_inventories: vec![KeyInventory {
            domain: KeyDomain::PdfFieldName,
            backend: 0,
            complete,
        }],
    };
    store
        .validate(EvidenceLimits::default())
        .expect("valid evidence");
    store
}

fn graphs(
    old: &EvidenceStore,
    new: &EvidenceStore,
) -> (
    pdfdelta_core::document::DocumentGraph,
    pdfdelta_core::document::DocumentGraph,
) {
    use pdfdelta_core::document::DocumentGraph;
    let make = |store| {
        DocumentGraph::from_evidence(
            store,
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .expect("native graph")
    };
    (make(old), make(new))
}

fn compare_graphs(
    old: &EvidenceStore,
    new: &EvidenceStore,
    a: &pdfdelta_core::document::DocumentGraph,
    b: &pdfdelta_core::document::DocumentGraph,
    limits: pdfdelta_core::document::DocumentComparisonLimits,
) -> pdfdelta_core::document::DocumentViewComparison {
    use pdfdelta_core::document::{
        CorrespondenceScope, DocumentView, NodeId, compare_document_views,
    };
    compare_document_views(
        DocumentView {
            evidence: old,
            graph: a,
        },
        DocumentView {
            evidence: new,
            graph: b,
        },
        CorrespondenceScope {
            old: NodeId(0),
            new: NodeId(0),
        },
        limits,
        Default::default(),
    )
    .expect("document comparison")
}

#[test]
fn closed_field_presence_accounts_for_slots_without_character_masks() {
    use pdfdelta_core::document::{
        Channel, ChannelInventory, DocumentView, KeyedElementOperationKind, SourceRef,
        document_coverage,
    };
    for (old_names, new_names, kind) in [
        (
            vec!["a"],
            vec!["a", "b"],
            KeyedElementOperationKind::Inserted,
        ),
        (
            vec!["a", "b"],
            vec!["a"],
            KeyedElementOperationKind::Removed,
        ),
        (vec![], vec!["b"], KeyedElementOperationKind::Inserted),
        (vec!["b"], vec![], KeyedElementOperationKind::Removed),
    ] {
        let mut old = fields("old", &old_names, true);
        let mut new = fields("new", &new_names, true);
        for store in [&mut old, &mut new] {
            store.inventories.push(ChannelInventory {
                page: None,
                channel: Channel::Forms,
                backend: 0,
                sources: store
                    .structured
                    .iter()
                    .map(|element| SourceRef::Structured {
                        element: element.id,
                    })
                    .collect(),
                complete: true,
            });
        }
        let (a, b) = graphs(&old, &new);
        let comparison = compare_graphs(&old, &new, &a, &b, Default::default());
        let operations: Vec<_> = comparison.keyed_element_operations().collect();
        assert_eq!(operations.len(), 1);
        assert_eq!(operations[0].kind, kind);
        let keys = comparison.key_presence.as_ref().expect("raw keys");
        assert_eq!(keys.claims[operations[0].claim].key, b"b");
        assert_eq!(
            operations[0].review_sources,
            vec![operations[0].identity_source]
        );
        assert!(
            comparison.comparisons().all(|pair| !pair.old.is_empty()
                && !pair.new.is_empty()
                && pair.text_mask.is_none())
        );
        let coverage = document_coverage(
            DocumentView {
                evidence: &old,
                graph: &a,
            },
            DocumentView {
                evidence: &new,
                graph: &b,
            },
            &comparison,
            &BTreeSet::from([Channel::Forms]),
        );
        assert!(coverage[0].complete);
        assert_eq!(
            coverage[0].old_presence_sources + coverage[0].new_presence_sources,
            1
        );
    }
}

#[test]
fn graph_labels_aliases_and_reparenting_cannot_forge_native_membership() {
    use pdfdelta_core::document::{NodeId, SourceRef};
    let old = fields("old", &["a"], true);
    let new = fields("new", &["a", "b"], true);
    for mutation in 0..4 {
        let (a, mut b) = graphs(&old, &new);
        let position = b
            .nodes
            .iter()
            .position(|node| node.sources.contains(&SourceRef::Structured { element: 1 }))
            .expect("new field");
        let id = b.nodes[position].id;
        match mutation {
            0 => b.nodes[position].identity.as_mut().expect("identity").value = "forged".into(),
            1 => {
                let mut alias = b.nodes[position].clone();
                alias.id = NodeId(999);
                b.nodes.push(alias);
                let mut edge = b
                    .edges
                    .iter()
                    .find(|edge| edge.to == id)
                    .expect("parent")
                    .clone();
                edge.to = NodeId(999);
                b.edges.push(edge);
            }
            2 => {
                let parent = b
                    .nodes
                    .iter()
                    .find(|node| node.sources.contains(&SourceRef::Structured { element: 0 }))
                    .expect("other field")
                    .id;
                b.edges
                    .iter_mut()
                    .find(|edge| edge.to == id)
                    .expect("parent")
                    .from = parent;
            }
            _ => b
                .source_conflicts
                .push(pdfdelta_core::document::SourceConflict {
                    sources: vec![
                        SourceRef::Structured { element: 0 },
                        SourceRef::Structured { element: 1 },
                    ],
                    reason: "competing ownership of one field appearance".into(),
                }),
        }
        let comparison = compare_graphs(&old, &new, &a, &b, Default::default());
        assert_eq!(comparison.keyed_element_operations().count(), 0);
        assert!(
            !comparison
                .key_presence
                .expect("keys")
                .scoped
                .expect("membership")
                .obligations
                .is_empty()
        );
    }
}

#[test]
fn incomplete_populations_renames_and_scoped_work_limits_never_create_element_operations() {
    use pdfdelta_core::document::{DocumentComparisonLimits, ScopedKeyMissing};
    for (names, complete) in [(vec!["b"], true), (vec![], false), (vec![""], true)] {
        let old = fields("old", &["a"], true);
        let new = fields("new", &names, complete);
        let (a, b) = graphs(&old, &new);
        assert_eq!(
            compare_graphs(&old, &new, &a, &b, Default::default())
                .keyed_element_operations()
                .count(),
            0
        );
    }
    let old = fields("old", &["a"], true);
    let new = fields("new", &[], true);
    let (a, b) = graphs(&old, &new);
    let complete = compare_graphs(&old, &new, &a, &b, Default::default());
    let raw_work = complete.key_presence.expect("keys").work;
    let limited = compare_graphs(
        &old,
        &new,
        &a,
        &b,
        DocumentComparisonLimits {
            keys: KeyPresenceLimits { max_work: raw_work },
            ..Default::default()
        },
    );
    assert_eq!(limited.keyed_element_operations().count(), 0);
    let scoped = limited
        .key_presence
        .expect("keys")
        .scoped
        .expect("scoped keys");
    assert!(!scoped.exhaustive);
    assert!(
        scoped
            .obligations
            .iter()
            .any(|obligation| obligation.missing == ScopedKeyMissing::WorkBudget)
    );
}

fn paragraphs(revision: &str, parent: Option<u64>) -> EvidenceStore {
    let mut store = fields(revision, &[], true);
    store.key_inventories.push(KeyInventory {
        domain: KeyDomain::PdfStructureId,
        backend: 0,
        complete: true,
    });
    for (id, identifier, role, owner) in [
        (0, "section-a", "Sect", None),
        (1, "section-b", "Sect", None),
    ]
    .into_iter()
    .chain(parent.map(|parent| (2, "paragraph", "P", Some(parent))))
    {
        store.structured.push(StructuredEvidence {
            id,
            page: None,
            bounds: None,
            object: None,
            backend: 0,
            value: StructuredValue::StructureElement {
                content: None,
                identifier: Some(identifier.as_bytes().to_vec()),
                role: role.into(),
                glyphs: Vec::new(),
                text: (role == "P").then(|| "Existing words under a new identity".into()),
                parent: owner,
                order: Some(id as u32),
            },
        });
    }
    store
}

#[test]
fn paragraph_presence_requires_a_matched_scope_and_retains_movement() {
    use pdfdelta_core::document::{
        KeyedElementOperationKind, NodeKind, ScopedKeyCounterpart, ViewBasis,
    };
    for (old_parent, new_parent, expected) in [
        (Some(0), None, Some(KeyedElementOperationKind::Removed)),
        (None, Some(0), Some(KeyedElementOperationKind::Inserted)),
        (Some(0), Some(1), None),
    ] {
        let old = paragraphs("old", old_parent);
        let new = paragraphs("new", new_parent);
        let (mut a, b) = graphs(&old, &new);
        let comparison = compare_graphs(&old, &new, &a, &b, Default::default());
        let operations: Vec<_> = comparison.keyed_element_operations().collect();
        if let Some(expected) = expected {
            assert_eq!(operations.len(), 1);
            assert_eq!(operations[0].kind, expected);
            assert_eq!(operations[0].node_kind, NodeKind::Paragraph);
            let scoped = comparison
                .key_presence
                .as_ref()
                .expect("keys")
                .scoped
                .as_ref()
                .expect("scoped");
            assert!(scoped.witnesses[operations[0].witness].scope > 0);
            for node in &mut a.nodes {
                if node.kind == NodeKind::Section {
                    node.basis = ViewBasis::ReconstructedStructure;
                }
            }
            assert_eq!(
                compare_graphs(&old, &new, &a, &b, Default::default())
                    .keyed_element_operations()
                    .count(),
                0
            );
        } else {
            assert!(operations.is_empty());
            let scoped = comparison
                .key_presence
                .expect("keys")
                .scoped
                .expect("scoped");
            assert!(scoped.claims.iter().any(|claim| matches!(
                claim.counterpart,
                ScopedKeyCounterpart::OutsideScope { .. }
            )));
        }
    }
}

fn native_pdf_store(revision: &str, field_names: &[&str], paragraph_ids: &[&str]) -> EvidenceStore {
    use lopdf::{Document as Pdf, Object, Stream, dictionary};
    use pdfdelta_core::{
        document::{PageEvidence, extract_form_evidence, extract_structure_evidence},
        model::PageId,
        pdf::{LopdfParser, PdfParser},
        source::{ContentStreamGlyphExtractor, GlyphExtractor},
    };
    let mut pdf = Pdf::with_version("1.7");
    let pages = pdf.new_object_id();
    let page = pdf.new_object_id();
    let structure_root = pdf.new_object_id();
    let font = pdf.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
    let content: String = paragraph_ids
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                "/P << /MCID {index} >> BDC BT /F1 10 Tf 1 0 0 1 10 {} Tm (Body) Tj ET EMC\n",
                80 - index * 12
            )
        })
        .collect();
    let contents = pdf.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    pdf.objects.insert(page, Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![Object::from(0), 0.into(), 100.into(), 100.into()], "Contents" => contents,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } } }));
    pdf.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }));
    let tags: Vec<_> = paragraph_ids
        .iter()
        .enumerate()
        .map(|(index, key)| {
            Object::Reference(pdf.add_object(dictionary! {
                "Type" => "StructElem", "S" => "P", "P" => structure_root, "Pg" => page,
                "ID" => Object::string_literal(*key), "K" => index as i64,
            }))
        })
        .collect();
    pdf.objects.insert(
        structure_root,
        Object::Dictionary(dictionary! { "Type" => "StructTreeRoot", "K" => tags }),
    );
    let fields: Vec<_> = field_names.iter().map(|name| Object::Reference(pdf.add_object(dictionary! {
        "FT" => "Tx", "T" => Object::string_literal(*name), "V" => Object::string_literal("Stored value"),
    }))).collect();
    let catalog = pdf.add_object(
        dictionary! { "Type" => "Catalog", "Pages" => pages, "StructTreeRoot" => structure_root,
        "AcroForm" => dictionary! { "Fields" => fields } },
    );
    pdf.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).expect("PDF fixture");
    let parsed = LopdfParser
        .parse(bytes.into(), Default::default())
        .expect("parsed PDF");
    let extraction = ContentStreamGlyphExtractor
        .extract_outcome(parsed.as_ref(), Default::default())
        .expect("native glyphs");
    let backend = fields_backend();
    let mut store = EvidenceStore::from_native(
        revision.into(),
        backend,
        vec![PageEvidence {
            page: PageId(0),
            bounds: None,
        }],
        extraction,
        Default::default(),
    )
    .expect("native evidence");
    let forms = extract_form_evidence(parsed.as_ref(), 0, 0, Default::default()).expect("forms");
    let structure = extract_structure_evidence(
        parsed.as_ref(),
        &store.native,
        0,
        forms.fields.len() as u64,
        Default::default(),
    )
    .expect("structure");
    assert!(forms.key_inventory.complete && structure.key_inventory.complete);
    store.structured.extend(forms.fields);
    store.structured.extend(structure.elements);
    store
        .inventories
        .extend([forms.inventory, structure.inventory]);
    store
        .key_inventories
        .extend([forms.key_inventory, structure.key_inventory]);
    store.issues.extend(forms.issues);
    store.issues.extend(structure.issues);
    store
}

fn fields_backend() -> BackendIdentity {
    BackendIdentity {
        kind: BackendKind::NativeParser,
        name: "fixture".into(),
        version: "1".into(),
        profile: "raw-keys".into(),
        model: None,
    }
}

#[test]
fn native_pdf_fields_and_tagged_paragraphs_reach_scoped_membership_operations() {
    use pdfdelta_core::document::{
        Channel, DocumentComparisonLimits, KeyedElementOperationKind, MatchingChannels, NodeKind,
        SourceRef,
    };
    for paragraphs in [false, true] {
        let old = native_pdf_store(
            "old",
            if paragraphs { &[] } else { &["a"] },
            if paragraphs { &["a"] } else { &[] },
        );
        let new = native_pdf_store(
            "new",
            if paragraphs { &[] } else { &["a", "b"] },
            if paragraphs { &["a", "b"] } else { &[] },
        );
        for (old, new, kind) in [
            (&old, &new, KeyedElementOperationKind::Inserted),
            (&new, &old, KeyedElementOperationKind::Removed),
        ] {
            let (a, b) = graphs(old, new);
            let comparison = compare_graphs(old, new, &a, &b, Default::default());
            let operations: Vec<_> = comparison.keyed_element_operations().collect();
            assert_eq!(
                operations.len(),
                1,
                "{paragraphs}: {:?}",
                comparison.key_presence
            );
            assert_eq!(operations[0].kind, kind);
            assert_eq!(
                operations[0].node_kind,
                if paragraphs {
                    NodeKind::Paragraph
                } else {
                    NodeKind::Field
                }
            );
            assert!(matches!(
                operations[0].identity_source,
                SourceRef::Structured { .. }
            ));
            assert_eq!(
                operations[0]
                    .review_sources
                    .iter()
                    .any(|source| matches!(source, SourceRef::Native { .. })),
                paragraphs
            );
            if paragraphs {
                let mut limits = DocumentComparisonLimits::default();
                limits.matching.channels = MatchingChannels::from(&BTreeSet::from([Channel::Text]));
                let text_only = compare_graphs(old, new, &a, &b, limits);
                assert_eq!(text_only.keyed_element_operations().count(), 0);
            }
        }
    }
}

#[test]
fn only_closed_populations_prove_key_absence() {
    for (old, new, complete, absent, reason) in [
        (vec!["a", "b"], vec!["a"], true, 1, None),
        (vec!["a"], vec!["a", "b"], true, 1, None),
        (vec!["a"], vec![], true, 1, None),
        (
            vec!["a"],
            vec![],
            false,
            0,
            Some(MissingKeyEvidence::CompletePopulation),
        ),
        (
            vec!["a"],
            vec!["b"],
            true,
            0,
            Some(MissingKeyEvidence::RenameOrRepartitionResolution),
        ),
        (
            vec!["a", "a"],
            vec![],
            true,
            0,
            Some(MissingKeyEvidence::UniqueKeys),
        ),
        (
            vec!["a"],
            vec![""],
            true,
            0,
            Some(MissingKeyEvidence::NamedMembers),
        ),
        (vec!["a", "b"], vec!["b", "a"], true, 0, None),
    ] {
        let old = fields("old", &old, true);
        let new = fields("new", &new, complete);
        let comparison = compare_document_keys(
            &old,
            &new,
            &BTreeSet::from([KeyDomain::PdfFieldName]),
            KeyPresenceLimits::default(),
        );
        assert!(comparison.exhaustive);
        assert_eq!(
            comparison
                .claims
                .iter()
                .filter(|claim| matches!(
                    claim.counterpart,
                    KeyCounterpart::AbsentInDocument { .. }
                ))
                .count(),
            absent
        );
        if let Some(reason) = reason {
            assert!(
                comparison
                    .obligations
                    .iter()
                    .any(|obligation| obligation.missing == reason)
            );
        } else {
            assert!(comparison.obligations.is_empty());
        }
        for claim in &comparison.claims {
            if let KeyCounterpart::AbsentInDocument { population } = claim.counterpart {
                let witness = &comparison.populations[population];
                assert_eq!(
                    witness.revision,
                    if claim.side == PresenceSide::Old {
                        "new"
                    } else {
                        "old"
                    }
                );
            }
        }
    }
}

#[test]
fn missing_inventory_and_changed_evidence_invalidate_absence() {
    let old = fields("old", &["a"], true);
    let mut new = fields("new", &[], true);
    let domains = BTreeSet::from([KeyDomain::PdfFieldName]);
    let before = compare_document_keys(&old, &new, &domains, KeyPresenceLimits::default());
    assert!(matches!(
        before.claims[0].counterpart,
        KeyCounterpart::AbsentInDocument { .. }
    ));
    new.key_inventories.clear();
    let missing = compare_document_keys(&old, &new, &domains, KeyPresenceLimits::default());
    assert!(matches!(
        missing.claims[0].counterpart,
        KeyCounterpart::PresentOnly
    ));
    new = fields("reacquired", &["a"], true);
    let changed = compare_document_keys(&old, &new, &domains, KeyPresenceLimits::default());
    assert!(matches!(
        changed.claims[0].counterpart,
        KeyCounterpart::Matched { .. }
    ));
    assert_eq!(changed.new_revision, "reacquired");
    let limited = compare_document_keys(&old, &new, &domains, KeyPresenceLimits { max_work: 0 });
    assert!(!limited.exhaustive);
    assert!(limited.claims.is_empty());
    assert_eq!(
        limited.obligations[0].missing,
        MissingKeyEvidence::RivalSearch
    );
}

#[test]
fn key_inventory_rejects_duplicate_domains_and_non_native_backends() {
    let mut store = fields("old", &["a"], true);
    store.key_inventories.push(store.key_inventories[0].clone());
    assert!(store.validate(EvidenceLimits::default()).is_err());
    store.key_inventories.pop();
    store.backends[0].kind = BackendKind::StructureModel;
    assert!(store.validate(EvidenceLimits::default()).is_err());
}
