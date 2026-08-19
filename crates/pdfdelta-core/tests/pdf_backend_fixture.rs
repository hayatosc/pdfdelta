use std::{io::Write as _, sync::Arc};

use lopdf::{
    Document, EncryptionState, EncryptionVersion, Object, Permissions, Stream, dictionary,
    xref::XrefType,
};
use pdfdelta_core::{
    Error,
    pdf::{LopdfParser, ObjectRef, ParseLimits, PdfObject, PdfParser, PdfVersion},
};

const CONTENT: &[u8] = b"BT /F1 12 Tf 72 720 Td (Project archive) Tj ET\n\
BT /F1 12 Tf 72 700 Td (Quarterly summary) Tj ET\n\
BT /F1 12 Tf 72 680 Td (Project archive) Tj ET\n\
BT /F1 12 Tf 72 660 Td (Quarterly summary) Tj ET\n\
BT /F1 12 Tf 72 640 Td (Project archive) Tj ET\n\
BT /F1 12 Tf 72 620 Td (Quarterly summary) Tj ET\n";

#[derive(Clone, Copy)]
struct FixtureIds {
    catalog: lopdf::ObjectId,
    content: lopdf::ObjectId,
    marker: lopdf::ObjectId,
    page: lopdf::ObjectId,
    pages: lopdf::ObjectId,
    resources: lopdf::ObjectId,
}

fn limits() -> ParseLimits {
    ParseLimits {
        max_input_bytes: 1024 * 1024,
        max_objects: 100,
        max_recursion_depth: 16,
        max_decoded_stream_bytes: 1024 * 1024,
        max_total_object_stream_bytes: 4 * 1024 * 1024,
        max_pages: 10,
    }
}

fn fixture_document(page_count: usize) -> (Document, FixtureIds) {
    let mut document = Document::with_version("1.4");
    let pages = document.new_object_id();
    let font = document.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources = document.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font },
    });

    let mut content_stream = Stream::new(dictionary! {}, CONTENT.to_vec());
    content_stream
        .compress()
        .expect("fixture content should compress");
    let content = document.add_object(content_stream);
    let marker = document.add_object(dictionary! {
        "Kind" => "ArchiveEntry",
        "Value" => Object::string_literal("initial"),
        "Details" => dictionary! { "Revision" => 1 },
    });

    let mut page_ids = Vec::new();
    for _ in 0..page_count {
        page_ids.push(document.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages,
            "Contents" => content,
        }));
    }
    document.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids.iter().copied().map(Object::Reference).collect::<Vec<_>>(),
            "Count" => page_count as i64,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
        }),
    );
    let catalog = document.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages,
    });
    document.trailer.set("Root", catalog);

    (
        document,
        FixtureIds {
            catalog,
            content,
            marker,
            page: page_ids[0],
            pages,
            resources,
        },
    )
}

fn classic_fixture(page_count: usize) -> (Vec<u8>, FixtureIds) {
    let (document, ids) = fixture_document(page_count);
    (serialize_classic(document), ids)
}

fn serialize_classic(mut document: Document) -> Vec<u8> {
    document.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("classic fixture should serialize");
    bytes
}

fn parse(
    bytes: Vec<u8>,
    limits: ParseLimits,
) -> pdfdelta_core::Result<Box<dyn pdfdelta_core::pdf::ParsedPdf>> {
    LopdfParser.parse(Arc::from(bytes), limits)
}

fn handwritten_modern_fixture() -> Vec<u8> {
    handwritten_modern_fixture_with_stale(None, "initial")
}

