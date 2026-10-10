use crate::{
    Result,
    document::{
        NativeStructureKid, StructuredValue, extract_structure_evidence, with_native_order_proof,
    },
    model::{
        DecodedText, Document, FontId, Glyph, GlyphCropStatus, GlyphId, GlyphPathClipStatus,
        GlyphProvenance, MarkedContent, PageId, Rect, TextRenderMode, Vec2,
    },
    pdf::{
        DecodedStream, ObjectRef, PageRef, ParsedPdf, PdfDict, PdfObject, PdfVersion, RawStream,
    },
};

fn object(number: u32) -> ObjectRef {
    ObjectRef {
        object_number: number,
        generation: 0,
    }
}

fn dictionary(entries: Vec<(&str, PdfObject)>) -> PdfObject {
    PdfObject::Dictionary(
        entries
            .into_iter()
            .map(|(key, value)| (key.as_bytes().to_vec(), value))
            .collect(),
    )
}

fn name(value: &str) -> PdfObject {
    PdfObject::Name(value.as_bytes().to_vec())
}

#[derive(Clone)]
struct AnnotationPdf {
    annotation: PdfDict,
}

impl AnnotationPdf {
    fn link(explicit_type: bool) -> Self {
        let PdfObject::Dictionary(mut annotation) = dictionary(vec![
            ("Subtype", name("Link")),
            (
                "Rect",
                PdfObject::Array(vec![
                    PdfObject::Integer(0),
                    PdfObject::Real(1.5),
                    PdfObject::Integer(20),
                    PdfObject::Integer(30),
                ]),
            ),
            ("StructParent", PdfObject::Integer(1)),
        ]) else {
            unreachable!()
        };
        if explicit_type {
            annotation.insert(b"Type".to_vec(), name("Annot"));
        }
        Self { annotation }
    }
}

impl ParsedPdf for AnnotationPdf {
    fn version(&self) -> PdfVersion {
        PdfVersion { major: 1, minor: 7 }
    }
    fn trailer(&self) -> Result<PdfDict> {
        Ok([(b"Root".to_vec(), PdfObject::Reference(object(1)))]
            .into_iter()
            .collect())
    }
    fn resolve(&self, reference: ObjectRef) -> Result<PdfObject> {
        use PdfObject as P;
        Ok(match reference.object_number {
            1 => dictionary(vec![
                ("Type", name("Catalog")),
                ("StructTreeRoot", P::Reference(object(2))),
            ]),
            2 => dictionary(vec![
                ("Type", name("StructTreeRoot")),
                ("K", P::Reference(object(3))),
                (
                    "ParentTree",
                    dictionary(vec![(
                        "Nums",
                        P::Array(vec![
                            P::Integer(0),
                            P::Array(vec![P::Reference(object(3)), P::Reference(object(3))]),
                            P::Integer(1),
                            P::Reference(object(3)),
                        ]),
                    )]),
                ),
            ]),
            3 => dictionary(vec![
                ("Type", name("StructElem")),
                ("S", name("P")),
                ("P", P::Reference(object(2))),
                ("Pg", P::Reference(object(4))),
                (
                    "K",
                    P::Array(vec![
                        P::Integer(0),
                        dictionary(vec![
                            ("Type", name("OBJR")),
                            ("Obj", P::Reference(object(6))),
                            ("Pg", P::Reference(object(4))),
                        ]),
                        P::Integer(1),
                    ]),
                ),
            ]),
            4 => dictionary(vec![
                ("Type", name("Page")),
                ("StructParents", P::Integer(0)),
                ("Annots", P::Array(vec![P::Reference(object(6))])),
            ]),
            5 => dictionary(vec![("Type", name("Page"))]),
            6 => P::Dictionary(self.annotation.clone()),
            _ => {
                return Err(crate::Error::Unsupported(
                    "annotation fixture has no other objects".into(),
                ));
            }
        })
    }
    fn pages(&self) -> Result<Vec<PageRef>> {
        Ok(vec![PageRef(object(4)), PageRef(object(5))])
    }
    fn page_dict(&self, page: PageRef) -> Result<PdfDict> {
        let PdfObject::Dictionary(dictionary) = self.resolve(page.0)? else {
            unreachable!()
        };
        Ok(dictionary)
    }
    fn raw_stream(&self, _: ObjectRef) -> Result<RawStream> {
        Err(crate::Error::Unsupported(
            "annotation fixture has no streams".into(),
        ))
    }
    fn decoded_stream(&self, _: ObjectRef) -> Result<DecodedStream> {
        Err(crate::Error::Unsupported(
            "annotation fixture has no streams".into(),
        ))
    }
}

