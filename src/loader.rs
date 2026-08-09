// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Shared "load a PDF, with `qpdf`-based recovery" helper, used by
//! both `compress()` and `analyze()`.
//!
//! Two distinct problems are handled here, both via `qpdf`:
//!
//! 1. **Structural defects**: some PDF generators write technically-
//!    invalid structures that lenient tools (`qpdf`, `pikepdf`, most
//!    viewers) silently tolerate, but that `lopdf` parses strictly
//!    and rejects — the most common case found in practice:
//!    cross-reference table entries written as 19 bytes (a single
//!    `\n` line ending) instead of the PDF-spec-mandated 20 bytes.
//!    This is always attempted automatically — it never changes
//!    anything about the document itself, only how we manage to
//!    *read* it.
//!
//! 2. **Encryption**: `lopdf` has no decryption support at all, so an
//!    encrypted PDF's streams would be unreadable garbage to it. Many
//!    "encrypted" PDFs in the wild have an *empty* user password —
//!    they only restrict printing/copying via an owner password, and
//!    open with no password prompt in any normal viewer. `qpdf
//!    --decrypt` handles this automatically. Unlike the structural
//!    repair, this is **opt-in only** (`allow_decrypt`): encryption is
//!    a property of the original file the caller didn't necessarily
//!    ask to have removed, even when it's trivially bypassable — we
//!    refuse outright unless explicitly told it's OK to remove it.
//!
//! Error messages here are kept to one short line on purpose — they
//! get displayed inline next to a filename in batch mode (one row per
//! file), so a multi-sentence explanation would wrap badly and bury
//! the result list. Anything needing more detail belongs in `--help`
//! or the README, not here.

use crate::tools::{self, ToolSet};
use anyhow::{anyhow, Context, Result};
use lopdf::Document;
use std::path::{Path, PathBuf};

/// Loads `path`, recovering via `qpdf` when needed:
/// - always retries against a structurally-repaired copy if the
///   direct load fails (this never changes the document, only how we
///   manage to read it)
/// - decrypts the file **only if `allow_decrypt` is `true`** and it
///   turns out to be encrypted (succeeds automatically for empty/no-
///   required-password files; fails clearly if a real password is
///   needed)
///
/// Returns the loaded `Document` plus an optional temp-file path —
/// pass it to [`cleanup`] once you're done.
pub fn load_with_repair(
    path: &Path,
    tools: &ToolSet,
    allow_decrypt: bool,
) -> Result<(Document, Option<PathBuf>)> {
    let (doc, temp) = load_structural(path, tools)?;

    if !is_encrypted(&doc) {
        return Ok((doc, temp));
    }

    if !allow_decrypt {
        cleanup(temp);
        return Err(anyhow!(
            "Encrypted PDF — pass --allow-decrypt to attempt automatic removal"
        ));
    }

    // Opted in — try the automatic (empty-password) decrypt path.
    if !tools.qpdf {
        cleanup(temp);
        return Err(anyhow!(
            "Encrypted PDF — qpdf not installed, can't attempt removal"
        ));
    }

    let decrypted = match tools::qpdf_decrypt(path) {
        Some(p) => p,
        None => {
            cleanup(temp);
            return Err(anyhow!(
                "Encrypted PDF — needs a real password, couldn't remove automatically"
            ));
        }
    };

    let doc2 = match Document::load(&decrypted) {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&decrypted);
            cleanup(temp);
            return Err(e).context("Decrypted, but still unreadable — likely corrupt");
        }
    };

    cleanup(temp);
    Ok((doc2, Some(decrypted)))
}

/// Loads `path`, retrying via a `qpdf` structural-repair pass if the
/// direct load fails. Doesn't know or care about encryption — that's
/// handled separately by the caller, [`load_with_repair`]. Always
/// attempted (no opt-in needed): fixing a malformed-but-not-corrupt
/// file structure never changes the document's actual content.
fn load_structural(path: &Path, tools: &ToolSet) -> Result<(Document, Option<PathBuf>)> {
    match Document::load(path) {
        Ok(doc) => Ok((doc, None)),

        Err(direct_err) => {
            if !tools.qpdf {
                return Err(direct_err)
                    .context("Couldn't read this PDF — install qpdf for a better chance");
            }

            let Some(repaired) = tools::qpdf_repair(path) else {
                return Err(direct_err)
                    .context("Couldn't read this PDF (repair attempt failed too)");
            };

            match Document::load(&repaired) {
                Ok(doc) => Ok((doc, Some(repaired))),
                Err(repaired_err) => {
                    let _ = std::fs::remove_file(&repaired);
                    Err(repaired_err).context("Couldn't read this PDF, likely corrupt")
                }
            }
        }
    }
}

/// Removes the temp file created by [`load_with_repair`], if any.
pub fn cleanup(temp: Option<PathBuf>) {
    if let Some(p) = temp {
        let _ = std::fs::remove_file(p);
    }
}

/// `true` if `doc`'s trailer has an `/Encrypt` entry, meaning the PDF
/// is (at least nominally) encrypted — regardless of whether a real
/// password is actually required to access it.
fn is_encrypted(doc: &Document) -> bool {
    doc.trailer.get(b"Encrypt").is_ok()
}
