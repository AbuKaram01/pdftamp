// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Document-level PDF metadata — distinct from per-image EXIF.
//!
//! A PDF carries two separate metadata layers, both unrelated to the
//! page content itself:
//!   - the `/Info` dictionary (Author, Producer, Creator, timestamps, ...)
//!   - an optional XMP metadata stream, referenced from the document
//!     Catalog via `/Metadata` (can carry the same fields, sometimes more)
//!
//! Neither affects how the document looks or prints — they're purely
//! identifying/provenance data, which makes them an easy, zero-risk
//! thing to strip for both privacy and a small size reduction.

use lopdf::{Document, Object};

/// What document-level metadata was found, before stripping.
#[derive(Debug, Clone, Default)]
pub struct MetadataInfo {
    /// Field names present in the `/Info` dictionary
    /// (e.g. `"Author"`, `"Producer"`, `"CreationDate"`).
    pub info_fields: Vec<String>,
    /// Size in bytes of the XMP metadata stream, if the document has one.
    pub xmp_bytes: Option<u64>,
}

impl MetadataInfo {
    /// `true` if no `/Info` fields and no XMP stream were found.
    pub fn is_empty(&self) -> bool {
        self.info_fields.is_empty() && self.xmp_bytes.is_none()
    }
}

/// Inspects `doc` without modifying it.
pub fn inspect(doc: &Document) -> MetadataInfo {
    let mut info = MetadataInfo::default();

    if let Some(dict) = info_dict(doc) {
        info.info_fields = dict
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect();
    }

    if let Some(stream) = xmp_stream(doc) {
        info.xmp_bytes = Some(stream.content.len() as u64);
    }

    info
}

/// Removes the `/Info` dictionary and the XMP `/Metadata` stream
/// entirely. This doesn't just unlink the references — the underlying
/// objects are dropped from `doc.objects`, so they aren't written out
/// at all when the document is saved, rather than being left as
/// orphaned (but still recoverable) objects in the file.
///
/// Returns what was actually found and removed, so callers can report
/// it (e.g. "stripped: Author, Producer, CreationDate + 2.1 KB XMP").
pub fn strip(doc: &mut Document) -> MetadataInfo {
    let removed = inspect(doc);

    // ── /Info dictionary, referenced from the trailer ───────────
    if let Ok(Object::Reference(id)) = doc.trailer.get(b"Info") {
        let id = *id;
        doc.objects.remove(&id);
    }
    doc.trailer.remove(b"Info");

    // ── XMP /Metadata stream, referenced from the Catalog ───────
    if let Some(root_id) = catalog_id(doc) {
        let metadata_id = match doc.objects.get(&root_id) {
            Some(Object::Dictionary(d)) => match d.get(b"Metadata") {
                Ok(Object::Reference(id)) => Some(*id),
                _ => None,
            },
            _ => None,
        };

        if let Some(meta_id) = metadata_id {
            doc.objects.remove(&meta_id);
        }
        if let Some(Object::Dictionary(d)) = doc.objects.get_mut(&root_id) {
            d.remove(b"Metadata");
        }
    }

    removed
}

// ════════════════════════════════════════════════════════════════
//  Lookup helpers
// ════════════════════════════════════════════════════════════════

/// Resolves the trailer's `/Info` reference to the dictionary it
/// points at, if present and well-formed.
fn info_dict(doc: &Document) -> Option<&lopdf::Dictionary> {
    match doc.trailer.get(b"Info") {
        Ok(Object::Reference(id)) => match doc.objects.get(id) {
            Some(Object::Dictionary(d)) => Some(d),
            _ => None,
        },
        _ => None,
    }
}

/// The object ID the trailer's `/Root` entry points at — i.e. the
/// document Catalog.
fn catalog_id(doc: &Document) -> Option<lopdf::ObjectId> {
    match doc.trailer.get(b"Root") {
        Ok(Object::Reference(id)) => Some(*id),
        _ => None,
    }
}

/// Resolves the Catalog's `/Metadata` reference to the XMP stream it
/// points at, if present and well-formed.
fn xmp_stream(doc: &Document) -> Option<&lopdf::Stream> {
    let root_id = catalog_id(doc)?;
    match doc.objects.get(&root_id) {
        Some(Object::Dictionary(d)) => match d.get(b"Metadata") {
            Ok(Object::Reference(meta_id)) => match doc.objects.get(meta_id) {
                Some(Object::Stream(s)) => Some(s),
                _ => None,
            },
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, StringFormat};

    /// Builds a minimal document with both an `/Info` dictionary
    /// (`Author`, `Producer`) and an XMP `/Metadata` stream attached
    /// to its Catalog, for exercising [`inspect`] and [`strip`].
    fn doc_with_info_and_xmp() -> Document {
        let mut doc = Document::new();

        let mut info = Dictionary::new();
        info.set(
            "Author",
            Object::String(b"Test Author".to_vec(), StringFormat::Literal),
        );
        info.set(
            "Producer",
            Object::String(b"pdftamp tests".to_vec(), StringFormat::Literal),
        );
        let info_id = doc.add_object(Object::Dictionary(info));
        doc.trailer.set("Info", Object::Reference(info_id));

        let xmp_dict = Dictionary::new();
        let xmp_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            xmp_dict,
            b"<x:xmpmeta>fake xmp payload</x:xmpmeta>".to_vec(),
        )));

        let mut catalog = Dictionary::new();
        catalog.set("Type", Object::Name(b"Catalog".to_vec()));
        catalog.set("Metadata", Object::Reference(xmp_id));
        let catalog_id = doc.add_object(Object::Dictionary(catalog));
        doc.trailer.set("Root", Object::Reference(catalog_id));

        doc
    }

    #[test]
    fn inspect_detects_info_fields_and_xmp_size() {
        let doc = doc_with_info_and_xmp();
        let info = inspect(&doc);

        assert!(info.info_fields.contains(&"Author".to_string()));
        assert!(info.info_fields.contains(&"Producer".to_string()));
        assert!(info.xmp_bytes.unwrap() > 0);
    }

    #[test]
    fn strip_removes_info_and_xmp_objects_entirely() {
        let mut doc = doc_with_info_and_xmp();
        let info_id = match doc.trailer.get(b"Info") {
            Ok(Object::Reference(id)) => *id,
            _ => panic!(),
        };

        strip(&mut doc);

        // The trailer no longer points to an Info dict.
        assert!(doc.trailer.get(b"Info").is_err());
        // The underlying object was dropped, not just unlinked.
        assert!(!doc.objects.contains_key(&info_id));

        // The Catalog no longer references Metadata.
        let root_id = catalog_id(&doc).unwrap();
        if let Some(Object::Dictionary(d)) = doc.objects.get(&root_id) {
            assert!(d.get(b"Metadata").is_err());
        }

        assert!(inspect(&doc).is_empty());
    }

    #[test]
    fn inspect_on_clean_document_is_empty() {
        let doc = Document::new();
        assert!(inspect(&doc).is_empty());
    }
}
