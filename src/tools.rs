// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Detection and invocation of external tools pdftamp can use to
//! get better results than a pure-Rust implementation would, or to
//! recover from PDFs that `lopdf` can't parse on its own.
//!
//! Two distinct groups, both optional — pdftamp works without
//! either, just less effectively:
//!
//! - **Image compressors** (`jpegoptim`, `oxipng`, `pngquant`): widely
//!   used, battle-tested binaries that typically outperform what
//!   [`crate::images`] can do with the pure-Rust `image` crate alone.
//!   When available, we shell out to them; otherwise callers fall
//!   back to the `image` crate.
//! - **`qpdf`**: used by [`crate::loader`] as a repair/decrypt
//!   fallback for PDFs `lopdf` can't load on its own — see
//!   [`qpdf_repair`] and [`qpdf_decrypt`].

use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

/// Process-wide counter used to generate unique temp-file names,
/// avoiding collisions when multiple images are processed concurrently.
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Snapshot of which optional external tools are available on `PATH`.
pub struct ToolSet {
    /// Whether `jpegoptim` was found.
    pub jpegoptim: bool,
    /// Whether `oxipng` was found.
    pub oxipng: bool,
    /// Whether `pngquant` was found.
    pub pngquant: bool,
    /// `qpdf` — used as a repair fallback when `lopdf` fails to load
    /// a malformed-but-not-actually-corrupt PDF (e.g. xref tables
    /// with non-spec-compliant 19-byte entries instead of 20, a
    /// surprisingly common defect from less rigorous PDF generators).
    pub qpdf: bool,
}

impl ToolSet {
    /// Probes `PATH` for each supported tool. Cheap enough to call once
    /// per `compress()` invocation.
    pub fn detect() -> Self {
        Self {
            jpegoptim: is_available("jpegoptim"),
            oxipng: is_available("oxipng"),
            pngquant: is_available("pngquant"),
            qpdf: is_available("qpdf"),
        }
    }

    /// Returns a plain-data status row per known tool — no printing.
    /// `render.rs` renders this however fits (currently a simple CLI
    /// table).
    pub fn statuses(&self) -> Vec<ToolStatus> {
        vec![
            ToolStatus {
                name: "jpegoptim",
                found: self.jpegoptim,
                install_hint: "apt install jpegoptim",
            },
            ToolStatus {
                name: "oxipng",
                found: self.oxipng,
                install_hint: "cargo install oxipng",
            },
            ToolStatus {
                name: "pngquant",
                found: self.pngquant,
                install_hint: "apt install pngquant",
            },
            ToolStatus {
                name: "qpdf",
                found: self.qpdf,
                install_hint: "apt install qpdf",
            },
        ]
    }
}

/// One row of [`ToolSet::statuses`] — whether a tool was found, plus
/// an install hint to show if it wasn't.
#[derive(Debug, Clone, Copy)]
pub struct ToolStatus {
    /// The tool's binary name (e.g. `"jpegoptim"`).
    pub name: &'static str,
    /// Whether it was found on `PATH`.
    pub found: bool,
    /// A short install command to show when `found` is `false`.
    pub install_hint: &'static str,
}

/// Checks whether `cmd` resolves to an executable on `PATH`.
pub fn is_available(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Builds a unique path under the system temp directory, scoped to the
/// current process (so concurrent runs of pdftamp never collide).
fn tmp_path(ext: &str) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("pdfsq_{}_{}.{}", std::process::id(), n, ext))
}

// ════════════════════════════════════════════════════════════════
//  jpegoptim
// ════════════════════════════════════════════════════════════════

/// Re-encodes JPEG bytes at a target quality. `strip_metadata`
/// controls whether embedded EXIF/ICC/comments are removed
/// (`--strip-all`) or explicitly preserved (`--strip-none`) — mirrors
/// jpegoptim's own flag names so the behaviour is unsurprising.
pub fn jpegoptim_lossy(bytes: &[u8], quality: u8, strip_metadata: bool) -> Option<Vec<u8>> {
    run_jpegoptim(
        bytes,
        &[
            format!("--max={}", quality),
            strip_flag(strip_metadata),
            "--overwrite".into(),
            "--quiet".into(),
        ],
    )
}

