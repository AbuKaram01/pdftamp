// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration tests — baseline `compress`/`analyze` behavior on a
//! simple fixture (one FlateDecode image, one raw content stream).
//! See `common.rs` for the shared fixture-building helpers.

use flate2::{write::ZlibEncoder, Compression};
use lopdf::{Dictionary, Document, Object, Stream};
use std::io::Write;
use std::path::Path;

use super::common::{noise_bytes, temp_pdf};
use crate::analyze::analyze;
use crate::compress::{compress, CompressOpts};
use crate::paths::OnConflict;

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Builds a minimal valid PDF: one page, one FlateDecode image, and
/// one unfiltered content stream.
fn build_fixture(path: &Path) {
    let mut doc = Document::new();

    // ── 48x48 RGB image ─────────────────────────────────────────
    let (w, h) = (48u32, 48u32);
    let raw = noise_bytes((w * h * 3) as usize, 12345);

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).expect("zlib write");
    let compressed = encoder.finish().expect("zlib finish");

    let mut img_dict = Dictionary::new();
    img_dict.set("Type", Object::Name(b"XObject".to_vec()));
    img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
    img_dict.set("Width", Object::Integer(w as i64));
    img_dict.set("Height", Object::Integer(h as i64));
    img_dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    img_dict.set("BitsPerComponent", Object::Integer(8));
    img_dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    img_dict.set("Length", Object::Integer(compressed.len() as i64));

    let img_id = doc.add_object(Object::Stream(Stream::new(img_dict, compressed)));

    // ── raw content stream (no Filter) ──────────────────────────
    let content = b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec();
    let mut content_dict = Dictionary::new();
    content_dict.set("Length", Object::Integer(content.len() as i64));
    let content_id = doc.add_object(Object::Stream(Stream::new(content_dict, content)));

    // ── Resources ────────────────────────────────────────────────
    let mut xobjects = Dictionary::new();
    xobjects.set("Im0", Object::Reference(img_id));
    let mut resources = Dictionary::new();
    resources.set("XObject", Object::Dictionary(xobjects));
    let resources_id = doc.add_object(Object::Dictionary(resources));

    // ── Page ─────────────────────────────────────────────────────
    let mut page = Dictionary::new();
    page.set("Type", Object::Name(b"Page".to_vec()));
    page.set("Contents", Object::Reference(content_id));
    page.set("Resources", Object::Reference(resources_id));
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

    // ── Pages ────────────────────────────────────────────────────
    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set("Kids", Object::Array(vec![Object::Reference(page_id)]));
    pages.set("Count", Object::Integer(1));
    let pages_id = doc.add_object(Object::Dictionary(pages));

    // lopdf doesn't set Parent automatically — link it ourselves.
    if let Some(Object::Dictionary(page_dict)) = doc.objects.get_mut(&page_id) {
        page_dict.set("Parent", Object::Reference(pages_id));
    }

    // ── Catalog + trailer ────────────────────────────────────────
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));

    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path).expect("failed to save the test PDF");
}

/// Checks that `path` is still a loadable PDF and still contains an
/// Image XObject.
fn output_has_image(path: &Path) -> bool {
    let Ok(doc) = Document::load(path) else {
        return false;
    };
    doc.objects.values().any(|obj| {
        matches!(obj, Object::Stream(s) if matches!(
            s.dict.get(b"Subtype"),
            Ok(Object::Name(n)) if n.as_slice() == b"Image"
        ))
    })
}

// ════════════════════════════════════════════════════════════════
//  Tests
// ════════════════════════════════════════════════════════════════

#[test]
fn analyze_detects_image_filter_and_raw_stream() {
    let input = temp_pdf("analyze_in");
    build_fixture(&input);

    let result = analyze(&input, false).expect("analyze should succeed");

    let flate_images = result
        .images
        .get("FlateDecode")
        .expect("expected a FlateDecode image to be detected");
    assert_eq!(flate_images.count, 1);

    assert_eq!(result.raw_streams.count, 1);

    let _ = std::fs::remove_file(&input);
}

