// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration test — AcroForm fields (and their appearance streams)
//! must survive `compress()` — form data isn't something a size
//! optimizer should ever touch. See `common.rs` for shared
//! helpers.

use lopdf::{Dictionary, Document, Object, Stream};
use std::path::Path;

use super::common::{decoded_stream_content, resolve, temp_pdf};
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Builds a PDF with a one-field AcroForm: a text field widget
/// annotation on the page, with an (unfiltered) appearance stream.
/// Returns the exact appearance-stream bytes for later comparison.
fn build_fixture_with_acroform(path: &Path) -> Vec<u8> {
    let mut doc = Document::new();

    let appearance_content = b"/Tx BMC q 1 1 1 rg 0 0 100 20 re f Q EMC".to_vec();
    let mut ap_dict = Dictionary::new();
    ap_dict.set("Type", Object::Name(b"XObject".to_vec()));
    ap_dict.set("Subtype", Object::Name(b"Form".to_vec()));
    ap_dict.set(
        "BBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    ap_dict.set("Length", Object::Integer(appearance_content.len() as i64));
    let ap_id = doc.add_object(Object::Stream(Stream::new(
        ap_dict,
        appearance_content.clone(),
    )));

    let mut appearance = Dictionary::new();
    appearance.set("N", Object::Reference(ap_id));

    let mut field = Dictionary::new();
    field.set("Type", Object::Name(b"Annot".to_vec()));
    field.set("Subtype", Object::Name(b"Widget".to_vec()));
    field.set("FT", Object::Name(b"Tx".to_vec()));
    field.set(
        "T",
        Object::String(b"applicant_name".to_vec(), lopdf::StringFormat::Literal),
    );
    field.set(
        "V",
        Object::String(b"Jane Doe".to_vec(), lopdf::StringFormat::Literal),
    );
    field.set(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(20),
        ]),
    );
    field.set("AP", Object::Dictionary(appearance));
    let field_id = doc.add_object(Object::Dictionary(field));

    let mut acroform = Dictionary::new();
    acroform.set("Fields", Object::Array(vec![Object::Reference(field_id)]));
    let acroform_id = doc.add_object(Object::Dictionary(acroform));

    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Annots", Object::Array(vec![Object::Reference(field_id)]));
    page.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(200),
        ]),
    );
    let page_id = doc.add_object(Object::Dictionary(page));

    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    let pages_id = doc.add_object(Object::Dictionary(pages));
    if let Some(Object::Dictionary(p)) = doc.objects.get_mut(&page_id) {
        p.set("Parent", Object::Reference(pages_id));
    }

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    catalog.set("AcroForm", Object::Reference(acroform_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path)
        .expect("failed to save the AcroForm test PDF");
    appearance_content
}

// ════════════════════════════════════════════════════════════════
//  Test
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_preserves_acroform_fields_and_appearance() {
    let input = temp_pdf("acroform_in");
    let output = temp_pdf("acroform_out");
    let original_appearance = build_fixture_with_acroform(&input);

    let opts = CompressOpts::default();
    compress(&input, &output, &opts).expect("compress should succeed");

    let doc = Document::load(&output).expect("output should be a valid, loadable PDF");

    let root = match resolve(&doc, doc.trailer.get(b"Root").expect("Root must exist")) {
        Object::Dictionary(d) => d,
        _ => panic!("Root is not a dictionary"),
    };
    let acroform = match resolve(&doc, root.get(b"AcroForm").expect("AcroForm must survive")) {
        Object::Dictionary(d) => d,
        _ => panic!("AcroForm is not a dictionary"),
    };
    let Object::Array(fields) = acroform.get(b"Fields").expect("Fields array must survive") else {
        panic!("Fields is not an array");
    };
    let field = match resolve(&doc, &fields[0]) {
        Object::Dictionary(d) => d,
        _ => panic!("field is not a dictionary"),
    };

    assert!(matches!(field.get(b"FT"), Ok(Object::Name(n)) if n.as_slice() == b"Tx"));
    assert!(matches!(
        field.get(b"T"), Ok(Object::String(s, _)) if s.as_slice() == b"applicant_name"
    ));
    assert!(matches!(
        field.get(b"V"), Ok(Object::String(s, _)) if s.as_slice() == b"Jane Doe"
    ));

    let ap = match field.get(b"AP").expect("AP must survive") {
        Object::Dictionary(d) => d,
        _ => panic!("AP is not a dictionary"),
    };
    let appearance_stream = match resolve(&doc, ap.get(b"N").expect("AP/N must survive")) {
        Object::Stream(s) => s,
        _ => panic!("AP/N is not a stream"),
    };
    assert_eq!(
        decoded_stream_content(appearance_stream),
        original_appearance,
        "appearance stream content must survive byte-for-byte, whether or not it got deflated"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
