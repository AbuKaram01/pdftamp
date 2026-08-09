// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Image (re)compression — this is where most of the file-size savings
//! come from (the 80% in the 80/20 rule this project follows).
//!
//! We support three input encodings, all converging on JPEG output:
//!   1. `DCTDecode` (already JPEG) → re-encode at a lower quality
//!   2. `FlateDecode` / no filter (raw pixels) → decode, then encode as JPEG
//!   3. `LZWDecode` (older PDFs) → decode, then encode as JPEG
//!
//! External tools (`jpegoptim`) are preferred when available since they
//! tend to outperform a from-scratch encoder; see `tools.rs`.

use flate2::read::ZlibDecoder;
use image::{codecs::jpeg::JpegEncoder, ExtendedColorType, ImageFormat};
use lopdf::{Object, Stream};
use std::io::Read;

use crate::predictor;
use crate::tools::{self, ToolSet};

// ════════════════════════════════════════════════════════════════
//  Detection helpers
// ════════════════════════════════════════════════════════════════

/// `true` if `stream` is an Image XObject already encoded as JPEG.
pub fn is_jpeg_image(stream: &Stream) -> bool {
    is_image(stream) && has_filter(stream, b"DCTDecode")
}

/// `true` if `stream` is an Image XObject stored as raw pixels — either
/// explicitly `FlateDecode`-compressed, or with no filter at all.
pub fn is_flate_image(stream: &Stream) -> bool {
    if !is_image(stream) {
        return false;
    }
    has_filter(stream, b"FlateDecode") || no_filter(stream)
}

/// `true` if `stream` is an Image XObject compressed with the older
/// `LZWDecode` filter, common in PDFs from the 1990s/early 2000s.
pub fn is_lzw_image(stream: &Stream) -> bool {
    is_image(stream) && has_filter(stream, b"LZWDecode")
}

/// `true` if `stream`'s `/Subtype` is `/Image` — i.e. it's an Image
/// XObject rather than a content stream or some other stream type.
fn is_image(stream: &Stream) -> bool {
    stream
        .dict
        .get(b"Subtype")
        .map(|o| matches!(o, Object::Name(n) if n.as_slice() == b"Image"))
        .unwrap_or(false)
}

/// `true` if `stream`'s `/Filter` is exactly the single name `name`
/// (not a filter array/chain).
fn has_filter(stream: &Stream, name: &[u8]) -> bool {
    stream
        .dict
        .get(b"Filter")
        .map(|o| matches!(o, Object::Name(n) if n.as_slice() == name))
        .unwrap_or(false)
}

/// `true` if `stream` has no `/Filter` entry at all.
fn no_filter(stream: &Stream) -> bool {
    stream.dict.get(b"Filter").is_err()
}

// ════════════════════════════════════════════════════════════════
//  Strategy 1 — re-encode existing JPEG images
// ════════════════════════════════════════════════════════════════

/// Re-encodes a `DCTDecode` image at a lower JPEG quality.
///
/// `strip_metadata` controls EXIF/ICC/comment removal independently
/// of `quality`. This matters for the fallback path: when `jpegoptim`
/// isn't installed, the only available re-encoder is the `image`
/// crate, which always drops EXIF as a side effect of its decode/encode
/// round-trip — it has no way to preserve it. So if the caller asked
/// to *keep* metadata and `jpegoptim` isn't available, we skip
/// re-encoding entirely rather than silently breaking that promise.
///
/// Returns `None` for CMYK images (left untouched — see `is_cmyk`) or
/// if the result wouldn't actually be smaller.
pub fn compress_jpeg(
    stream: &Stream,
    quality: u8,
    t: &ToolSet,
    strip_metadata: bool,
) -> Option<(Stream, i64)> {
    if is_cmyk(stream) {
        return None;
    }

    let original = &stream.content;

    let compressed = if t.jpegoptim {
        tools::jpegoptim_lossy(original, quality, strip_metadata)
            .or_else(|| tools::jpegoptim_lossless(original, strip_metadata))
    } else if strip_metadata {
        // Dropping metadata was requested anyway, so the image crate's
        // inability to preserve it isn't a problem here.
        jpeg_via_crate(original, quality)
    } else {
        // Metadata must be kept, but our only fallback encoder can't
        // do that — refuse rather than silently strip it.
        None
    };

    apply(stream, compressed?)
}

/// Fallback JPEG re-encoder using the `image` crate, used only when
/// `jpegoptim` isn't available on the system.
fn jpeg_via_crate(original: &[u8], quality: u8) -> Option<Vec<u8>> {
    let img = image::load_from_memory_with_format(original, ImageFormat::Jpeg).ok()?;
    let (w, h) = (img.width(), img.height());
    let mut buf = Vec::new();
    match img.color() {
        image::ColorType::L8 => {
            let g = img.to_luma8();
            JpegEncoder::new_with_quality(&mut buf, quality)
                .encode(g.as_raw(), w, h, ExtendedColorType::L8)
                .ok()?;
        }
        _ => {
            let rgb = img.to_rgb8();
            JpegEncoder::new_with_quality(&mut buf, quality)
                .encode(rgb.as_raw(), w, h, ExtendedColorType::Rgb8)
                .ok()?;
        }
    }
    Some(buf)
}

