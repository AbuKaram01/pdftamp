// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration test — the trailer `/ID` (a pair of byte strings used
//! by some viewers and tools to detect whether "this is the same
//! logical document") must survive `compress()` unchanged. See
//! `common.rs` for shared helpers.

use lopdf::{Dictionary, Document, Object};
use std::path::Path;

use super::common::temp_pdf;
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Like [`build_fixture`], but sets a known, fixed `/ID` array in the
/// trailer — the pair of byte strings PDF readers/workflow tools use
/// to identify a specific file version (e.g. for tracking whether two
/// copies are "the same document"). We deliberately use unusual,
/// easy-to-spot-if-mangled byte values rather than a realistic-looking
/// MD5-style ID, so a partial/truncated survival would also be caught.
fn build_fixture_with_id(path: &Path) -> (Vec<u8>, Vec<u8>) {
    let mut doc = Document::new();

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    let permanent_id: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x11, 0x22, 0x33];
    let changing_id: Vec<u8> = vec![0xCA, 0xFE, 0xBA, 0xBE, 0x44, 0x55, 0x66, 0x77];

    doc.trailer.set(
        "ID",
        Object::Array(vec![
            Object::String(permanent_id.clone(), lopdf::StringFormat::Hexadecimal),
            Object::String(changing_id.clone(), lopdf::StringFormat::Hexadecimal),
        ]),
    );

    doc.save(path).expect("failed to save the /ID test PDF");
    (permanent_id, changing_id)
}

// ════════════════════════════════════════════════════════════════
//  Test
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_preserves_trailer_id() {
    let input = temp_pdf("id_in");
    let output = temp_pdf("id_out");
    let (permanent_id, changing_id) = build_fixture_with_id(&input);

    let opts = CompressOpts::default();
    compress(&input, &output, &opts).expect("compress should succeed");

    let doc = Document::load(&output).expect("output should be a valid, loadable PDF");

    let Ok(Object::Array(id_arr)) = doc.trailer.get(b"ID") else {
        panic!(
            "trailer /ID must survive compression — got: {:?}",
            doc.trailer.get(b"ID")
        );
    };
    assert_eq!(
        id_arr.len(),
        2,
        "trailer /ID must still have exactly two entries"
    );

    let as_bytes = |o: &Object| match o {
        Object::String(b, _) => b.clone(),
        other => panic!("expected an /ID entry to be a string, got {other:?}"),
    };

    assert_eq!(
        as_bytes(&id_arr[0]),
        permanent_id,
        "the permanent file identifier must survive unchanged"
    );
    assert_eq!(
        as_bytes(&id_arr[1]),
        changing_id,
        "the (first) changing identifier must survive unchanged"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
