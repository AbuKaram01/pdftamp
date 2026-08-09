// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! PDF content analysis — pure data, zero printing.
//!
//! Useful before compressing a PDF (to understand what's inside it
//! and why a compression run might or might not save much space).
//! Kept separate from `render.rs` so this stays plain data —
//! testable without printing anything.

use crate::metadata::{self, MetadataInfo};
use lopdf::{Document, Object};
use std::{collections::HashMap, path::Path};

/// Aggregated stats for one group of streams sharing the same filter.
#[derive(Debug, Clone, Default)]
pub struct FilterStats {
    /// Number of streams in this group.
    pub count: usize,
    /// Total size, in bytes, of this group's stream content
    /// (compressed size, as stored in the PDF — not decoded size).
    pub bytes: u64,
}

impl FilterStats {
    /// Records one more stream of `n` bytes in this group.
    fn add(&mut self, n: usize) {
        self.count += 1;
        self.bytes += n as u64;
    }
}

/// Whether a given filter is one we know how to compress.
/// `render.rs` uses this to color/label results without
/// duplicating the filter list itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterSupport {
    /// We actively compress streams using this filter.
    Supported,
    /// We recognize the filter but don't compress it yet (e.g. JPEG 2000).
    KnownUnsupported,
    /// Not a filter we have any specific handling for.
    Unknown,
}

/// Full breakdown of a PDF's image and content-stream filters.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    /// Image stats, keyed by filter name (`DCTDecode`, `FlateDecode`, ...).
    pub images: HashMap<String, FilterStats>,
    /// Content streams with no filter at all — cheap, guaranteed wins.
    pub raw_streams: FilterStats,
    /// Content streams already compressed with `FlateDecode`.
    pub flate_streams: FilterStats,
    /// Anything else (multi-filter chains, uncommon filters, ...).
    pub other_streams: FilterStats,
    /// Document-level `/Info` and XMP metadata found in the file.
    /// Shown so you can decide *before* compressing whether you want
    /// to remove it (`strip_metadata` — opt-in only, kept by default)
    /// — for example if a JPEG's EXIF carries copyright info, or the
    /// `/Info` dictionary matters for a legal document's provenance.
    pub metadata: MetadataInfo,
}

impl Analysis {
    /// Classifies a filter name by whether `pdftamp` currently
    /// compresses it.
    pub fn support_for(filter: &str) -> FilterSupport {
        match filter {
            "DCTDecode" | "FlateDecode" | "No filter (raw)" | "LZWDecode" => {
                FilterSupport::Supported
            }
            "JPXDecode" | "CCITTFaxDecode" | "JBIG2Decode" => FilterSupport::KnownUnsupported,
            _ => FilterSupport::Unknown,
        }
    }
}

/// Loads `path` and returns a breakdown of its image/content-stream
/// filters. The only I/O here is reading the file (plus, if needed, a
/// `qpdf` structural-repair attempt — see `loader.rs`).
///
/// `allow_decrypt`: if the PDF is encrypted, pass `true` to let it be
/// decrypted automatically (only works for an empty/no-required
/// password). Defaults to refusing outright otherwise — `analyze()`
/// won't bypass a file's protection just to inspect it unless you
/// explicitly say that's OK.
///
/// # Errors
///
/// Returns an error if `path` can't be read, isn't a valid PDF, or is
/// encrypted and either `allow_decrypt` is `false` or decryption
/// fails (e.g. a real password is required).
pub fn analyze(path: &Path, allow_decrypt: bool) -> anyhow::Result<Analysis> {
    let tools = crate::tools::ToolSet::detect();
    let (doc, repaired) = crate::loader::load_with_repair(path, &tools, allow_decrypt)?;
    let result = analyze_doc(&doc);
    crate::loader::cleanup(repaired);
    Ok(result)
}

