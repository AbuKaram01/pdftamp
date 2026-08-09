// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration tests for `compress_directory`: the default
//! next-to-original destination, the explicit mirrored-directory
//! opt-in, and every `OnConflict` policy end to end (not just the
//! unit-level `paths::commit` tests — these go through the real
//! `WalkDir` + `compress()` path).
//!
//! Self-contained rather than sharing `common.rs`'s fixture helpers:
//! these tests only need a minimal, structurally-valid PDF (no
//! images or content streams to compress), and care about exact
//! byte-for-byte survival of pre-existing files, which is easier to
//! reason about starting from a fixture built locally here than from
//! `common.rs`'s noise-filled ones (meant for exercising the
//! compression pipeline itself, not path/conflict handling).

use lopdf::{Dictionary, Document, Object};
use std::path::Path;

use crate::batch::{compress_directory, DestStrategy};
use crate::compress::CompressOpts;
use crate::paths::OnConflict;

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Smallest possible valid PDF — no images or content streams, since
/// these tests are about *where files end up*, not what compression
/// does to their contents (that's covered elsewhere).
fn build_fixture(path: &Path) {
    let mut doc = Document::new();
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc.save(path).expect("failed to save fixture PDF");
}

/// Fresh, empty temp directory for one test, named after `name` plus
/// this process's ID so parallel `cargo test` runs never collide.
fn test_dir(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("pdftamp_batch_test_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ════════════════════════════════════════════════════════════════
//  Default destination: next to the original
// ════════════════════════════════════════════════════════════════

#[test]
fn next_to_original_places_output_beside_each_source_file() {
    let dir = test_dir("next_to_original");
    let sub = dir.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    build_fixture(&dir.join("report.pdf"));
    build_fixture(&sub.join("intro.pdf"));

    let opts = CompressOpts::default();
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.succeeded_count(), 2);
    assert!(dir.join("report-compressed.pdf").is_file());
    assert!(sub.join("intro-compressed.pdf").is_file());
    // Nothing should have been written outside each file's own folder.
    assert!(!dir.join("intro-compressed.pdf").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn next_to_original_does_not_reprocess_the_file_it_just_wrote() {
    // The scenario the upfront file-list collection exists to
    // prevent: with only one real source file, the batch must still
    // report exactly one item — not pick up its own
    // "*-compressed.pdf" output mid-walk and process that too.
    let dir = test_dir("no_self_reprocess");
    build_fixture(&dir.join("a.pdf"));

    let opts = CompressOpts::default();
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(
        batch.items.len(),
        1,
        "expected exactly one processed item (the original), got: {:?}",
        batch.items.iter().map(|i| &i.input).collect::<Vec<_>>()
    );
    assert!(dir.join("a-compressed.pdf").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

// ════════════════════════════════════════════════════════════════
//  Explicit mirrored output directory (opt-in)
// ════════════════════════════════════════════════════════════════

#[test]
fn mirror_recreates_the_relative_structure_under_the_output_dir() {
    let dir = test_dir("mirror");
    let input_dir = dir.join("books");
    let output_dir = dir.join("books-compressed");
    std::fs::create_dir_all(input_dir.join("rust")).unwrap();
    build_fixture(&input_dir.join("rust").join("intro.pdf"));

    let opts = CompressOpts::default();
    let batch =
        compress_directory(&input_dir, DestStrategy::Mirror(&output_dir), &opts, |_| {}).unwrap();

    assert_eq!(batch.succeeded_count(), 1);
    assert!(output_dir.join("rust").join("intro.pdf").is_file());
    // The mirrored copy keeps the original name — no "-compressed"
    // suffix — since the whole tree already lives under a separately
    // named output directory.
    assert!(!output_dir
        .join("rust")
        .join("intro-compressed.pdf")
        .is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Finds the `BatchItem` whose input file has this exact file name.
/// Mainly for readability at call sites — most of these tests only
/// ever produce one item, but naming it beats indexing `items[0]`
/// and hoping the walk order matches.
fn item_for<'a>(
    batch: &'a crate::batch::BatchReport,
    file_name: &str,
) -> &'a crate::batch::BatchItem {
    batch
        .items
        .iter()
        .find(|i| i.input.file_name().and_then(|n| n.to_str()) == Some(file_name))
        .unwrap_or_else(|| panic!("no batch item for '{file_name}' — got: {batch:?}"))
}

// ════════════════════════════════════════════════════════════════
//  OnConflict policies, end to end
// ════════════════════════════════════════════════════════════════

// These three tests pre-create "report-compressed.pdf" as a plain
// placeholder file (not a real PDF — doesn't need to be, since
// `looks_like_own_output` keeps the walk from ever treating it as a
// second input in its own right; see the dedicated test for that
// below) with content that's trivially distinguishable from whatever
// "report.pdf" compresses down to, so a byte comparison actually
// proves something.

const PLACEHOLDER: &[u8] = b"PRE-EXISTING PLACEHOLDER CONTENT, NOT A PDF";

#[test]
fn refuse_leaves_the_existing_file_untouched_and_reports_that_item_as_failed() {
    let dir = test_dir("conflict_refuse");
    build_fixture(&dir.join("report.pdf"));
    std::fs::write(dir.join("report-compressed.pdf"), PLACEHOLDER).unwrap();

    let opts = CompressOpts {
        on_conflict: OnConflict::Refuse,
        ..Default::default()
    };
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.items.len(), 1);
    assert!(
        item_for(&batch, "report.pdf").result.is_err(),
        "report.pdf's target already existed — it should have been refused, not overwritten"
    );
    assert_eq!(
        std::fs::read(dir.join("report-compressed.pdf")).unwrap(),
        PLACEHOLDER,
        "the pre-existing file must survive completely untouched"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overwrite_replaces_the_existing_file() {
    let dir = test_dir("conflict_overwrite");
    build_fixture(&dir.join("report.pdf"));
    std::fs::write(dir.join("report-compressed.pdf"), PLACEHOLDER).unwrap();

    let opts = CompressOpts {
        on_conflict: OnConflict::Overwrite,
        ..Default::default()
    };
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.items.len(), 1);
    assert!(item_for(&batch, "report.pdf").result.is_ok());
    let written = std::fs::read(dir.join("report-compressed.pdf")).unwrap();
    assert_ne!(
        written, PLACEHOLDER,
        "the placeholder content should have been replaced"
    );
    assert!(Document::load(dir.join("report-compressed.pdf")).is_ok());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rename_keeps_the_existing_file_and_saves_the_new_one_alongside() {
    let dir = test_dir("conflict_rename");
    build_fixture(&dir.join("report.pdf"));
    std::fs::write(dir.join("report-compressed.pdf"), PLACEHOLDER).unwrap();

    let opts = CompressOpts {
        on_conflict: OnConflict::Rename,
        ..Default::default()
    };
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.items.len(), 1);
    let report_item = item_for(&batch, "report.pdf");
    assert!(report_item.result.is_ok());
    assert_eq!(report_item.output, dir.join("report-compressed (1).pdf"));
    assert_eq!(
        std::fs::read(dir.join("report-compressed.pdf")).unwrap(),
        PLACEHOLDER,
        "the pre-existing file must survive untouched"
    );
    assert!(
        dir.join("report-compressed (1).pdf").is_file(),
        "report.pdf's compressed copy should have been saved under a numbered name instead"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ════════════════════════════════════════════════════════════════
//  Skipping a previous run's own output
// ════════════════════════════════════════════════════════════════

#[test]
fn next_to_original_skips_files_that_look_like_previous_output() {
    let dir = test_dir("skip_own_output");
    build_fixture(&dir.join("report.pdf"));
    // Simulates leftovers from an earlier run: a plain "-compressed"
    // file, plus a numbered one from a past --if-exists=rename.
    build_fixture(&dir.join("old-compressed.pdf"));
    build_fixture(&dir.join("old-compressed (1).pdf"));

    let opts = CompressOpts::default();
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(
        batch.items.len(),
        1,
        "only report.pdf should have been treated as real input, got: {:?}",
        batch.items.iter().map(|i| &i.input).collect::<Vec<_>>()
    );
    assert_eq!(item_for(&batch, "report.pdf").input, dir.join("report.pdf"));
    assert_eq!(batch.skipped_own_output.len(), 2);
    assert!(batch
        .skipped_own_output
        .contains(&dir.join("old-compressed.pdf")));
    assert!(batch
        .skipped_own_output
        .contains(&dir.join("old-compressed (1).pdf")));
    // Skipped means untouched, not deleted.
    assert!(dir.join("old-compressed.pdf").is_file());
    assert!(dir.join("old-compressed (1).pdf").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mirror_does_not_skip_files_that_look_like_compressed_output() {
    // The skip only makes sense for NextToOriginal, where pdftamp's
    // own naming convention is what creates the risk. Under an
    // explicit Mirror output directory, a file already named
    // "*-compressed.pdf" is just an ordinary input like any other.
    let dir = test_dir("mirror_no_skip");
    let input_dir = dir.join("in");
    let output_dir = dir.join("out");
    std::fs::create_dir_all(&input_dir).unwrap();
    build_fixture(&input_dir.join("already-compressed.pdf"));

    let opts = CompressOpts::default();
    let batch =
        compress_directory(&input_dir, DestStrategy::Mirror(&output_dir), &opts, |_| {}).unwrap();

    assert_eq!(batch.items.len(), 1);
    assert!(batch.skipped_own_output.is_empty());
    assert!(output_dir.join("already-compressed.pdf").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

// ════════════════════════════════════════════════════════════════
//  --dry-run
// ════════════════════════════════════════════════════════════════

#[test]
fn dry_run_next_to_original_creates_nothing_but_reports_success() {
    let dir = test_dir("dryrun_next_to_original");
    let sub = dir.join("nested");
    std::fs::create_dir_all(&sub).unwrap();
    build_fixture(&dir.join("report.pdf"));
    build_fixture(&sub.join("intro.pdf"));

    let opts = CompressOpts {
        dry_run: true,
        ..Default::default()
    };
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.succeeded_count(), 2);
    assert!(batch.items.iter().all(|i| match &i.result {
        Ok(r) => r.dry_run,
        Err(_) => false,
    }));
    // Same paths a real run would have used are still reported...
    assert!(batch
        .items
        .iter()
        .any(|i| i.output == dir.join("report-compressed.pdf")));
    assert!(batch
        .items
        .iter()
        .any(|i| i.output == sub.join("intro-compressed.pdf")));
    // ...but neither was actually created.
    assert!(!dir.join("report-compressed.pdf").exists());
    assert!(!sub.join("intro-compressed.pdf").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dry_run_mirror_creates_neither_the_output_tree_nor_any_file_in_it() {
    let dir = test_dir("dryrun_mirror");
    let input_dir = dir.join("books");
    let output_dir = dir.join("books-compressed");
    std::fs::create_dir_all(input_dir.join("rust")).unwrap();
    build_fixture(&input_dir.join("rust").join("intro.pdf"));

    let opts = CompressOpts {
        dry_run: true,
        ..Default::default()
    };
    let batch =
        compress_directory(&input_dir, DestStrategy::Mirror(&output_dir), &opts, |_| {}).unwrap();

    assert_eq!(batch.succeeded_count(), 1);
    assert!(
        !output_dir.exists(),
        "a dry run must not even create the top-level mirrored output directory"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dry_run_under_refuse_still_reports_the_conflict_and_leaves_everything_untouched() {
    // A dry run is supposed to tell you the truth about what a real
    // run would do — including a Refuse conflict that would make it
    // fail on that particular file.
    let dir = test_dir("dryrun_conflict_refuse");
    build_fixture(&dir.join("report.pdf"));
    std::fs::write(dir.join("report-compressed.pdf"), PLACEHOLDER).unwrap();

    let opts = CompressOpts {
        dry_run: true,
        on_conflict: OnConflict::Refuse,
        ..Default::default()
    };
    let batch = compress_directory(&dir, DestStrategy::NextToOriginal, &opts, |_| {}).unwrap();

    assert_eq!(batch.items.len(), 1);
    assert!(
        item_for(&batch, "report.pdf").result.is_err(),
        "a dry run under Refuse should report the same failure a real run would"
    );
    assert_eq!(
        std::fs::read(dir.join("report-compressed.pdf")).unwrap(),
        PLACEHOLDER,
        "and, dry run or not, never touch the file already there"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
