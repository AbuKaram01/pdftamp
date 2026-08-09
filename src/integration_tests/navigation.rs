// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration tests — bookmarks (`/Outlines`) and link annotations
//! must keep pointing at the right page after compression, even
//! though page object IDs can shift around during the rewrite. See
//! `common.rs` for shared helpers.

use flate2::{write::ZlibEncoder, Compression};
use lopdf::{Dictionary, Document, Object, Stream};
use std::io::Write;
use std::path::Path;

use super::common::{noise_bytes, resolve, temp_pdf};
use crate::compress::{compress, CompressOpts};

// ════════════════════════════════════════════════════════════════
//  Fixture
// ════════════════════════════════════════════════════════════════

/// Builds a 3-page PDF with a navigation structure that exercises
/// exactly the things `compress()` doesn't (and shouldn't) touch:
///   - an outline (bookmark) whose `/Dest` points at page 2
///   - a clickable link annotation on page 1 whose `/A /GoTo` action
///     points at page 3
///   - page 2 also carries a real image, so the file actually goes
///     through the compression/rewrite path, not just a no-op save
///
/// Pages are distinguished by a unique `MediaBox` size each (100x100,
/// 200x200, 300x300) rather than by object number — object numbers
/// are free to change across a `lopdf` rewrite, but `MediaBox` is
/// content `compress()` never touches, so it's a reliable "this is
/// definitely page N" marker to check against after the round-trip.
fn build_fixture_with_navigation(path: &Path) {
    let mut doc = Document::new();

    // ── Page 1 (100x100) — will carry the link annotation ───────
    let mut page1 = Dictionary::new();
    page1.set("Type", Object::Name(b"Page".to_vec()));
    page1.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(100),
            Object::Integer(100),
        ]),
    );
    let page1_id = doc.add_object(Object::Dictionary(page1));

    // ── Page 2 (200x200) — target of the bookmark; has an image ──
    let raw = noise_bytes(8 * 8 * 3, 999);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&raw).expect("zlib write");
    let compressed = encoder.finish().expect("zlib finish");

    let mut img_dict = Dictionary::new();
    img_dict.set("Type", Object::Name(b"XObject".to_vec()));
    img_dict.set("Subtype", Object::Name(b"Image".to_vec()));
    img_dict.set("Width", Object::Integer(8));
    img_dict.set("Height", Object::Integer(8));
    img_dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
    img_dict.set("BitsPerComponent", Object::Integer(8));
    img_dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
    img_dict.set("Length", Object::Integer(compressed.len() as i64));
    let img_id = doc.add_object(Object::Stream(Stream::new(img_dict, compressed)));

    let mut xobjects = Dictionary::new();
    xobjects.set("Im0", Object::Reference(img_id));
    let mut resources = Dictionary::new();
    resources.set("XObject", Object::Dictionary(xobjects));
    let resources_id = doc.add_object(Object::Dictionary(resources));

    let content = b"q 8 0 0 8 0 0 cm /Im0 Do Q".to_vec();
    let mut content_dict = Dictionary::new();
    content_dict.set("Length", Object::Integer(content.len() as i64));
    let content_id = doc.add_object(Object::Stream(Stream::new(content_dict, content)));

    let mut page2 = Dictionary::new();
    page2.set("Type", Object::Name(b"Page".to_vec()));
    page2.set("Contents", Object::Reference(content_id));
    page2.set("Resources", Object::Reference(resources_id));
    page2.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(200),
            Object::Integer(200),
        ]),
    );
    let page2_id = doc.add_object(Object::Dictionary(page2));

    // ── Page 3 (300x300) — target of the link annotation ─────────
    let mut page3 = Dictionary::new();
    page3.set("Type", Object::Name(b"Page".to_vec()));
    page3.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(300),
            Object::Integer(300),
        ]),
    );
    let page3_id = doc.add_object(Object::Dictionary(page3));

    // ── Link annotation on page 1, pointing at page 3 ─────────────
    let mut goto_action = Dictionary::new();
    goto_action.set("Type", Object::Name(b"Action".to_vec()));
    goto_action.set("S", Object::Name(b"GoTo".to_vec()));
    goto_action.set(
        "D",
        Object::Array(vec![
            Object::Reference(page3_id),
            Object::Name(b"Fit".to_vec()),
        ]),
    );

    let mut link = Dictionary::new();
    link.set("Type", Object::Name(b"Annot".to_vec()));
    link.set("Subtype", Object::Name(b"Link".to_vec()));
    link.set(
        "Rect",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(10),
            Object::Integer(10),
        ]),
    );
    link.set("A", Object::Dictionary(goto_action));
    let link_id = doc.add_object(Object::Dictionary(link));

    if let Some(Object::Dictionary(p1)) = doc.objects.get_mut(&page1_id) {
        p1.set("Annots", Object::Array(vec![Object::Reference(link_id)]));
    }

    // ── Pages tree ─────────────────────────────────────────────────
    let mut pages = Dictionary::new();
    pages.set("Type", Object::Name(b"Pages".to_vec()));
    pages.set(
        "Kids",
        Object::Array(vec![
            Object::Reference(page1_id),
            Object::Reference(page2_id),
            Object::Reference(page3_id),
        ]),
    );
    pages.set("Count", Object::Integer(3));
    let pages_id = doc.add_object(Object::Dictionary(pages));

    for pid in [page1_id, page2_id, page3_id] {
        if let Some(Object::Dictionary(p)) = doc.objects.get_mut(&pid) {
            p.set("Parent", Object::Reference(pages_id));
        }
    }

    // ── Outline (bookmark) pointing at page 2 ────────────────────
    let mut outline_item = Dictionary::new();
    outline_item.set(
        "Title",
        Object::String(b"Chapter 2".to_vec(), lopdf::StringFormat::Literal),
    );
    outline_item.set(
        "Dest",
        Object::Array(vec![
            Object::Reference(page2_id),
            Object::Name(b"Fit".to_vec()),
        ]),
    );
    let outline_item_id = doc.add_object(Object::Dictionary(outline_item));

    let mut outlines = Dictionary::new();
    outlines.set("Type", Object::Name(b"Outlines".to_vec()));
    outlines.set("First", Object::Reference(outline_item_id));
    outlines.set("Last", Object::Reference(outline_item_id));
    outlines.set("Count", Object::Integer(1));
    let outlines_id = doc.add_object(Object::Dictionary(outlines));

    if let Some(Object::Dictionary(item)) = doc.objects.get_mut(&outline_item_id) {
        item.set("Parent", Object::Reference(outlines_id));
    }

    // ── Catalog + trailer ──────────────────────────────────────────
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    catalog.set("Outlines", Object::Reference(outlines_id));
    let catalog_id = doc.add_object(Object::Dictionary(catalog));
    doc.trailer.set("Root", Object::Reference(catalog_id));

    doc.save(path)
        .expect("failed to save the navigation test PDF");
}

