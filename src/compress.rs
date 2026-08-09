// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Compression pipeline — zero printing here; results are returned
//! as plain structs so `render.rs` can decide how to display them.

use crate::paths::{self, OnConflict};
use crate::tools::ToolSet;
use crate::{images, streams};
use anyhow::Result;
use lopdf::Object;
use std::path::{Path, PathBuf};

/// Options controlling a single `compress()` call.
#[derive(Debug, Clone)]
pub struct CompressOpts {
    /// JPEG re-encode quality (1-95). Lower = smaller, more lossy.
    /// Ignored when `lossless` is `true`.
    pub quality: u8,
    /// When `true`, pixel data is never re-encoded: JPEG images only
    /// get their metadata stripped, and raw/LZW images are left
    /// untouched (converting them to JPEG would always be lossy,
    /// regardless of quality setting). Content streams are still
    /// deflated, since that's always lossless.
    pub lossless: bool,
    /// When `true`, the document-level `/Info` dictionary, XMP
    /// `/Metadata` stream, and per-image JPEG EXIF/ICC data are
    /// removed. **Defaults to `false`**: removing this isn't
    /// compression (the bytes saved are negligible) — it's a
    /// separate, privacy-motivated decision the caller has to opt
    /// into explicitly. `compress()` only does what its name says
    /// unless told otherwise.
    pub strip_metadata: bool,
    /// When `true`, an encrypted PDF (even one with an empty/no-
    /// required user password) may be automatically decrypted via
    /// `qpdf` so it can be compressed. **Defaults to `false`**:
    /// encryption is a property of the original file the caller
    /// didn't necessarily ask to have removed, even when it's
    /// trivially bypassable. Without this, `compress()` refuses any
    /// encrypted PDF outright rather than silently changing that
    /// property of the output.
    pub allow_decrypt: bool,
    /// When `true`, `Report::events` is populated with one entry per
    /// object that was actually modified — useful for progress bars
    /// or verbose logging. Has a small overhead, so it's opt-in.
    pub emit_events: bool,
    /// What to do if the output path already exists. See
    /// [`OnConflict`] — defaults to [`OnConflict::Refuse`], so
    /// `compress()` never overwrites or renames anything unless
    /// explicitly told to.
    pub on_conflict: OnConflict,
    /// When `true`, `compress()` runs its full pipeline — loading,
    /// recompressing every eligible image, deflating raw streams,
    /// inspecting (and, if `strip_metadata` is set, simulating the
    /// removal of) document metadata — entirely in memory, and
    /// reports exactly what it would have done, but never creates,
    /// overwrites, renames, or otherwise touches anything at `output`
    /// or its parent directories. **Defaults to `false`**.
    ///
    /// The returned [`Report`] is the genuine result of actually
    /// running the pipeline against `input`, not a guess: every
    /// count, every byte figure, and every metadata field it reports
    /// is the same as a real run would produce — the only difference
    /// is that nothing gets written. See [`Report::final_output`] for
    /// how the path a dry run *would* have used is determined without
    /// creating it.
    pub dry_run: bool,
}

impl Default for CompressOpts {
    fn default() -> Self {
        Self {
            quality: 75,
            lossless: false,
            strip_metadata: false,
            allow_decrypt: false,
            emit_events: false,
            on_conflict: OnConflict::Refuse,
            dry_run: false,
        }
    }
}

/// One per-object outcome, emitted only when `emit_events` is set.
#[derive(Debug, Clone)]
pub enum CompressEvent {
    /// A JPEG image had its pixel data re-encoded at a lower quality
    /// (or, in lossless mode, just had its metadata stripped).
    JpegRecompressed {
        /// The PDF object ID, formatted for display (e.g. `"(12, 0)"`).
        object_id: String,
        /// Bytes saved by this object alone.
        bytes_saved: i64,
    },
    /// A `FlateDecode`-compressed raw image was converted to JPEG.
    FlateToJpeg {
        /// The PDF object ID, formatted for display.
        object_id: String,
        /// Bytes saved by this object alone.
        bytes_saved: i64,
    },
    /// An `LZWDecode`-compressed raw image was converted to JPEG.
    LzwToJpeg {
        /// The PDF object ID, formatted for display.
        object_id: String,
        /// Bytes saved by this object alone.
        bytes_saved: i64,
    },
    /// A non-image content stream that had no compression filter was
    /// Deflate-compressed.
    StreamDeflated {
        /// The PDF object ID, formatted for display.
        object_id: String,
        /// Bytes saved by this object alone.
        bytes_saved: i64,
    },
}