/// Removes embedded metadata (EXIF/ICC profile/comments) from a JPEG
/// **without** touching the pixel data — a genuinely lossless size
/// reduction. Used by [`Profile::Lossless`](crate::profiles::Profile).
///
/// Requires `jpegoptim`: there's no safe pure-Rust way to strip JPEG
/// metadata without risking a re-encode, so this returns `None` if
/// the tool isn't installed rather than silently falling back to a
/// lossy path.
pub fn strip_jpeg_metadata(stream: &Stream, t: &ToolSet) -> Option<(Stream, i64)> {
    if !t.jpegoptim {
        return None;
    }
    let stripped = tools::jpegoptim_lossless(&stream.content, true)?;
    apply(stream, stripped)
}

// ════════════════════════════════════════════════════════════════
//  Strategy 2 — FlateDecode / raw pixels → JPEG
// ════════════════════════════════════════════════════════════════

/// Decodes a raw/`FlateDecode` image, undoes any PNG predictor, and
/// re-encodes it as JPEG.
///
/// Only handles the common case: 8-bit, `DeviceRGB` or `DeviceGray`.
/// Anything else (16-bit, indexed palettes, ICC-based colorspaces,
/// CMYK) is left untouched rather than risking incorrect output.
///
/// `strip_metadata` is threaded through to the optional `jpegoptim`
/// polish pass for API consistency, though it has no real effect
/// here in practice — raw PDF image streams never carry EXIF to
/// begin with, so there's nothing to preserve or strip either way.
pub fn compress_flate_to_jpeg(
    stream: &Stream,
    quality: u8,
    t: &ToolSet,
    strip_metadata: bool,
) -> Option<(Stream, i64)> {
    if is_cmyk(stream) {
        return None;
    }

    let w = get_u32(stream, b"Width")?;
    let h = get_u32(stream, b"Height")?;
    let bits = get_u32(stream, b"BitsPerComponent").unwrap_or(8);
    if bits != 8 {
        return None;
    }

    let (channels, color_type) = color_info(stream)?;

    // Step 1: undo zlib compression (no-op if there was no Filter at all).
    let decompressed = if has_filter(stream, b"FlateDecode") {
        let mut d = ZlibDecoder::new(&stream.content[..]);
        let mut buf = Vec::new();
        d.read_to_end(&mut buf).ok()?;
        buf
    } else {
        stream.content.clone()
    };

    // Step 2: undo PNG-style row prediction, if the encoder applied one.
    // Predictor 2 (TIFF horizontal differencing) is only valid for
    // LZWDecode per the PDF spec — a FlateDecode stream marked with
    // Predictor 2 would be a non-compliant PDF generator's mistake.
    // Rather than guessing how to undo it (and risking the same class
    // of corruption a wrong assumption caused before), we refuse.
    if get_predictor(stream) == 2 {
        return None;
    }

    let pixels = if has_png_predictor(stream) {
        predictor::undo(&decompressed, w, h, channels as usize)?
    } else {
        decompressed
    };

    let expected = w as usize * h as usize * channels as usize;
    if pixels.len() != expected {
        return None;
    }

    // Step 3: encode as JPEG.
    let mut initial = Vec::new();
    JpegEncoder::new_with_quality(&mut initial, quality)
        .encode(&pixels, w, h, color_type)
        .ok()?;

    // Step 4: let jpegoptim squeeze out a bit more, if available.
    let final_bytes = if t.jpegoptim {
        tools::jpegoptim_lossy(&initial, quality, strip_metadata).unwrap_or(initial)
    } else {
        initial
    };

    let saved = stream.content.len() as i64 - final_bytes.len() as i64;
    if saved <= 0 {
        return None;
    }

    let mut new_stream = stream.clone();
    new_stream.content = final_bytes;
    let len = new_stream.content.len() as i64;
    new_stream
        .dict
        .set("Filter", Object::Name(b"DCTDecode".to_vec()));
    new_stream.dict.set("Length", Object::Integer(len));
    new_stream.dict.remove(b"DecodeParms"); // predictor params no longer apply

    Some((new_stream, saved))
}

// ════════════════════════════════════════════════════════════════
//  Strategy 3 — LZWDecode → JPEG (older PDFs)
// ════════════════════════════════════════════════════════════════

