// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration test — a `/Names /EmbeddedFiles` file attachment must
//! survive `compress()` byte-for-byte. Uses deterministic binary
//! noise (not text) as the payload specifically to make sure binary
//! safety isn't accidentally depending on the payload happening to
//! look like valid UTF-8 or PDF syntax. See `common.rs` for
//! shared helpers.

use lopdf::{Dictionary, Document, Object, Stream};
use std::path::Path;

use super::common::{decoded_stream_content, noise_bytes, resolve, temp_pdf};
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Builds a PDF with one file attachment via `/Names /EmbeddedFiles`.
/// The attachment's content is deterministic binary noise (not text)
/// specifically to make sure binary safety isn't accidentally
/// depending on the payload happening to look like valid UTF-8 or
/// PDF syntax.
fn build_fixture_with_embedded_file(path: &Path) -> Vec<u8> {
    let mut doc = Document::new();

    let attachment_bytes = noise_bytes(512, 777);

    let mut ef_stream_dict = Dictionary::new();
    ef_stream_dict.set("Type", Object::Name(b"EmbeddedFile".to_vec()));
    ef_stream_dict.set("Length", Object::Integer(attachment_bytes.len() as i64));
    let mut params = Dictionary::new();
    params.set("Size", Object::Integer(attachment_bytes.len() as i64));
    ef_stream_dict.set("Params", Object::Dictionary(params));
    let ef_stream_id = doc.add_object(Object::Stream(Stream::new(
        ef_stream_dict,
        attachment_bytes.clone(),
    )));

    let mut ef_wrapper = Dictionary::new();
    ef_wrapper.set("F", Object::Reference(ef_stream_id));

    let mut filespec = Dictionary::new();
    filespec.set("Type", Object::Name(b"Filespec".to_vec()));
    filespec.set(
        "F",
        Object::String(b"attachment.bin".to_vec(), lopdf::StringFormat::Literal),
    );
    filespec.set("EF", Object::Dictionary(ef_wrapper));
    let filespec_id = doc.add_object(Object::Dictionary(filespec));

    let mut ef_names = Dictionary::new();
    ef_names.set(
        "Names",
        Object::Array(vec![
            Object::String(b"attachment.bin".to_vec(), lopdf::StringFormat::Literal),
            Object::Reference(filespec_id),
        ]),
    );
    let ef_names_id = doc.add_object(Object::Dictionary(ef_names));

    let mut names = Dictionary::new();
    names.set("EmbeddedFiles", Object::Reference(ef_names_id));
    let names_id = doc.add_object(Object::Dictionary(names));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Names", Object::Reference(names_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path)
        .expect("failed to save the embedded-file test PDF");
    attachment_bytes
}

// ════════════════════════════════════════════════════════════════
//  Test
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_preserves_embedded_file_bytes() {
    let input = temp_pdf("embedded_in");
    let output = temp_pdf("embedded_out");
    let original_bytes = build_fixture_with_embedded_file(&input);

    let opts = CompressOpts::default();
    compress(&input, &output, &opts).expect("compress should succeed");

    let doc = Document::load(&output).expect("output should be a valid, loadable PDF");

    let root = match resolve(&doc, doc.trailer.get(b"Root").expect("Root must exist")) {
        Object::Dictionary(d) => d,
        _ => panic!("Root is not a dictionary"),
    };
    let names = match resolve(&doc, root.get(b"Names").expect("Names must survive")) {
        Object::Dictionary(d) => d,
        _ => panic!("Names is not a dictionary"),
    };
    let ef_tree = match resolve(
        &doc,
        names
            .get(b"EmbeddedFiles")
            .expect("Names/EmbeddedFiles must survive"),
    ) {
        Object::Dictionary(d) => d,
        _ => panic!("EmbeddedFiles name tree is not a dictionary"),
    };
    let Object::Array(entries) = ef_tree.get(b"Names").expect("EF Names array must survive") else {
        panic!("Names is not an array");
    };

    let filespec = match resolve(&doc, &entries[1]) {
        Object::Dictionary(d) => d,
        _ => panic!("filespec is not a dictionary"),
    };
    assert!(
        matches!(
            filespec.get(b"F"), Ok(Object::String(s, _)) if s.as_slice() == b"attachment.bin"
        ),
        "filename must survive unchanged"
    );

    let ef = match filespec.get(b"EF").expect("EF dict must survive") {
        Object::Dictionary(d) => d,
        _ => panic!("EF is not a dictionary"),
    };
    let ef_stream = match resolve(&doc, ef.get(b"F").expect("EF/F must survive")) {
        Object::Stream(s) => s,
        _ => panic!("EF/F is not a stream"),
    };
    assert_eq!(
        decoded_stream_content(ef_stream),
        original_bytes,
        "embedded file content must survive byte-for-byte, whether or not it got deflated"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