/// Strips metadata only, without touching the actual pixel data —
/// a free, lossless size reduction. Returns `None` immediately if
/// `strip_metadata` is `false`, since there would be nothing left
/// for this function to do.
pub fn jpegoptim_lossless(bytes: &[u8], strip_metadata: bool) -> Option<Vec<u8>> {
    if !strip_metadata {
        return None;
    }
    run_jpegoptim(
        bytes,
        &["--strip-all".into(), "--overwrite".into(), "--quiet".into()],
    )
}

/// Maps `strip_metadata` to the matching `jpegoptim` flag.
fn strip_flag(strip_metadata: bool) -> String {
    if strip_metadata {
        "--strip-all".into()
    } else {
        "--strip-none".into()
    }
}

/// Writes `bytes` to a temp file, runs `jpegoptim` on it in place, then
/// reads the result back. Returns `None` if the tool fails or doesn't
/// actually shrink the file.
fn run_jpegoptim(bytes: &[u8], args: &[String]) -> Option<Vec<u8>> {
    let path = tmp_path("jpg");
    std::fs::write(&path, bytes).ok()?;

    let ok = Command::new("jpegoptim")
        .args(args)
        .arg(&path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let result = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);

    if !ok {
        return None;
    }
    let r = result?;
    (r.len() < bytes.len()).then_some(r)
}

// ════════════════════════════════════════════════════════════════
//  pngquant + oxipng
// ════════════════════════════════════════════════════════════════

/// Optimizes PNG bytes by chaining `pngquant` (lossy palette reduction)
/// followed by `oxipng` (lossless re-compression). Either step is
/// skipped if its tool isn't available. Returns `None` if neither tool
/// is present, or if the result isn't actually smaller.
///
/// Currently unreferenced: the compression pipeline in
/// [`crate::images`] converts every raw/re-encodable image to JPEG
/// rather than producing PNG output, so there's no call site for a
/// PNG-specific optimizer yet. [`ToolSet::oxipng`]/[`ToolSet::pngquant`]
/// are still detected and shown to the user (see [`ToolSet::statuses`])
/// because they're relevant to a possible future PNG-preserving path
/// (e.g. for images with an `/SMask` alpha channel, which JPEG can't
/// represent) — kept rather than deleted so that path doesn't have to
/// be rebuilt from scratch. `#[allow(dead_code)]` documents that this
/// is a deliberate, known gap, not an oversight.
#[allow(dead_code)]
pub fn png_optimize(bytes: &[u8], quality: u8, tools: &ToolSet) -> Option<Vec<u8>> {
    if !tools.pngquant && !tools.oxipng {
        return None;
    }

    let src = tmp_path("png");
    std::fs::write(&src, bytes).ok()?;
    let mut current = src.clone();

    if tools.pngquant {
        let dst = tmp_path("png");
        let ok = Command::new("pngquant")
            .arg(format!("--quality=0-{}", quality))
            .arg("--output")
            .arg(&dst)
            .arg("--force")
            .arg("--strip")
            .arg("--quiet")
            .arg(&current)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok && dst.exists() {
            if current != src {
                let _ = std::fs::remove_file(&current);
            }
            current = dst;
        }
    }

    if tools.oxipng {
        let dst = tmp_path("png");
        let ok = Command::new("oxipng")
            .args(["-o", "4"])
            .arg("--out")
            .arg(&dst)
            .arg("--strip")
            .arg("all")
            .arg("--quiet")
            .arg(&current)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok && dst.exists() {
            if current != src {
                let _ = std::fs::remove_file(&current);
            }
            current = dst;
        }
    }

    let result = std::fs::read(&current).ok();
    if current != src {
        let _ = std::fs::remove_file(&current);
    }
    let _ = std::fs::remove_file(&src);

    let r = result?;
    (r.len() < bytes.len()).then_some(r)
}