#[test]
fn compress_never_bloats_output_and_preserves_structure() {
    // Note: this test validates the *guarantee* (output never exceeds
    // input), not which internal path produced it. Deliberately
    // forcing the `kept_original` fallback branch would require
    // controlling lopdf's exact re-serialization behavior on a real,
    // structurally-complex document — not reliably reproducible with
    // a tiny synthetic fixture, so we don't try to force it here.
    let input = temp_pdf("compress_in");
    let output = temp_pdf("compress_out");
    build_fixture(&input);

    let opts = CompressOpts {
        quality: 75,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("compress should succeed");

    // The whole-file safety net guarantees output is never larger
    // than input — no tolerance margin needed.
    assert!(
        report.output_bytes <= report.input_bytes,
        "output ({} bytes) must never exceed input ({} bytes)",
        report.output_bytes,
        report.input_bytes
    );

    // The raw content stream should always compress — Deflate on
    // simple text is a guaranteed, easy win.
    assert!(
        report.streams_compressed >= 1,
        "expected the raw stream to be deflated"
    );

    // Most important: the image must still be present after compression.
    assert!(
        output_has_image(&output),
        "image should still be present after compression"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn compress_with_emit_events_records_at_least_one_event() {
    let input = temp_pdf("events_in");
    let output = temp_pdf("events_out");
    build_fixture(&input);

    let opts = CompressOpts {
        quality: 75,
        emit_events: true,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("compress should succeed");

    // The raw stream is guaranteed to get deflated, so we should get
    // at least one event back.
    assert!(
        !report.events.is_empty(),
        "emit_events=true should record events"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

// ════════════════════════════════════════════════════════════════
//  --dry-run
// ════════════════════════════════════════════════════════════════

#[test]
fn dry_run_never_creates_the_output_file() {
    let input = temp_pdf("dryrun_in");
    let output = temp_pdf("dryrun_out");
    build_fixture(&input);

    let opts = CompressOpts {
        quality: 75,
        dry_run: true,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("dry run should succeed");

    assert!(report.dry_run);
    assert_eq!(
        report.final_output, output,
        "dry run should still report the path it *would* have used"
    );
    assert!(
        !output.exists(),
        "a dry run must never actually create the output file"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn dry_run_never_creates_the_outputs_parent_directory() {
    let input = temp_pdf("dryrun_parent_in");
    // A subdirectory that doesn't exist yet — a real run would create
    // it via `create_dir_all`; a dry run must not.
    let output = std::env::temp_dir()
        .join(format!("pdftamp_dryrun_nonexistent_{}", std::process::id()))
        .join("out.pdf");
    build_fixture(&input);

    let opts = CompressOpts {
        dry_run: true,
        ..Default::default()
    };
    compress(&input, &output, &opts).expect("dry run should succeed");

    assert!(
        !output.parent().unwrap().exists(),
        "a dry run must never create the output's parent directory either"
    );

    let _ = std::fs::remove_file(&input);
}

#[test]
fn dry_run_reports_the_same_numbers_a_real_run_would() {
    // Two independent copies of the same fixture, one compressed for
    // real and one only previewed — the reported stats should match
    // exactly, since a dry run runs the genuine pipeline in memory
    // and only skips the final write.
    let input_a = temp_pdf("dryrun_parity_a");
    let input_b = temp_pdf("dryrun_parity_b");
    let output_a = temp_pdf("dryrun_parity_a_out");
    let output_b = temp_pdf("dryrun_parity_b_out");
    build_fixture(&input_a);
    build_fixture(&input_b);

    let real_opts = CompressOpts {
        quality: 75,
        ..Default::default()
    };
    let dry_opts = CompressOpts {
        quality: 75,
        dry_run: true,
        ..Default::default()
    };

    let real = compress(&input_a, &output_a, &real_opts).expect("real run should succeed");
    let dry = compress(&input_b, &output_b, &dry_opts).expect("dry run should succeed");

    assert_eq!(real.input_bytes, dry.input_bytes);
    assert_eq!(real.output_bytes, dry.output_bytes);
    assert_eq!(real.jpeg_compressed, dry.jpeg_compressed);
    assert_eq!(real.flate_converted, dry.flate_converted);
    assert_eq!(real.lzw_converted, dry.lzw_converted);
    assert_eq!(real.streams_compressed, dry.streams_compressed);
    assert_eq!(real.kept_original, dry.kept_original);
    assert!(!real.dry_run);
    assert!(dry.dry_run);
    assert!(
        output_a.is_file(),
        "the real run should have written its output"
    );
    assert!(!output_b.exists(), "the dry run should not have");

    let _ = std::fs::remove_file(&input_a);
    let _ = std::fs::remove_file(&input_b);
    let _ = std::fs::remove_file(&output_a);
}

#[test]
fn dry_run_under_refuse_reports_the_same_conflict_error_a_real_run_would() {
    let input = temp_pdf("dryrun_refuse_in");
    let output = temp_pdf("dryrun_refuse_out");
    build_fixture(&input);
    std::fs::write(&output, b"PRE-EXISTING PLACEHOLDER").unwrap();

    let opts = CompressOpts {
        dry_run: true,
        on_conflict: OnConflict::Refuse,
        ..Default::default()
    };
    let err = compress(&input, &output, &opts).expect_err("should refuse, same as a real run");
    assert!(err.to_string().contains("already exists"));
    // Never touched, exactly like a real Refuse run would leave it.
    assert_eq!(std::fs::read(&output).unwrap(), b"PRE-EXISTING PLACEHOLDER");

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn dry_run_under_overwrite_reports_success_without_touching_the_existing_file() {
    let input = temp_pdf("dryrun_overwrite_in");
    let output = temp_pdf("dryrun_overwrite_out");
    build_fixture(&input);
    std::fs::write(&output, b"PRE-EXISTING PLACEHOLDER").unwrap();

    let opts = CompressOpts {
        dry_run: true,
        on_conflict: OnConflict::Overwrite,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("dry run should succeed");

    assert_eq!(report.final_output, output);
    assert!(!report.renamed_to_avoid_conflict);
    // A dry run never actually overwrites, even under Overwrite.
    assert_eq!(std::fs::read(&output).unwrap(), b"PRE-EXISTING PLACEHOLDER");

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn dry_run_under_rename_picks_the_same_numbered_name_without_creating_it() {
    let input = temp_pdf("dryrun_rename_in");
    let output = temp_pdf("dryrun_rename_out");
    build_fixture(&input);
    std::fs::write(&output, b"placeholder 0").unwrap();
    let numbered_1 = numbered_sibling(&output, 1);
    std::fs::write(&numbered_1, b"placeholder 1").unwrap();
    let numbered_2 = numbered_sibling(&output, 2);

    let opts = CompressOpts {
        dry_run: true,
        on_conflict: OnConflict::Rename,
        ..Default::default()
    };
    let report = compress(&input, &output, &opts).expect("dry run should succeed");

    assert_eq!(report.final_output, numbered_2);
    assert!(report.renamed_to_avoid_conflict);
    assert!(
        !numbered_2.exists(),
        "the winning candidate name is reported, but a dry run never creates it"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _ = std::fs::remove_file(&numbered_1);
}

/// `report-compressed.pdf` + `2` → `report-compressed (2).pdf` — a
/// tiny local re-implementation of `paths::numbered_candidate` (which
/// is private to that module) just for building this test's fixture
/// names.
fn numbered_sibling(path: &Path, n: u32) -> std::path::PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap();
    path.with_file_name(format!("{stem} ({n}).{ext}"))
}