fn native() -> Document<Glyph> {
    Document::new(
        (0_u32..2)
            .map(|index| Glyph {
                id: GlyphId(u64::from(index) + 1),
                text: DecodedText::Mapped(if index == 0 { "A" } else { "B" }.into()),
                raw_code: vec![65 + index as u8],
                page: PageId(0),
                bbox: Rect {
                    min: Vec2 {
                        x: f64::from(index) * 10.0,
                        y: 20.0,
                    },
                    max: Vec2 {
                        x: f64::from(index) * 10.0 + 6.0,
                        y: 30.0,
                    },
                },
                baseline: Vec2 {
                    x: f64::from(index) * 10.0,
                    y: 20.0,
                },
                direction: Vec2 { x: 1.0, y: 0.0 },
                font_id: FontId(1),
                font_size: 10.0,
                render_order: index,
                render_mode: TextRenderMode::Fill,
                crop_status: GlyphCropStatus::Inside,
                path_clip_status: GlyphPathClipStatus::Unclipped,
                provenance: GlyphProvenance {
                    content_stream: object(10),
                    operator_index: index,
                },
            })
            .collect(),
    )
    .with_marked_content(
        (0..2)
            .map(|index| MarkedContent {
                page: PageId(0),
                form: None,
                mcid: index as u32,
                glyph_range: index..index + 1,
                complete: true,
            })
            .collect(),
    )
}

fn assert_acquisition(pdf: AnnotationPdf, expected: bool, budget: usize) {
    let native = native();
    let before = native.clone();
    let tags = extract_structure_evidence(&pdf, &native, 0, 0, Default::default())
        .expect("bounded annotation fixture evidence");
    assert_eq!(tags.native_inventory.complete, expected);
    let StructuredValue::StructureElement {
        content: Some(content),
        ..
    } = &tags.elements[0].value
    else {
        panic!("retained ordered contents")
    };
    assert_eq!(
        content[1],
        if expected {
            NativeStructureKid::Annotation {
                object: object(6),
                page: PageId(0),
            }
        } else {
            NativeStructureKid::Unresolved
        }
    );
    assert!(!tags.inventory.complete);
    let mut remaining = budget;
    with_native_order_proof(
        Some(Box::new(pdf)),
        native,
        &mut remaining,
        |native, proof, _| {
            assert_eq!(*native, before);
            assert_eq!(proof.is_some(), expected && budget > 1);
            if let Some(proof) = proof {
                assert!(proof.binds(native));
                assert!(!proof.binds(&before));
                // The annotation remains a barrier and contributes no physical glyph.
                assert_eq!(proof.runs(), &[vec![GlyphId(1)], vec![GlyphId(2)]]);
            }
        },
    );
    if budget > 1 {
        assert!(remaining < budget);
    } else {
        assert_eq!(remaining, budget);
    }
}

#[test]
fn omitted_link_annotation_type_preserves_the_non_glyph_barrier() {
    assert_acquisition(AnnotationPdf::link(false), true, 32_000_000);
}

#[test]
fn explicit_annotation_keeps_existing_acquisition_and_budget_refusal() {
    let mut pdf = AnnotationPdf::link(true);
    pdf.annotation.remove(b"Rect".as_slice());
    assert_acquisition(pdf, true, 32_000_000);
    assert_acquisition(AnnotationPdf::link(false), true, 1);
}

#[test]
fn omitted_type_requires_a_known_link_and_a_finite_rectangle() {
    for (key, value) in [
        ("Type", name("XObject")),
        ("Type", PdfObject::Null),
        ("Subtype", name("Form")),
        ("Subtype", name("Widget")),
        ("Subtype", PdfObject::Null),
        ("Rect", PdfObject::Null),
        ("Rect", PdfObject::Array(vec![PdfObject::Integer(0); 3])),
        ("Rect", PdfObject::Array(vec![PdfObject::Real(f64::NAN); 4])),
        (
            "Rect",
            PdfObject::Array(vec![PdfObject::Real(f64::INFINITY); 4]),
        ),
        ("Rect", PdfObject::Array(vec![name("0"); 4])),
        ("StructParents", PdfObject::Integer(1)),
        ("P", PdfObject::Reference(object(5))),
    ] {
        let mut pdf = AnnotationPdf::link(false);
        pdf.annotation.insert(key.as_bytes().to_vec(), value);
        assert_acquisition(pdf, false, 32_000_000);
    }
    for key in ["Subtype", "Rect"] {
        let mut pdf = AnnotationPdf::link(false);
        pdf.annotation.remove(key.as_bytes());
        assert_acquisition(pdf, false, 32_000_000);
    }
}