/// Outcome of compressing a single PDF.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// Number of JPEG images re-encoded (see [`CompressEvent::JpegRecompressed`]).
    pub jpeg_compressed: usize,
    /// Number of `FlateDecode` images converted to JPEG (see [`CompressEvent::FlateToJpeg`]).
    pub flate_converted: usize,
    /// Number of `LZWDecode` images converted to JPEG (see [`CompressEvent::LzwToJpeg`]).
    pub lzw_converted: usize,
    /// Number of non-image streams Deflate-compressed (see [`CompressEvent::StreamDeflated`]).
    pub streams_compressed: usize,
    /// Size of the input file, in bytes.
    pub input_bytes: u64,
    /// Size of the output file, in bytes. Equal to `input_bytes` when
    /// `kept_original` is `true`.
    pub output_bytes: u64,
    /// `true` if the lopdf-rewritten output ended up *larger* than
    /// the original — this can happen on image-light, structure-heavy
    /// documents, since lopdf's re-serialization doesn't always
    /// preserve the original generator's compact object/xref-stream
    /// packing. When this happens, the original file is copied to
    /// `output` unchanged instead, so compression can never make a
    /// file bigger.
    pub kept_original: bool,
    /// What document-level metadata (`/Info` fields + XMP size) was
    /// found in the input — populated regardless of whether
    /// `strip_metadata` was set, so callers always know what's there.
    pub metadata_found: crate::metadata::MetadataInfo,
    /// `/Info` dictionary field names actually removed (empty unless
    /// `strip_metadata` was `true`).
    pub metadata_fields_removed: Vec<String>,
    /// Size of the XMP `/Metadata` stream actually removed, if any
    /// (only populated when `strip_metadata` was `true`).
    pub xmp_bytes_removed: Option<u64>,
    /// Empty unless `CompressOpts::emit_events` was `true`.
    pub events: Vec<CompressEvent>,
    /// The path the compressed file was actually written to. Equal
    /// to the `output` argument passed to [`compress`] unless
    /// `opts.on_conflict` was [`OnConflict::Rename`] and that path
    /// was already taken, in which case this is the numbered
    /// alternative that got used instead.
    pub final_output: PathBuf,
    /// `true` if `on_conflict` was [`OnConflict::Rename`] and the
    /// requested output path was already taken, so `final_output`
    /// ended up different from what was asked for.
    pub renamed_to_avoid_conflict: bool,
    /// Mirrors [`CompressOpts::dry_run`] for this run — `true` means
    /// every other field here still reflects genuine work (the same
    /// image/stream compression and metadata inspection a real run
    /// does), but nothing was actually written to disk: `final_output`
    /// is where it *would* have gone, not where it did.
    pub dry_run: bool,
}

impl Report {
    /// Bytes saved (input size minus output size). Can be negative in
    /// rare cases where compression didn't help overall.
    pub fn bytes_saved(&self) -> i64 {
        self.input_bytes as i64 - self.output_bytes as i64
    }

    /// Percentage saved, relative to the input size. `0.0` if the
    /// input was empty.
    pub fn saved_pct(&self) -> f64 {
        if self.input_bytes == 0 {
            return 0.0;
        }
        self.bytes_saved() as f64 / self.input_bytes as f64 * 100.0
    }
}