/// Decodes an `LZWDecode` image, undoes whichever predictor was used
/// (TIFF-style horizontal differencing or PNG-style), and re-encodes
/// it as JPEG. Mirrors `compress_flate_to_jpeg`, differing only in the
/// decompression step.
pub fn compress_lzw_to_jpeg(
    stream: &Stream,
    quality: u8,
    t: &ToolSet,
    strip_metadata: bool,
) -> Option<(Stream, i64)> {
    if is_cmyk(stream) {
        return None;
    }

    let w = get_u32(stream, b"Width")?;
    let h = get_u32(stream, b"Height")?;
    let bits = get_u32(stream, b"BitsPerComponent").unwrap_or(8);
    if bits != 8 {
        return None;
    }

    let (channels, color_type) = color_info(stream)?;

    let decompressed = decompress_lzw(&stream.content)?;

    let pixels = match get_predictor(stream) {
        2 => {
            // TIFF predictor 2: horizontal differencing per row.
            let mut p = decompressed;
            undo_tiff_predictor(&mut p, w, channels);
            p
        }
        p if p >= 10 => {
            // PNG-style predictor — rare with LZW, but spec-legal.
            predictor::undo(&decompressed, w, h, channels as usize)?
        }
        _ => decompressed, // no predictor applied
    };

    let expected = w as usize * h as usize * channels as usize;
    if pixels.len() != expected {
        return None;
    }

    let mut initial = Vec::new();
    JpegEncoder::new_with_quality(&mut initial, quality)
        .encode(&pixels, w, h, color_type)
        .ok()?;

    let final_bytes = if t.jpegoptim {
        tools::jpegoptim_lossy(&initial, quality, strip_metadata).unwrap_or(initial)
    } else {
        initial
    };

    let saved = stream.content.len() as i64 - final_bytes.len() as i64;
    if saved <= 0 {
        return None;
    }

    let mut new_stream = stream.clone();
    new_stream.content = final_bytes;
    let len = new_stream.content.len() as i64;
    new_stream
        .dict
        .set("Filter", Object::Name(b"DCTDecode".to_vec()));
    new_stream.dict.set("Length", Object::Integer(len));
    new_stream.dict.remove(b"DecodeParms");

    Some((new_stream, saved))
}

/// Decodes LZW-compressed bytes using the TIFF/PDF variant (MSB-first
/// bit order, early code-width change), which is what PDF's
/// `LZWDecode` filter specifies.
fn decompress_lzw(data: &[u8]) -> Option<Vec<u8>> {
    use weezl::{decode::Decoder, BitOrder};
    let mut decoder = Decoder::with_tiff_size_switch(BitOrder::Msb, 8);
    let mut out = Vec::new();
    decoder.into_stream(&mut out).decode_all(data).status.ok()?;
    Some(out)
}

/// Reads the `Predictor` value from `DecodeParms`, defaulting to `1`
/// (no predictor) when absent — matches the PDF spec's default.
fn get_predictor(stream: &Stream) -> u32 {
    match stream.dict.get(b"DecodeParms") {
        Ok(Object::Dictionary(d)) => match d.get(b"Predictor") {
            Ok(Object::Integer(p)) => *p as u32,
            _ => 1,
        },
        _ => 1,
    }
}

/// Reverses TIFF predictor 2 (horizontal differencing): each pixel
/// was stored as a delta from the pixel `bpp` bytes before it, so we
/// reconstruct by running a prefix sum across each row.
fn undo_tiff_predictor(data: &mut [u8], width: u32, channels: u32) {
    let row_len = width as usize * channels as usize;
    let bpp = channels as usize;
    for row in data.chunks_mut(row_len) {
        for i in bpp..row.len() {
            row[i] = row[i].wrapping_add(row[i - bpp]);
        }
    }
}

// ════════════════════════════════════════════════════════════════
//  Shared helpers
// ════════════════════════════════════════════════════════════════

/// Wraps a successfully-compressed byte buffer into a new `Stream`,
/// bailing out if the result isn't actually smaller than the original.
fn apply(stream: &Stream, compressed: Vec<u8>) -> Option<(Stream, i64)> {
    let saved = stream.content.len() as i64 - compressed.len() as i64;
    if saved <= 0 {
        return None;
    }
    let mut s = stream.clone();
    s.dict
        .set("Length", Object::Integer(compressed.len() as i64));
    s.content = compressed;
    Some((s, saved))
}

/// Reads an integer dictionary entry (e.g. `/Width`, `/Height`) as a
/// `u32`. Returns `None` if the key is missing or isn't an integer.
fn get_u32(s: &Stream, key: &[u8]) -> Option<u32> {
    match s.dict.get(key) {
        Ok(Object::Integer(n)) => Some(*n as u32),
        _ => None,
    }
}

