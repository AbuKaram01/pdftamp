// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration test — both string-based (`/JS (...)`, common for
//! short scripts) and stream-based (`/JS <<stream>>`, common for
//! longer scripts) JavaScript actions must survive `compress()`
//! byte-for-byte (after decoding, since opportunistic deflate of an
//! unfiltered stream is legitimate lossless compression, not
//! corruption). See `common.rs` for shared helpers.

use lopdf::{Dictionary, Document, Object, Stream};
use std::path::Path;

use super::common::{decoded_stream_content, resolve, temp_pdf};
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Builds a PDF with two JavaScript actions attached via the
/// document-level `/Names /JavaScript` tree: one with `/JS` as a
/// plain string (the common case) and one with `/JS` as a stream
/// (used for longer scripts) — the stream variant specifically
/// exercises the same "might get opportunistically deflated" path as
/// the AcroForm appearance stream above.
fn build_fixture_with_javascript(path: &Path) -> Vec<u8> {
    let mut doc = Document::new();

    let js_string_source = b"app.alert('from string');".to_vec();
    let mut js_string_action = Dictionary::new();
    js_string_action.set("S", Object::Name(b"JavaScript".to_vec()));
    js_string_action.set(
        "JS",
        Object::String(js_string_source.clone(), lopdf::StringFormat::Literal),
    );
    let js_string_id = doc.add_object(Object::Dictionary(js_string_action));

    let js_stream_source = b"app.alert('from a much longer script stored as a stream instead of a string, since real-world long scripts commonly are');".to_vec();
    let mut js_stream_dict = Dictionary::new();
    js_stream_dict.set("Length", Object::Integer(js_stream_source.len() as i64));
    let js_stream_obj_id = doc.add_object(Object::Stream(Stream::new(
        js_stream_dict,
        js_stream_source.clone(),
    )));

    let mut js_stream_action = Dictionary::new();
    js_stream_action.set("S", Object::Name(b"JavaScript".to_vec()));
    js_stream_action.set("JS", Object::Reference(js_stream_obj_id));
    let js_stream_action_id = doc.add_object(Object::Dictionary(js_stream_action));

    let mut js_names = Dictionary::new();
    js_names.set(
        "Names",
        Object::Array(vec![
            Object::String(b"ScriptOne".to_vec(), lopdf::StringFormat::Literal),
            Object::Reference(js_string_id),
            Object::String(b"ScriptTwo".to_vec(), lopdf::StringFormat::Literal),
            Object::Reference(js_stream_action_id),
        ]),
    );
    let js_names_id = doc.add_object(Object::Dictionary(js_names));

    let mut names = Dictionary::new();
    names.set("JavaScript", Object::Reference(js_names_id));
    let names_id = doc.add_object(Object::Dictionary(names));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Names", Object::Reference(names_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path)
        .expect("failed to save the JavaScript test PDF");
    js_stream_source
}

// ════════════════════════════════════════════════════════════════
//  Test
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_preserves_javascript_actions() {
    let input = temp_pdf("javascript_in");
    let output = temp_pdf("javascript_out");
    let original_stream_js = build_fixture_with_javascript(&input);

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
    let js_tree = match resolve(
        &doc,
        names
            .get(b"JavaScript")
            .expect("Names/JavaScript must survive"),
    ) {
        Object::Dictionary(d) => d,
        _ => panic!("JavaScript name tree is not a dictionary"),
    };
    let Object::Array(entries) = js_tree.get(b"Names").expect("JS Names array must survive") else {
        panic!("Names is not an array");
    };

    // entries = [name1, action1, name2, action2, ...]
    let action1 = match resolve(&doc, &entries[1]) {
        Object::Dictionary(d) => d,
        _ => panic!("first JS action is not a dictionary"),
    };
    assert!(
        matches!(
            action1.get(b"JS"), Ok(Object::String(s, _)) if s.as_slice() == b"app.alert('from string');"
        ),
        "string-based /JS must survive unchanged"
    );

    let action2 = match resolve(&doc, &entries[3]) {
        Object::Dictionary(d) => d,
        _ => panic!("second JS action is not a dictionary"),
    };
    let js_stream = match resolve(
        &doc,
        action2.get(b"JS").expect("stream-based /JS must survive"),
    ) {
        Object::Stream(s) => s,
        _ => panic!("stream-based /JS is not a stream"),
    };
    assert_eq!(
        decoded_stream_content(js_stream),
        original_stream_js,
        "stream-based /JS content must survive byte-for-byte, whether or not it got deflated"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