fn handwritten_modern_fixture_with_stale(
    stale_value: Option<&str>,
    current_value: &str,
) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n%\xFF\xFF\xFF\xFF\n".to_vec();
    let catalog = append_object(&mut bytes, 1, b"<< /Type /Catalog /Pages 2 0 R >>");
    let pages = append_object(
        &mut bytes,
        2,
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << >> /MediaBox [0 0 612 792] >>",
    );
    let page = append_object(
        &mut bytes,
        3,
        b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>",
    );
    let content = append_stream_object(&mut bytes, 4, b"<< /Length 6 >>", b"BT ET\n");
    let stale_object_stream = stale_value.map(|value| append_object_stream(&mut bytes, 6, value));
    let object_stream_id = if stale_object_stream.is_some() { 7 } else { 6 };
    let object_stream = append_object_stream(&mut bytes, object_stream_id, current_value);

    let xref_id = object_stream_id + 1;
    let xref_offset = bytes.len();
    let mut entries = vec![
        (0, 0, u16::MAX),
        (1, catalog, 0),
        (1, pages, 0),
        (1, page, 0),
        (1, content, 0),
        (2, object_stream_id as usize, 0),
    ];
    if let Some(stale_object_stream) = stale_object_stream {
        entries.push((1, stale_object_stream, 0));
    }
    entries.push((1, object_stream, 0));
    entries.push((1, xref_offset, 0));
    assert_eq!(entries.len(), xref_id as usize + 1);
    write!(
        bytes,
        "{xref_id} 0 obj\n<< /Type /XRef /Size {} /Root 1 0 R /W [1 4 2] /Index [0 {}] /Length {} >>\nstream\n",
        entries.len(),
        entries.len(),
        entries.len() * 7
    )
    .expect("xref stream header should serialize");
    for (kind, field_two, field_three) in entries {
        bytes.push(kind);
        bytes.extend_from_slice(
            &u32::try_from(field_two)
                .expect("fixture offset should fit in u32")
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&field_three.to_be_bytes());
    }
    write!(
        bytes,
        "\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("xref stream trailer should serialize");
    bytes
}

fn append_object_stream(bytes: &mut Vec<u8>, object_number: u32, value: &str) -> usize {
    let object_stream_content = format!(
        "5 0 << /Kind /ArchiveEntry /Value ({value}) /Padding ({}) >>",
        "a".repeat(256)
    );
    let mut stream = Stream::new(dictionary! {}, object_stream_content.into_bytes());
    stream
        .compress()
        .expect("object-stream fixture should compress");
    assert!(stream.dict.get(b"Filter").is_ok());
    let object_stream_dictionary = format!(
        "<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length {} >>",
        stream.content.len()
    );
    append_stream_object(
        bytes,
        object_number,
        object_stream_dictionary.as_bytes(),
        &stream.content,
    )
}

fn compressed_empty_object_stream(decoded_bytes: usize) -> Stream {
    let mut stream = Stream::new(
        dictionary! {
            "Type" => "ObjStm",
            "N" => 0,
            "First" => 0,
        },
        vec![b' '; decoded_bytes],
    );
    stream
        .compress()
        .expect("empty object-stream fixture should compress");
    assert!(stream.dict.get(b"Filter").is_ok());
    stream
}