/// CMYK images need a color-managed conversion to RGB to look correct
/// as JPEG; we don't attempt that here, so they're left untouched.
fn is_cmyk(s: &Stream) -> bool {
    s.dict
        .get(b"ColorSpace")
        .map(|o| matches!(o, Object::Name(n) if n.as_slice() == b"DeviceCMYK"))
        .unwrap_or(false)
}

/// `true` if `DecodeParms` specifies a PNG-style predictor
/// (`Predictor >= 10`, per the PDF spec's predictor value mapping).
fn has_png_predictor(s: &Stream) -> bool {
    match s.dict.get(b"DecodeParms") {
        Ok(Object::Dictionary(d)) => {
            matches!(d.get(b"Predictor"), Ok(Object::Integer(p)) if *p >= 10)
        }
        _ => false,
    }
}

/// Maps a PDF `ColorSpace` name to (channel count, JPEG color type).
/// Returns `None` for anything beyond plain RGB/Gray (indexed
/// palettes, ICC profiles, etc.) — those are left untouched.
fn color_info(stream: &Stream) -> Option<(u32, ExtendedColorType)> {
    match stream.dict.get(b"ColorSpace").ok()? {
        Object::Name(n) => match n.as_slice() {
            b"DeviceRGB" => Some((3, ExtendedColorType::Rgb8)),
            b"DeviceGray" => Some((1, ExtendedColorType::L8)),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolSet;
    use lopdf::Dictionary;

    /// Encodes a small, noisy (non-uniform) RGB JPEG at a given
    /// quality, so re-encoding at a lower quality reliably shrinks it.
    /// A solid-color test image wouldn't work here: JPEG already
    /// compresses uniform color extremely well at any quality, so
    /// there'd be nothing left to save by lowering it further.
    fn noisy_jpeg_bytes(quality: u8) -> Vec<u8> {
        let (w, h) = (32u32, 32u32);
        let mut state = 7u32;
        let pixels: Vec<u8> = (0..(w * h * 3))
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xFF) as u8
            })
            .collect();

        let mut buf = Vec::new();
        JpegEncoder::new_with_quality(&mut buf, quality)
            .encode(&pixels, w, h, ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    /// Wraps JPEG bytes in a minimal Image XObject stream dictionary
    /// (`/Filter /DCTDecode`, 32×32) suitable for [`compress_jpeg`].
    fn jpeg_stream(content: Vec<u8>) -> Stream {
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"XObject".to_vec()));
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Filter", Object::Name(b"DCTDecode".to_vec()));
        dict.set("Width", Object::Integer(32));
        dict.set("Height", Object::Integer(32));
        dict.set("Length", Object::Integer(content.len() as i64));
        Stream::new(dict, content)
    }

    /// A [`ToolSet`] with every external tool marked unavailable —
    /// exercises the pure-Rust fallback paths.
    fn no_tools() -> ToolSet {
        ToolSet {
            jpegoptim: false,
            oxipng: false,
            pngquant: false,
            qpdf: false,
        }
    }

    #[test]
    fn keep_metadata_without_jpegoptim_skips_recompression() {
        let stream = jpeg_stream(noisy_jpeg_bytes(95));
        let result = compress_jpeg(&stream, 40, &no_tools(), false);
        assert!(
            result.is_none(),
            "should refuse to recompress when metadata must be kept but can't be"
        );
    }

    #[test]
    fn strip_metadata_without_jpegoptim_uses_image_crate_fallback() {
        let stream = jpeg_stream(noisy_jpeg_bytes(95));
        let result = compress_jpeg(&stream, 40, &no_tools(), true);
        assert!(
            result.is_some(),
            "expected the image-crate fallback to shrink a quality-95 noisy JPEG at quality 40"
        );
    }

    #[test]
    fn flate_image_with_nonstandard_tiff_predictor_is_refused() {
        // A FlateDecode image marked with Predictor 2 (TIFF horizontal
        // differencing) is non-compliant per the PDF spec — Predictor 2
        // is only valid for LZWDecode. Rather than guessing how to
        // undo it, we must refuse outright.
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"XObject".to_vec()));
        dict.set("Subtype", Object::Name(b"Image".to_vec()));
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        dict.set("ColorSpace", Object::Name(b"DeviceRGB".to_vec()));
        dict.set("BitsPerComponent", Object::Integer(8));
        dict.set("Width", Object::Integer(4));
        dict.set("Height", Object::Integer(4));

        let mut parms = Dictionary::new();
        parms.set("Predictor", Object::Integer(2));
        dict.set("DecodeParms", Object::Dictionary(parms));

        // Content doesn't matter — we should bail before ever
        // trying to interpret it.
        let stream = Stream::new(dict, vec![0u8; 10]);

        let result = compress_flate_to_jpeg(&stream, 75, &no_tools(), true);
        assert!(
            result.is_none(),
            "should refuse a non-compliant Predictor=2 on FlateDecode rather than guess"
        );
    }
}