/// Extracts a page dictionary's `MediaBox` as `(width, height)`, for
/// comparing against the known marker sizes from
/// [`build_fixture_with_navigation`].
fn page_media_box_size(page_dict: &Dictionary) -> (i64, i64) {
    let Ok(Object::Array(arr)) = page_dict.get(b"MediaBox") else {
        panic!("expected page to have a MediaBox");
    };
    let as_i64 = |o: &Object| match o {
        Object::Integer(n) => *n,
        Object::Real(r) => *r as i64,
        _ => panic!("expected a numeric MediaBox entry"),
    };
    (as_i64(&arr[2]), as_i64(&arr[3])) // [x0 y0 x1 y1] → width, height
}

// ════════════════════════════════════════════════════════════════
//  Test
// ════════════════════════════════════════════════════════════════

#[test]
fn compress_preserves_bookmark_and_link_navigation_targets() {
    let input = temp_pdf("navigation_in");
    let output = temp_pdf("navigation_out");
    build_fixture_with_navigation(&input);

    let opts = CompressOpts::default();
    compress(&input, &output, &opts).expect("compress should succeed");

    let doc = Document::load(&output).expect("output should be a valid, loadable PDF");

    // ── Bookmark must still resolve to the 200x200 page ───────────
    let root = match resolve(&doc, doc.trailer.get(b"Root").expect("Root must exist")) {
        Object::Dictionary(d) => d,
        _ => panic!("Root is not a dictionary"),
    };
    let outlines = match resolve(&doc, root.get(b"Outlines").expect("Outlines must survive")) {
        Object::Dictionary(d) => d,
        _ => panic!("Outlines is not a dictionary"),
    };
    let first_item = match resolve(
        &doc,
        outlines
            .get(b"First")
            .expect("First outline item must survive"),
    ) {
        Object::Dictionary(d) => d,
        _ => panic!("outline item is not a dictionary"),
    };
    let Object::Array(dest) = first_item.get(b"Dest").expect("bookmark Dest must survive") else {
        panic!("Dest is not an array");
    };
    let bookmark_target = match resolve(&doc, &dest[0]) {
        Object::Dictionary(d) => d,
        _ => panic!("bookmark destination is not a page dictionary"),
    };
    assert_eq!(
        page_media_box_size(bookmark_target),
        (200, 200),
        "bookmark must still point at the page it originally targeted"
    );

    // ── Link annotation must still resolve to the 300x300 page ────
    // Find page 1 (100x100) by its marker size, since object numbers
    // aren't assumed stable across the rewrite.
    let page1 = doc
        .objects
        .values()
        .filter_map(|o| {
            if let Object::Dictionary(d) = o {
                Some(d)
            } else {
                None
            }
        })
        .find(|d| {
            matches!(d.get(b"Type"), Ok(Object::Name(n)) if n.as_slice() == b"Page")
                && page_media_box_size(d) == (100, 100)
        })
        .expect("page 1 (100x100) must still exist after compression");

    let Object::Array(annots) = page1.get(b"Annots").expect("link annotation must survive") else {
        panic!("Annots is not an array");
    };
    let link = match resolve(&doc, &annots[0]) {
        Object::Dictionary(d) => d,
        _ => panic!("annotation is not a dictionary"),
    };
    let action = match resolve(&doc, link.get(b"A").expect("link action must survive")) {
        Object::Dictionary(d) => d,
        _ => panic!("action is not a dictionary"),
    };
    let Object::Array(link_dest) = action.get(b"D").expect("GoTo destination must survive") else {
        panic!("GoTo D is not an array");
    };
    let link_target = match resolve(&doc, &link_dest[0]) {
        Object::Dictionary(d) => d,
        _ => panic!("link destination is not a page dictionary"),
    };
    assert_eq!(
        page_media_box_size(link_target),
        (300, 300),
        "link annotation must still point at the page it originally targeted"
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}