/// Same as [`analyze`], but operates on an already-loaded [`Document`].
/// Useful for tests or callers that already have the document open.
pub fn analyze_doc(doc: &Document) -> Analysis {
    let mut result = Analysis {
        metadata: metadata::inspect(doc),
        ..Default::default()
    };

    for obj in doc.objects.values() {
        let Object::Stream(stream) = obj else {
            continue;
        };

        // PDF-internal bookkeeping streams (the cross-reference
        // stream, compressed-object containers) aren't user content
        // — they're structural plumbing the writer regenerates on
        // every save. An unfiltered one would otherwise get
        // miscounted as a "raw content stream" here.
        if is_internal_structure(&stream.dict) {
            continue;
        }

        let is_img = stream
            .dict
            .get(b"Subtype")
            .map(|o| matches!(o, Object::Name(n) if n.as_slice() == b"Image"))
            .unwrap_or(false);

        let size = stream.content.len();
        let filter = filter_name(&stream.dict);

        if is_img {
            result.images.entry(filter).or_default().add(size);
        } else {
            match filter.as_str() {
                "No filter (raw)" => result.raw_streams.add(size),
                "FlateDecode" => result.flate_streams.add(size),
                _ => result.other_streams.add(size),
            }
        }
    }

    result
}

/// `true` for PDF-internal structural streams — the cross-reference
/// stream (`/Type /XRef`, PDF 1.5+) and compressed object containers
/// (`/Type /ObjStm`) — which the writer regenerates wholesale on every
/// save and which aren't meaningful "content" to analyze or compress.
fn is_internal_structure(dict: &lopdf::Dictionary) -> bool {
    matches!(
        dict.get(b"Type"),
        Ok(Object::Name(n)) if n.as_slice() == b"XRef" || n.as_slice() == b"ObjStm"
    )
}

/// Extracts the human-readable filter name from a stream dictionary's
/// `/Filter` entry: the filter's own name, `"Multiple filters"` for a
/// filter array (a filter chain), or `"No filter (raw)"` if absent.
fn filter_name(dict: &lopdf::Dictionary) -> String {
    match dict.get(b"Filter") {
        Ok(Object::Name(n)) => String::from_utf8_lossy(n).into_owned(),
        Ok(Object::Array(_)) => "Multiple filters".into(),
        _ => "No filter (raw)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Stream};

    fn stream_with_type(type_name: Option<&[u8]>, filter: Option<&[u8]>) -> Object {
        let mut dict = Dictionary::new();
        if let Some(t) = type_name {
            dict.set("Type", Object::Name(t.to_vec()));
        }
        if let Some(f) = filter {
            dict.set("Filter", Object::Name(f.to_vec()));
        }
        Object::Stream(Stream::new(dict, b"irrelevant".to_vec()))
    }

    #[test]
    fn xref_stream_is_excluded_even_when_unfiltered() {
        let mut doc = Document::new();
        doc.objects
            .insert((1, 0), stream_with_type(Some(b"XRef"), None));

        let result = analyze_doc(&doc);
        assert_eq!(
            result.raw_streams.count, 0,
            "an unfiltered XRef stream must not count as raw content"
        );
        assert_eq!(result.other_streams.count, 0);
    }

    #[test]
    fn objstm_stream_is_excluded_even_when_unfiltered() {
        let mut doc = Document::new();
        doc.objects
            .insert((1, 0), stream_with_type(Some(b"ObjStm"), None));

        let result = analyze_doc(&doc);
        assert_eq!(
            result.raw_streams.count, 0,
            "an unfiltered ObjStm stream must not count as raw content"
        );
    }

    #[test]
    fn a_genuine_unfiltered_content_stream_is_still_counted() {
        let mut doc = Document::new();
        doc.objects.insert((1, 0), stream_with_type(None, None));

        let result = analyze_doc(&doc);
        assert_eq!(
            result.raw_streams.count, 1,
            "a real content stream with no Type/Filter must still be counted"
        );
    }
}
