// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration tests — encrypted and structurally-malformed PDFs.
//! `compress()`/`analyze()` refuse an encrypted PDF outright unless
//! `allow_decrypt` is set, and `qpdf` is used as a best-effort repair
//! path for PDFs with non-compliant-but-common structural defects
//! (e.g. 19-byte instead of spec-mandated 20-byte xref entries). See
//! `common.rs` for shared helpers.

use lopdf::{Dictionary, Document, Object};
use std::path::Path;

use super::common::temp_pdf;
use crate::analyze::analyze;
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixtures
// ════════════════════════════════════════════════════════════════

/// Builds a PDF with a deliberately non-compliant cross-reference
/// table: each entry uses a single `\n` (19 bytes) instead of the
/// PDF-spec-mandated 2-byte EOL (20 bytes) — exactly the defect found
/// in a real-world PDF that `lopdf` rejected with "Invalid file
/// trailer" while `qpdf`/`pikepdf`/every normal viewer opened it fine.
/// Written by hand (not via `lopdf`'s own writer) so we control the
/// xref table's exact bytes.
fn build_fixture_with_malformed_xref(path: &Path) {
    let mut body = Vec::new();
    body.extend_from_slice(b"%PDF-1.4\n");

    let mut offsets = Vec::new();
    macro_rules! add_obj {
        ($n:expr, $content:expr) => {{
            offsets.push(body.len());
            body.extend_from_slice(format!("{} 0 obj\n", $n).as_bytes());
            body.extend_from_slice($content);
            body.extend_from_slice(b"\nendobj\n");
        }};
    }

    add_obj!(1, b"<< /Type /Catalog /Pages 2 0 R >>");
    add_obj!(2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    add_obj!(
        3,
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >>"
    );

    let xref_offset = body.len();
    body.extend_from_slice(b"xref\n0 4\n");
    body.extend_from_slice(b"0000000000 65535 f\n"); // 19 bytes — the defect
    for &off in &offsets {
        body.extend_from_slice(format!("{off:010} 00000 n\n").as_bytes()); // 19 bytes too
    }
    body.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    body.extend_from_slice(b"startxref\n");
    body.extend_from_slice(format!("{xref_offset}\n").as_bytes());
    body.extend_from_slice(b"%%EOF");

    std::fs::write(path, body).expect("failed to write malformed-xref fixture");
}

/// Like [`build_fixture`], but adds an `/Encrypt` entry to the
/// trailer — enough to make `qpdf` (and thus `loader.rs`'s logic)
/// recognize it as encrypted. This deliberately isn't a
/// *real* encryption setup (no actual RC4 keys, no genuine `/Standard`
/// security handler data) — `qpdf --decrypt` will correctly fail on
/// it once allowed to try, which is exactly the "needs a real
/// password" path we want to exercise. The happy path (a real,
/// empty-password-protected PDF that `qpdf --decrypt` successfully
/// opens) was validated by hand against a real-world file rather than
/// a synthetic fixture, since constructing genuinely-RC4-encrypted
/// bytes without an encryption library isn't practical to do
/// correctly here.
fn build_fixture_marked_encrypted(path: &Path) {
    let mut doc = Document::new();

    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));

    let mut encrypt_dict = Dictionary::new();
    encrypt_dict.set("Filter", Object::Name(b"Standard".to_vec()));
    let encrypt_id = doc.add_object(Object::Dictionary(encrypt_dict));

    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc.trailer.set("Encrypt", Object::Reference(encrypt_id));

    doc.save(path)
        .expect("failed to save the encrypted-marker test PDF");
}

// ════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_refuses_encrypted_pdf_without_explicit_allow_decrypt() {
    // This must refuse immediately — without even invoking qpdf —
    // since the opt-in gate itself is what's being tested here, not
    // qpdf's behaviour. No `qpdf`-installed guard needed.
    let input = temp_pdf("encrypted_no_optin_in");
    let output = temp_pdf("encrypted_no_optin_out");
    build_fixture_marked_encrypted(&input);

    let opts = CompressOpts::default(); // allow_decrypt: false
    let result = compress(&input, &output, &opts);

    assert!(
        result.is_err(),
        "compress() must refuse an encrypted PDF unless allow_decrypt is true"
    );
    assert!(
        !output.exists(),
        "no output file should be written when compress() refuses"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn analyze_refuses_encrypted_pdf_without_explicit_allow_decrypt() {
    let input = temp_pdf("encrypted_analyze_no_optin");
    build_fixture_marked_encrypted(&input);

    let result = analyze(&input, false);
    assert!(
        result.is_err(),
        "analyze() must refuse an encrypted PDF unless allow_decrypt is true"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn compress_with_allow_decrypt_still_fails_clearly_on_a_real_password() {
    if !crate::tools::is_available("qpdf") {
        eprintln!("skipping: qpdf not installed in this environment");
        return;
    }

    let input = temp_pdf("encrypted_optin_in");
    let output = temp_pdf("encrypted_optin_out");
    build_fixture_marked_encrypted(&input);

    // Explicitly opted in — now qpdf actually gets a chance to try,
    // and correctly fails because this fixture isn't genuinely
    // decryptable (no real encryption parameters).
    let opts = CompressOpts {
        allow_decrypt: true,
        ..Default::default()
    };
    let result = compress(&input, &output, &opts);

    assert!(
        result.is_err(),
        "compress() should still fail when qpdf itself can't decrypt the file"
    );
    assert!(!output.exists());

    let _ = std::fs::remove_file(&input);
}

#[test]
fn analyze_with_allow_decrypt_still_fails_clearly_on_a_real_password() {
    if !crate::tools::is_available("qpdf") {
        eprintln!("skipping: qpdf not installed in this environment");
        return;
    }

    let input = temp_pdf("encrypted_analyze_optin");
    build_fixture_marked_encrypted(&input);

    let result = analyze(&input, true);
    assert!(
        result.is_err(),
        "analyze() should still fail clearly rather than report on undecryptable ciphertext"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn analyze_recovers_from_nonstandard_xref_entry_length_via_qpdf() {
    if !crate::tools::is_available("qpdf") {
        eprintln!("skipping: qpdf not installed in this environment");
        return;
    }

    let input = temp_pdf("malformed_xref");
    build_fixture_with_malformed_xref(&input);

    let result = analyze(&input, false);
    assert!(
        result.is_ok(),
        "analyze() should recover via the qpdf repair fallback, got: {:?}",
        result.err()
    );

    let _ = std::fs::remove_file(&input);
}