/// Defensively rewrites `path` so it can never be mistaken for a
/// command-line flag by the external tool it's about to be passed to.
///
/// A relative path whose text happens to start with `-` (nothing
/// stops a PDF from being named `-rf.pdf`, whether the user did it on
/// purpose or it just arrived that way from somewhere else) would
/// otherwise be indistinguishable from an option to a tool that
/// parses arguments the traditional Unix way — `qpdf -rf.pdf out.pdf`
/// could be read as flag `-r`, flag `-f`, and one positional argument,
/// instead of two file paths. Prefixing it with `./` keeps it pointing
/// at the exact same file while making that reading impossible. Note
/// this is unrelated to shell injection: every call in this module
/// already uses `Command::arg`, which passes arguments straight to
/// the OS without ever going through a shell, so characters like `;`
/// or `` ` `` in a filename are never interpreted as shell syntax.
fn safe_path_arg(path: &std::path::Path) -> std::path::PathBuf {
    let starts_with_dash = path
        .as_os_str()
        .to_str()
        .map(|s| s.starts_with('-'))
        .unwrap_or(false);

    if starts_with_dash && path.is_relative() {
        std::path::Path::new(".").join(path)
    } else {
        path.to_path_buf()
    }
}

// ════════════════════════════════════════════════════════════════
//  qpdf — repair fallback for malformed-but-not-corrupt PDFs
// ════════════════════════════════════════════════════════════════

/// Attempts to decrypt `input` via `qpdf`, writing the result to a
/// fresh temp path. This succeeds automatically for PDFs with an
/// empty/no-required user password (extremely common for files that
/// only restrict printing/copying via an *owner* password — viewable
/// by anyone, but technically encrypted at the byte level). It fails
/// cleanly if a real password is actually required.
///
/// Returns `None` if `qpdf` isn't available, or if decryption failed
/// (most likely because a real password is needed).
pub fn qpdf_decrypt(input: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = tmp_path("pdf");

    let ok = Command::new("qpdf")
        .arg("--decrypt")
        .arg(safe_path_arg(input))
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok && out.exists() {
        Some(out)
    } else {
        let _ = std::fs::remove_file(&out);
        None
    }
}

/// Rewrites `input` into a spec-compliant PDF at a fresh temp path,
/// using `qpdf`. Some PDF generators write technically-invalid
/// structures (e.g. cross-reference table entries that are 19 bytes
/// instead of the spec-mandated 20) that lenient readers like `qpdf`
/// and `pikepdf` tolerate silently, but that `lopdf` parses strictly
/// and rejects outright.
///
/// Returns `None` if `qpdf` isn't installed, or if it also fails
/// (meaning the file is likely genuinely corrupt, not just
/// non-compliant).
pub fn qpdf_repair(input: &std::path::Path) -> Option<std::path::PathBuf> {
    let out = tmp_path("pdf");

    let ok = Command::new("qpdf")
        .arg(safe_path_arg(input))
        .arg(&out)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok && out.exists() {
        Some(out)
    } else {
        let _ = std::fs::remove_file(&out);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn safe_path_arg_prefixes_relative_dash_names() {
        // A file literally named "-rf.pdf" would otherwise be
        // indistinguishable from a flag to a Unix-style argument
        // parser. `./` keeps it pointing at the same file while
        // making that impossible.
        assert_eq!(
            safe_path_arg(Path::new("-rf.pdf")),
            PathBuf::from("./-rf.pdf")
        );
        assert_eq!(
            safe_path_arg(Path::new("--decrypt")),
            PathBuf::from("./--decrypt")
        );
    }

    #[test]
    fn safe_path_arg_leaves_normal_paths_untouched() {
        assert_eq!(
            safe_path_arg(Path::new("report.pdf")),
            PathBuf::from("report.pdf")
        );
        assert_eq!(
            safe_path_arg(Path::new("books/report.pdf")),
            PathBuf::from("books/report.pdf")
        );
    }

    #[test]
    fn safe_path_arg_leaves_absolute_dash_paths_untouched() {
        // An absolute path can't be mistaken for a flag regardless of
        // what comes after the leading `/`, so no rewrite is needed
        // (and prefixing it with `./` would actually break it).
        assert_eq!(
            safe_path_arg(Path::new("/tmp/-rf.pdf")),
            PathBuf::from("/tmp/-rf.pdf")
        );
    }
}