/// Compresses a single PDF file: re-encodes images using the
/// strategies in `images.rs`, then deflates any remaining
/// uncompressed content streams. Writes the result to `output` — or,
/// if `opts.on_conflict` is [`OnConflict::Rename`] and `output` is
/// already taken, to a numbered alternative next to it (see
/// [`Report::final_output`] for which path actually got used).
///
/// The write itself is atomic: the finished PDF is assembled in a
/// temp file inside `output`'s own directory first, and only moved
/// into place — honouring `opts.on_conflict` — once it's completely
/// done. A crash, Ctrl-C, or a full disk partway through can only
/// ever corrupt that temp file, never `output` itself (or, in an
/// in-place-looking run, never the original — though callers should
/// still check [`paths::same_file`] themselves before calling this,
/// since refusing that case entirely is a clearer error than relying
/// on `on_conflict` to catch it).
///
/// When `opts.dry_run` is `true`, every step above still happens
/// exactly as described — `input` is loaded, images are recompressed,
/// streams are deflated, metadata is inspected (and, if
/// `opts.strip_metadata` is also set, its removal is simulated) — but
/// entirely against an in-memory `doc` and a scratch file that never
/// lives anywhere near `output`. Nothing is created, overwritten, or
/// renamed at `output` or under its parent directories; `output`'s
/// conflict handling itself is only *simulated* (see
/// [`paths::simulate_commit`]), including surfacing the same "already
/// exists" error a real [`OnConflict::Refuse`] run would, so a dry
/// run's success or failure matches what a real run would do.
///
/// # Errors
///
/// Returns an error if `input` can't be read, isn't a valid PDF, is
/// encrypted and `opts.allow_decrypt` is `false` (or decryption
/// fails), if `output` can't be written, or — depending on
/// `opts.on_conflict` — if `output` already exists (see
/// [`paths::commit`] for exactly which policies can fail that way,
/// and when; [`paths::simulate_commit`] fails under the same
/// conditions when `opts.dry_run` is set).
pub fn compress(input: &Path, output: &Path, opts: &CompressOpts) -> Result<Report> {
    let input_bytes = std::fs::metadata(input)?.len();
    let tools = ToolSet::detect();

    let (mut doc, repaired) = crate::loader::load_with_repair(input, &tools, opts.allow_decrypt)?;
    let ids: Vec<_> = doc.objects.keys().cloned().collect();

    let mut report = Report {
        input_bytes,
        dry_run: opts.dry_run,
        ..Default::default()
    };

    for id in &ids {
        let Some(Object::Stream(stream)) = doc.objects.get(id).cloned() else {
            continue;
        };

        if images::is_jpeg_image(&stream) {
            // Lossless mode never re-encodes pixel data — it only
            // strips embedded metadata (EXIF/ICC/comments), and only
            // if the caller opted into strip_metadata. Lossless without
            // strip_metadata means there's genuinely nothing left to
            // do to this image.
            let outcome = if opts.lossless {
                if opts.strip_metadata {
                    images::strip_jpeg_metadata(&stream, &tools)
                } else {
                    None
                }
            } else {
                images::compress_jpeg(&stream, opts.quality, &tools, opts.strip_metadata)
            };

            if let Some((s, saved)) = outcome {
                doc.objects.insert(*id, Object::Stream(s));
                report.jpeg_compressed += 1;
                if opts.emit_events {
                    report.events.push(CompressEvent::JpegRecompressed {
                        object_id: format!("{id:?}"),
                        bytes_saved: saved,
                    });
                }
            }
            continue;
        }

        if images::is_flate_image(&stream) {
            // Converting raw pixels to JPEG is inherently lossy
            // (it's a different, lossy format), so this strategy is
            // skipped entirely in lossless mode regardless of quality.
            if !opts.lossless {
                if let Some((s, saved)) = images::compress_flate_to_jpeg(
                    &stream,
                    opts.quality,
                    &tools,
                    opts.strip_metadata,
                ) {
                    doc.objects.insert(*id, Object::Stream(s));
                    report.flate_converted += 1;
                    if opts.emit_events {
                        report.events.push(CompressEvent::FlateToJpeg {
                            object_id: format!("{id:?}"),
                            bytes_saved: saved,
                        });
                    }
                }
            }
            continue;
        }

        if images::is_lzw_image(&stream) {
            // Same reasoning as the FlateDecode case above.
            if !opts.lossless {
                if let Some((s, saved)) =
                    images::compress_lzw_to_jpeg(&stream, opts.quality, &tools, opts.strip_metadata)
                {
                    doc.objects.insert(*id, Object::Stream(s));
                    report.lzw_converted += 1;
                    if opts.emit_events {
                        report.events.push(CompressEvent::LzwToJpeg {
                            object_id: format!("{id:?}"),
                            bytes_saved: saved,
                        });
                    }
                }
            }
            continue;
        }

        if streams::is_uncompressed(&stream) {
            if let Some((s, saved)) = streams::compress_stream(&stream) {
                doc.objects.insert(*id, Object::Stream(s));
                report.streams_compressed += 1;
                if opts.emit_events {
                    report.events.push(CompressEvent::StreamDeflated {
                        object_id: format!("{id:?}"),
                        bytes_saved: saved,
                    });
                }
            }
        }
    }

    // Document-level metadata is independent of image/stream
    // compression — it never affects how the PDF looks. We always
    // *inspect* it (so the report is honest about what's there even
    // when nothing was removed), but only *remove* it when explicitly
    // opted into via `strip_metadata`.
    report.metadata_found = crate::metadata::inspect(&doc);
    if opts.strip_metadata {
        let removed = crate::metadata::strip(&mut doc);
        report.metadata_fields_removed = removed.info_fields;
        report.xmp_bytes_removed = removed.xmp_bytes;
    }

    // Everything above only ever touches an in-memory `doc` — nothing
    // has been written to disk yet. From here on, write to a scratch
    // file first, never to `output` itself.
    //
    // A real run writes that scratch file inside `output`'s own
    // directory: that's what makes the final `paths::commit` call
    // below atomic (a same-filesystem rename) instead of a copy, so a
    // crash or a full disk partway through `doc.save` can only ever
    // corrupt this temp file, never anything a caller could mistake
    // for a finished result. A dry run still needs *somewhere* to
    // save `doc` to in order to measure the honest rewritten size
    // below — the whole point of a dry run is that its numbers are
    // real, not guessed — but that somewhere must never be `output`'s
    // own directory: creating that directory, or anything in it,
    // is exactly the kind of on-disk side effect a dry run promises
    // not to have. The system temp directory (which always already
    // exists) stands in instead, and the file there is discarded —
    // never moved anywhere — once this function returns.
    let tmp = if opts.dry_run {
        tempfile::Builder::new()
            .prefix(".pdftamp-dryrun-")
            .tempfile()?
    } else {
        let out_dir = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(out_dir)?;
        tempfile::Builder::new()
            .prefix(".pdftamp-tmp-")
            .tempfile_in(out_dir)?
    };

    let save_result = doc.save(tmp.path());
    crate::loader::cleanup(repaired);
    save_result?;
    let rewritten_bytes = std::fs::metadata(tmp.path())?.len();

    if rewritten_bytes > input_bytes {
        // Our rewrite ended up bigger than the original — almost
        // certainly lopdf's re-serialization losing compact
        // object/xref-stream packing on a document where we didn't
        // find much to compress. Never let "compression" make a file
        // bigger: fall back to the original, untouched. On a real
        // run that means copying it into the scratch file so it still
        // goes through the one atomic commit step below either way;
        // a dry run has nothing to copy *into* — the scratch file is
        // about to be discarded regardless — so it just reports the
        // same outcome directly.
        if !opts.dry_run {
            std::fs::copy(input, tmp.path())?;
        }
        report.kept_original = true;
        report.output_bytes = input_bytes;
    } else {
        report.output_bytes = rewritten_bytes;
    }

    // The actual commit — real or simulated — is the one place left
    // that decides `final_output` and whether a conflict got in the
    // way. `tmp` is dropped (and its scratch file deleted) right
    // after this either way: `paths::commit` consumes it by moving it
    // into place on a real run, while a dry run just lets it fall out
    // of scope untouched, since `paths::simulate_commit` never needed
    // it in the first place.
    let committed = if opts.dry_run {
        paths::simulate_commit(output, opts.on_conflict)?
    } else {
        paths::commit(tmp, output, opts.on_conflict)?
    };
    report.final_output = committed.path;
    report.renamed_to_avoid_conflict = committed.renamed;

    Ok(report)
}