fn handwritten_object_stream_budget_fixture(decoded_bytes: usize) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n%\xFF\xFF\xFF\xFF\n".to_vec();
    let mut offsets = vec![
        append_object(&mut bytes, 1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        append_object(
            &mut bytes,
            2,
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Resources << >> /MediaBox [0 0 612 792] >>",
        ),
        append_object(
            &mut bytes,
            3,
            b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>",
        ),
        append_stream_object(&mut bytes, 4, b"<< /Length 6 >>", b"BT ET\n"),
    ];
    for object_number in 5..=6 {
        let stream = compressed_empty_object_stream(decoded_bytes);
        let dictionary = format!(
            "<< /Type /ObjStm /N 0 /First 0 /Filter /FlateDecode /Length {} >>",
            stream.content.len()
        );
        offsets.push(append_stream_object(
            &mut bytes,
            object_number,
            dictionary.as_bytes(),
            &stream.content,
        ));
    }

    let xref_offset = bytes.len();
    bytes.extend_from_slice(b"xref\n0 7\n0000000000 65535 f \n");
    for offset in offsets {
        writeln!(bytes, "{offset:010} 00000 n ").expect("xref entry should serialize");
    }
    write!(
        bytes,
        "trailer\n<< /Size 7 /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("xref trailer should serialize");
    bytes
}

fn malformed_xref_stream_fixture(field_width: i64, entry_count: i64) -> Vec<u8> {
    let mut bytes = b"%PDF-1.5\n".to_vec();
    let xref_offset = bytes.len();
    write!(
        bytes,
        "1 0 obj\n<< /Type /XRef /Size 2 /W [0 {field_width} 0] /Index [0 {entry_count}] /Length 0 >>\nstream\n\nendstream\nendobj\nstartxref\n{xref_offset}\n%%EOF\n"
    )
    .expect("malformed xref fixture should serialize");
    bytes
}

fn append_object(bytes: &mut Vec<u8>, object_number: u32, body: &[u8]) -> usize {
    let offset = bytes.len();
    writeln!(bytes, "{object_number} 0 obj").expect("object header should serialize");
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(b"\nendobj\n");
    offset
}

fn append_stream_object(
    bytes: &mut Vec<u8>,
    object_number: u32,
    dictionary: &[u8],
    content: &[u8],
) -> usize {
    let offset = bytes.len();
    writeln!(bytes, "{object_number} 0 obj").expect("stream header should serialize");
    bytes.extend_from_slice(dictionary);
    bytes.extend_from_slice(b"\nstream\n");
    bytes.extend_from_slice(content);
    bytes.extend_from_slice(b"\nendstream\nendobj\n");
    offset
}

fn append_incremental_update(mut bytes: Vec<u8>, ids: FixtureIds) -> Vec<u8> {
    let previous_xref = last_startxref(&bytes);
    bytes.push(b'\n');
    let object_offset = append_object(
        &mut bytes,
        ids.marker.0,
        b"<< /Kind /ArchiveEntry /Value (latest) >>",
    );
    let xref_offset = bytes.len();
    write!(
        bytes,
        "xref\n{} 1\n{object_offset:010} 00000 n \ntrailer\n<< /Size {} /Root {} 0 R /Prev {previous_xref} >>\nstartxref\n{xref_offset}\n%%EOF\n",
        ids.marker.0,
        ids.catalog.0 + 1,
        ids.catalog.0,
    )
    .expect("incremental revision should serialize");
    bytes
}

fn last_startxref(bytes: &[u8]) -> usize {
    let marker = b"startxref\n";
    let marker_offset = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("fixture should contain startxref");
    let value = &bytes[marker_offset + marker.len()..];
    let digit_count = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    std::str::from_utf8(&value[..digit_count])
        .expect("startxref should be ASCII")
        .parse()
        .expect("startxref should contain an offset")
}

#[test]
fn parses_classic_xref_and_preserves_stream_evidence_and_page_inheritance() {
    let (bytes, ids) = classic_fixture(1);
    assert!(bytes.windows(b"xref".len()).any(|window| window == b"xref"));

    let pdf = parse(bytes, limits()).expect("classic PDF should parse");
    assert_eq!(pdf.version(), PdfVersion { major: 1, minor: 4 });
    assert_eq!(
        pdf.pages().expect("page tree should resolve"),
        vec![pdfdelta_core::pdf::PageRef(object_ref(ids.page))]
    );

    let page = pdf
        .page_dict(pdfdelta_core::pdf::PageRef(object_ref(ids.page)))
        .expect("page dictionary should resolve");
    assert_eq!(
        page.get(b"Resources".as_slice()),
        Some(&PdfObject::Reference(object_ref(ids.resources)))
    );
    assert!(matches!(
        page.get(b"MediaBox".as_slice()),
        Some(PdfObject::Array(_))
    ));

    let raw = pdf
        .raw_stream(object_ref(ids.content))
        .expect("raw stream should be available");
    let decoded = pdf
        .decoded_stream(object_ref(ids.content))
        .expect("FlateDecode stream should decode");
    assert_ne!(raw.bytes, CONTENT);
    assert_eq!(decoded.bytes, CONTENT);
    assert_eq!(
        raw.dictionary.get(b"Filter".as_slice()),
        Some(&PdfObject::Name(b"FlateDecode".to_vec()))
    );

    let marker = pdf
        .resolve(object_ref(ids.marker))
        .expect("indirect object should resolve");
    assert!(matches!(marker, PdfObject::Dictionary(_)));
    assert!(matches!(
        pdf.trailer()
            .expect("trailer should convert")
            .get(b"Root".as_slice()),
        Some(PdfObject::Reference(_))
    ));
}

#[test]
fn shares_inherited_page_resources_without_changing_page_dict() {
    let (mut document, ids) = fixture_document(3);
    let page_ids = document
        .objects
        .get(&ids.pages)
        .expect("Pages root should exist")
        .as_dict()
        .expect("Pages root should be a dictionary")
        .get(b"Kids")
        .expect("Pages root should contain Kids")
        .as_array()
        .expect("Kids should be an array")
        .iter()
        .map(|object| object.as_reference().expect("Kid should be a reference"))
        .collect::<Vec<_>>();
    document
        .objects
        .get_mut(&ids.pages)
        .expect("Pages root should exist")
        .as_dict_mut()
        .expect("Pages root should be a dictionary")
        .set(
            "Resources",
            dictionary! { "Large" => vec![Object::Integer(7); 128] },
        );
    document
        .objects
        .get_mut(&page_ids[2])
        .expect("leaf page should exist")
        .as_dict_mut()
        .expect("leaf page should be a dictionary")
        .set("Resources", dictionary! { "Marker" => "Leaf" });

    let pdf = parse(serialize_classic(document), limits()).expect("fixture should parse");
    let pages = pdf.pages().expect("page tree should resolve");
    let first = pdf
        .page_snapshot(pages[0])
        .expect("first page snapshot should resolve");
    let second = pdf
        .page_snapshot(pages[1])
        .expect("second page snapshot should resolve");
    let leaf = pdf
        .page_snapshot(pages[2])
        .expect("leaf page snapshot should resolve");
    let first_resources = first.resources.as_ref().expect("resources should inherit");
    let second_resources = second.resources.as_ref().expect("resources should inherit");
    let leaf_resources = leaf
        .resources
        .as_ref()
        .expect("leaf resources should resolve");

    assert!(Arc::ptr_eq(first_resources, second_resources));
    assert!(!Arc::ptr_eq(first_resources, leaf_resources));
    assert!(!first.dictionary.contains_key(b"Resources".as_slice()));
    assert!(!second.dictionary.contains_key(b"Resources".as_slice()));
    assert!(!leaf.dictionary.contains_key(b"Resources".as_slice()));
    assert_eq!(
        pdf.page_dict(pages[0])
            .expect("page dictionary should resolve")
            .get(b"Resources".as_slice()),
        Some(first_resources.as_ref())
    );
    assert!(matches!(
        leaf_resources.as_ref(),
        PdfObject::Dictionary(dictionary)
            if dictionary.get(b"Marker".as_slice())
                == Some(&PdfObject::Name(b"Leaf".to_vec()))
    ));
}

#[test]
fn rejects_page_snapshots_outside_the_page_tree() {
    let (bytes, ids) = classic_fixture(1);
    let pdf = parse(bytes, limits()).expect("fixture should parse");

    assert!(matches!(
        pdf.page_snapshot(pdfdelta_core::pdf::PageRef(object_ref(ids.marker))),
        Err(Error::Backend(message)) if message.contains("page is not in the page tree")
    ));
}

#[test]
fn parses_xref_and_object_streams() {
    let bytes = handwritten_modern_fixture();
    assert!(
        bytes
            .windows(b"ObjStm".len())
            .any(|window| window == b"ObjStm")
    );
    assert!(
        bytes
            .windows(b"/XRef".len())
            .any(|window| window == b"/XRef")
    );

    let pdf = parse(bytes, limits()).expect("modern PDF should parse");
    assert_eq!(pdf.version(), PdfVersion { major: 1, minor: 5 });
    let marker = pdf
        .resolve(object_ref((5, 0)))
        .expect("object-stream entry should resolve");
    let PdfObject::Dictionary(marker) = marker else {
        panic!("marker should remain a dictionary");
    };
    assert_eq!(
        marker.get(b"Value".as_slice()),
        Some(&PdfObject::String(b"initial".to_vec()))
    );
    assert_eq!(
        pdf.decoded_stream(object_ref((4, 0)))
            .expect("modern content stream should decode")
            .bytes,
        b"BT ET\n"
    );

    let raw = pdf
        .raw_stream(object_ref((6, 0)))
        .expect("encoded object stream should remain available");
    assert_eq!(
        raw.dictionary.get(b"Filter".as_slice()),
        Some(&PdfObject::Name(b"FlateDecode".to_vec()))
    );
    let decoded = pdf
        .decoded_stream(object_ref((6, 0)))
        .expect("object stream should decode on demand");
    assert_ne!(raw.bytes, decoded.bytes);
}

#[test]
fn reports_the_terminal_reference_after_resolving_an_alias_chain() {
    let (mut document, _) = fixture_document(1);
    let terminal = document.add_object(dictionary! {
        "Kind" => "SharedResource",
    });
    let second_alias = document.add_object(Object::Reference(terminal));
    let first_alias = document.add_object(Object::Reference(second_alias));
    let pdf = parse(serialize_classic(document), limits()).expect("fixture should parse");

    assert_eq!(
        pdf.terminal_reference(object_ref(first_alias))
            .expect("terminal reference should resolve"),
        object_ref(terminal)
    );
    let resolved = pdf
        .resolve_with_terminal(object_ref(first_alias))
        .expect("alias chain should resolve");
    assert_eq!(resolved.reference, object_ref(terminal));
    assert!(matches!(resolved.object, PdfObject::Dictionary(_)));
}

#[test]
fn accepts_stale_and_current_object_streams_with_the_same_embedded_id() {
    let pdf = parse(
        handwritten_modern_fixture_with_stale(Some("stale"), "current"),
        limits(),
    )
    .expect("stale object-stream entries should not invalidate the current revision");
    let marker = pdf
        .resolve(object_ref((5, 0)))
        .expect("current object-stream entry should resolve");
    let PdfObject::Dictionary(marker) = marker else {
        panic!("marker should remain a dictionary");
    };
    assert_eq!(
        marker.get(b"Value".as_slice()),
        Some(&PdfObject::String(b"current".to_vec()))
    );
}

#[test]
fn resolves_the_latest_incremental_revision() {
    let (bytes, ids) = classic_fixture(1);
    let bytes = append_incremental_update(bytes, ids);

    let pdf = parse(bytes, limits()).expect("incremental PDF should parse");
    let marker = pdf
        .resolve(object_ref(ids.marker))
        .expect("latest object should resolve");
    let PdfObject::Dictionary(marker) = marker else {
        panic!("marker should remain a dictionary");
    };
    assert_eq!(
        marker.get(b"Value".as_slice()),
        Some(&PdfObject::String(b"latest".to_vec()))
    );
}

#[test]
fn rejects_encrypted_documents_as_unsupported() {
    for user_password in ["user", ""] {
        let (mut document, _) = fixture_document(1);
        document.trailer.set(
            "ID",
            Object::Array(vec![
                Object::string_literal(vec![1_u8; 16]),
                Object::string_literal(vec![2_u8; 16]),
            ]),
        );
        let encryption = EncryptionState::try_from(EncryptionVersion::V1 {
            document: &document,
            owner_password: "owner",
            user_password,
            permissions: Permissions::all(),
        })
        .expect("fixture encryption state should build");
        document
            .encrypt(&encryption)
            .expect("fixture should encrypt");
        let mut bytes = Vec::new();
        document
            .save_to(&mut bytes)
            .expect("encrypted fixture should serialize");

        let Err(error) = parse(bytes, limits()) else {
            panic!("encrypted PDF should be unsupported");
        };
        assert!(matches!(error, Error::Unsupported(_)));
    }
}

#[test]
fn enforces_input_object_page_and_decode_limits() {
    let (bytes, ids) = classic_fixture(2);

    let mut constrained = limits();
    constrained.max_input_bytes = bytes.len() - 1;
    assert!(matches!(
        parse(bytes.clone(), constrained),
        Err(Error::LimitExceeded {
            resource: "PDF input bytes",
            ..
        })
    ));

    constrained = limits();
    constrained.max_objects = 1;
    assert!(matches!(
        parse(bytes.clone(), constrained),
        Err(Error::LimitExceeded {
            resource: "PDF object count",
            ..
        })
    ));

    constrained = limits();
    constrained.max_pages = 1;
    assert!(matches!(
        parse(bytes.clone(), constrained),
        Err(Error::LimitExceeded {
            resource: "PDF page count",
            ..
        })
    ));

    constrained = limits();
    constrained.max_decoded_stream_bytes = CONTENT.len() - 1;
    let pdf = parse(bytes, constrained).expect("content decoding should remain lazy");
    assert!(matches!(
        pdf.decoded_stream(object_ref(ids.content)),
        Err(Error::LimitExceeded {
            resource: "PDF decoded stream bytes",
            ..
        })
    ));
}

#[test]
fn rejects_invalid_zero_limits() {
    let (bytes, _) = classic_fixture(1);
    let mut constrained = limits();
    constrained.max_recursion_depth = 0;

    assert!(matches!(
        parse(bytes, constrained),
        Err(Error::InvalidConfiguration(_))
    ));
}

#[test]
fn applies_a_higher_catalog_version() {
    let (mut document, ids) = fixture_document(1);
    document
        .objects
        .get_mut(&ids.catalog)
        .expect("catalog should exist")
        .as_dict_mut()
        .expect("catalog should be a dictionary")
        .set("Version", "1.7");

    let pdf = parse(serialize_classic(document), limits()).expect("fixture should parse");
    assert_eq!(pdf.version(), PdfVersion { major: 1, minor: 7 });
}

#[test]
fn rejects_a_page_parent_outside_the_traversed_tree() {
    let (mut document, ids) = fixture_document(1);
    let unrelated_pages = document.add_object(dictionary! {
        "Type" => "Pages",
        "Kids" => Vec::<Object>::new(),
        "Count" => 0,
        "Resources" => dictionary! {},
        "MediaBox" => vec![0.into(), 0.into(), 300.into(), 300.into()],
    });
    document
        .objects
        .get_mut(&ids.page)
        .expect("page should exist")
        .as_dict_mut()
        .expect("page should be a dictionary")
        .set("Parent", unrelated_pages);

    assert!(matches!(
        parse(serialize_classic(document), limits()),
        Err(Error::Backend(message)) if message.contains("inconsistent Parent")
    ));
}

#[test]
fn bounds_page_tree_width_before_queueing_all_kids() {
    let (mut document, ids) = fixture_document(1);
    document
        .objects
        .get_mut(&ids.pages)
        .expect("Pages root should exist")
        .as_dict_mut()
        .expect("Pages root should be a dictionary")
        .set("Kids", vec![Object::Reference(ids.page); 200]);
    let mut constrained = limits();
    constrained.max_objects = 1_000;
    constrained.max_pages = 1;

    assert!(matches!(
        parse(serialize_classic(document), constrained),
        Err(Error::LimitExceeded {
            resource: "PDF page tree node count",
            ..
        })
    ));
}

#[test]
fn maps_the_xref_entry_limit_to_the_object_count_limit() {
    let mut constrained = limits();
    constrained.max_objects = 5;

    assert!(matches!(
        parse(handwritten_modern_fixture(), constrained),
        Err(Error::LimitExceeded {
            resource: "PDF object count",
            limit: 5,
        })
    ));
}

#[test]
fn bounds_aggregate_object_stream_decoding() {
    let bytes = handwritten_object_stream_budget_fixture(256);
    let mut constrained = limits();
    constrained.max_decoded_stream_bytes = 300;
    constrained.max_total_object_stream_bytes = 400;

    match parse(bytes, constrained) {
        Err(Error::LimitExceeded {
            resource: "PDF decoded object stream bytes",
            limit: 400,
        }) => {}
        Err(other) => panic!("expected aggregate object-stream limit, got {other:?}"),
        Ok(_) => panic!("aggregate object-stream limit should reject the PDF"),
    }
}

#[test]
fn rejects_an_object_stream_that_underreports_its_index_entries() {
    let mut bytes = handwritten_modern_fixture();
    let marker = b"/N 1 /First";
    let marker_offset = bytes
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("fixture should declare one object-stream entry");
    bytes[marker_offset + 3] = b'0';

    assert!(matches!(
        parse(bytes, limits()),
        Err(Error::Backend(message)) if message.contains("N declares 0 entries")
    ));
}

#[test]
fn rejects_oversized_and_zero_width_xref_stream_records() {
    for bytes in [
        malformed_xref_stream_fixture(i64::MAX, 1),
        malformed_xref_stream_fixture(0, i64::MAX),
    ] {
        assert!(matches!(parse(bytes, limits()), Err(Error::Backend(_))));
    }
}

#[test]
fn rejects_a_page_count_that_cannot_fit_the_configured_limit() {
    let (mut document, ids) = fixture_document(1);
    document
        .objects
        .get_mut(&ids.pages)
        .expect("Pages root should exist")
        .as_dict_mut()
        .expect("Pages root should be a dictionary")
        .set("Count", i64::MAX);

    assert!(matches!(
        parse(serialize_classic(document), limits()),
        Err(Error::LimitExceeded {
            resource: "PDF page count",
            ..
        })
    ));
}

#[test]
fn enforces_neutral_object_nesting_limit() {
    let (bytes, ids) = classic_fixture(1);
    let mut constrained = limits();
    constrained.max_recursion_depth = 1;

    let pdf = parse(bytes, constrained).expect("shallow page tree should parse");
    assert!(matches!(
        pdf.resolve(object_ref(ids.marker)),
        Err(Error::LimitExceeded {
            resource: "PDF object nesting depth",
            ..
        })
    ));
}

#[test]
fn classifies_unknown_stream_filters_as_unsupported() {
    let (mut document, ids) = fixture_document(1);
    document.reference_table.cross_reference_type = XrefType::CrossReferenceTable;
    document.objects.insert(
        ids.content,
        Object::Stream(Stream::new(
            dictionary! { "Filter" => "ArchiveDecode" },
            b"encoded archive content".to_vec(),
        )),
    );
    let mut bytes = Vec::new();
    document
        .save_to(&mut bytes)
        .expect("unknown-filter fixture should serialize");

    let pdf = parse(bytes, limits()).expect("unknown content filter should decode lazily");
    assert!(matches!(
        pdf.decoded_stream(object_ref(ids.content)),
        Err(Error::Unsupported(_))
    ));
}

fn object_ref(id: lopdf::ObjectId) -> ObjectRef {
    ObjectRef {
        object_number: id.0,
        generation: id.1,
    }
}
