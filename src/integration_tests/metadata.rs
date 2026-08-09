// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration tests — document-level metadata (`/Info` + XMP) is
//! kept by default and only removed when explicitly opted into via
//! `strip_metadata`. See `common.rs` for shared helpers.

use lopdf::{Dictionary, Document, Object, Stream};
use std::path::Path;

use super::common::temp_pdf;
use crate::analyze::analyze;
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Like [`build_fixture`], but also attaches an `/Info` dictionary and
/// an XMP `/Metadata` stream — used specifically to test metadata
/// stripping through `compress()`.
fn build_fixture_with_metadata(path: &Path) {
    let mut doc = Document::new();

    let mut info = Dictionary::new();
    info.set(
        "Author",
        Object::String(b"Test Author".to_vec(), lopdf::StringFormat::Literal),
    );
    info.set(
        "Producer",
        Object::String(b"pdftamp tests".to_vec(), lopdf::StringFormat::Literal),
    );
    let info_id = doc.add_object(Object::Dictionary(info));
    doc.trailer.set("Info", Object::Reference(info_id));

    let xmp_id = doc.add_object(Object::Stream(Stream::new(
        Dictionary::new(),
        b"<x:xmpmeta>fake xmp payload</x:xmpmeta>".to_vec(),
    )));

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Metadata", Object::Reference(xmp_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path)
        .expect("failed to save the metadata test PDF");
}

// ════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════

#[test]
fn analyze_surfaces_metadata_before_any_compression() {
    let input = temp_pdf("analyze_metadata_in");
    build_fixture_with_metadata(&input);

    let result = analyze(&input, false).expect("analyze should succeed");

    assert!(
        result.metadata.info_fields.contains(&"Author".to_string()),
        "expected analyze() to report the Author field before any compression happens"
    );
    assert!(
        result.metadata.xmp_bytes.is_some(),
        "expected analyze() to report the XMP stream size"
    );

    // analyze() must never modify the file — it's read-only.
    let untouched = Document::load(&input).expect("input should still be a valid PDF");
    assert!(
        untouched.trailer.get(b"Info").is_ok(),
        "analyze() must not strip anything"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn compress_keeps_metadata_by_default() {
    let input = temp_pdf("metadata_default_in");
    let output = temp_pdf("metadata_default_out");
    build_fixture_with_metadata(&input);

    // Plain defaults — strip_metadata is false unless explicitly set.
    let opts = CompressOpts::default();
    let report = compress(&input, &output, &opts).expect("compress should succeed");

    // Nothing should have been removed...
    assert!(report.metadata_fields_removed.is_empty());
    assert!(report.xmp_bytes_removed.is_none());

    // ...even though the report still honestly says metadata *exists*,
    // so a caller can decide to opt in after seeing this.
    assert!(
        report
            .metadata_found
            .info_fields
            .contains(&"Author".to_string()),
        "report should still surface that metadata exists, even though it wasn't touched"
    );
    assert!(report.metadata_found.xmp_bytes.is_some());

    let doc = Document::load(&output).expect("output should be a valid PDF");
    assert!(
        doc.trailer.get(b"Info").is_ok(),
        "Info dictionary should still be present by default"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn compress_strips_metadata_when_opted_in() {
    let input = temp_pdf("metadata_optin_in");
    let output = temp_pdf("metadata_optin_out");
    build_fixture_with_metadata(&input);

    let opts = CompressOpts {
        strip_metadata: true,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("compress should succeed");

    assert!(
        report
            .metadata_fields_removed
            .contains(&"Author".to_string()),
        "expected Author to be reported as removed once explicitly opted in"
    );
    assert!(
        report.xmp_bytes_removed.is_some(),
        "expected XMP stream to be reported as removed"
    );

    let doc = Document::load(&output).expect("output should be a valid PDF");
    assert!(
        doc.trailer.get(b"Info").is_err(),
        "Info dictionary should be gone once opted in"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
