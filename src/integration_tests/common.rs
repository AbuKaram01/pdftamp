// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Shared fixture-building and assertion helpers for the tests in
//! this module's sibling files.
//!
//! Not every test file uses every helper here, so `#![allow(dead_code)]`
//! below avoids "function is never used" warnings wherever a given
//! one isn't needed.
#![allow(dead_code)]

use lopdf::{Document, Object, Stream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

/// Pseudo-random deterministic bytes (xorshift32). This doesn't
/// compress well under Deflate (lossless), but JPEG (lossy) can still
/// shrink it by discarding high-frequency detail — which keeps the
/// test's expectations predictable.
pub fn noise_bytes(n: usize, seed: u32) -> Vec<u8> {
    let mut state = seed.max(1);
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state & 0xFF) as u8
        })
        .collect()
}

/// Process-wide counter, combined with the PID, to guarantee unique
/// temp paths — even across separate `cargo test` invocations in the
/// same session, where PID reuse could otherwise let a leftover file
/// from a previous run collide with this one (e.g. if `doc.save()`
/// doesn't truncate a pre-existing, larger file at that path).
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Builds a unique path under the system temp directory for a test
/// PDF fixture named `name` (e.g. `temp_pdf("basic")`).
pub fn temp_pdf(name: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "pdfsq_test_{}_{}_{}.pdf",
        std::process::id(),
        n,
        name
    ))
}

/// Resolves `obj` one level if it's a `Reference`, otherwise returns
/// it as-is. Just enough indirection-following for this test's needs.
pub fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.objects.get(id).unwrap_or(obj),
        other => other,
    }
}

/// Decodes a stream's content according to its `/Filter`, so tests
/// can compare *decoded* bytes rather than assuming a particular
/// filter was or wasn't applied. `compress()` may opportunistically
/// deflate an unfiltered stream it doesn't specifically recognize
/// (appearance streams, JS-as-stream, embedded files, ...) — that's
/// legitimate lossless compression, not corruption, so a test that
/// only checked "bytes are identical" would wrongly fail on a
/// perfectly fine deflate. Decoding first is the correct check.
pub fn decoded_stream_content(stream: &Stream) -> Vec<u8> {
    let is_flate = matches!(
        stream.dict.get(b"Filter"),
        Ok(Object::Name(n)) if n.as_slice() == b"FlateDecode"
    );
    if !is_flate {
        return stream.content.clone();
    }
    let mut decoder = flate2::read::ZlibDecoder::new(&stream.content[..]);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out)
        .expect("FlateDecode stream must decode cleanly");
    out
}
