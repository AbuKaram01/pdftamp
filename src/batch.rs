// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Batch compression — recursively compresses every PDF found under a
//! directory tree.
//!
//! Where each file's compressed copy goes is a [`DestStrategy`]: next
//! to its own original by default, or mirrored under a separate
//! output directory when one is given explicitly. Either way, an
//! existing file at the destination is handled per
//! [`crate::paths::OnConflict`] — refused, overwritten, or renamed —
//! never silently clobbered. See [`compress_directory`] for how those
//! pieces fit together, including the guard against a directory's own
//! previous output getting walked and reprocessed.

use crate::compress::{compress, CompressOpts, Report};
use anyhow::Result;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Outcome of compressing one file within a batch run.
#[derive(Debug)]
pub struct BatchItem {
    /// Path to the source file, as found while walking the input
    /// directory.
    pub input: PathBuf,
    /// Path the compressed file was (or would have been) written to.
    /// Empty when `result` is the internal-error variant described
    /// on [`compress_directory`]'s "safe path" note.
    pub output: PathBuf,
    /// `Err` holds a display-friendly message rather than `anyhow::Error`,
    /// so `BatchItem` stays simple to work with (a plain `Result`, no
    /// trait objects to juggle).
    pub result: Result<Report, String>,
}

impl BatchItem {
    /// Whether this file compressed successfully.
    pub fn succeeded(&self) -> bool {
        self.result.is_ok()
    }
}

/// Aggregated results of a [`compress_directory`] run.
#[derive(Debug, Default)]
pub struct BatchReport {
    /// One entry per file that was found and processed, in the order
    /// [`compress_directory`] encountered them.
    pub items: Vec<BatchItem>,
    /// Files the walk found but never even attempted to compress,
    /// because their name already looked like a previous run's own
    /// output — see [`looks_like_own_output`]. Empty under
    /// [`DestStrategy::Mirror`], where this doesn't apply.
    pub skipped_own_output: Vec<PathBuf>,
}

impl BatchReport {
    /// Number of files that compressed successfully.
    pub fn succeeded_count(&self) -> usize {
        self.items.iter().filter(|i| i.succeeded()).count()
    }

    /// Number of files that failed to compress.
    pub fn failed_count(&self) -> usize {
        self.items.len() - self.succeeded_count()
    }

    /// Sum of input sizes across successfully-compressed files only.
    /// Failed items don't have a `Report` to read sizes from.
    pub fn total_input_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter_map(|i| i.result.as_ref().ok())
            .map(|r| r.input_bytes)
            .sum()
    }

    /// Sum of output sizes across successfully-compressed files only.
    pub fn total_output_bytes(&self) -> u64 {
        self.items
            .iter()
            .filter_map(|i| i.result.as_ref().ok())
            .map(|r| r.output_bytes)
            .sum()
    }

    /// Total bytes saved across the batch (`total_input_bytes -
    /// total_output_bytes`). Signed because, in principle, a batch
    /// where every file already happened to be at (or near) its
    /// minimum size could come out non-positive.
    pub fn total_bytes_saved(&self) -> i64 {
        self.total_input_bytes() as i64 - self.total_output_bytes() as i64
    }
}

/// True if `path`'s file name already looks like something
/// `compress_directory` itself would have produced under
/// [`DestStrategy::NextToOriginal`] — `name-compressed.ext`, or a
/// `--if-exists=rename` numbered variant of it
/// (`name-compressed (2).ext`).
///
/// Used to skip re-walking a directory's own previous output on a
/// second run. Without this, running `compress-dir` again over a
/// folder that already has last time's `*-compressed.pdf` files in
/// it would pick those up as fresh input too — wasted work (they're
/// already about as small as this tool can make them), and it'd keep
/// compounding on every further run: `report-compressed.pdf` →
/// `report-compressed-compressed.pdf` → `...-compressed-compressed-
/// compressed.pdf`, forever.
///
/// This is a filename convention, not a guarantee: a genuinely
/// different, unrelated file that happens to already end in
/// "-compressed" gets skipped too. That trade-off is deliberate. The
/// cost is small — target that one file directly with
/// `pdftamp compress` instead, which is never subject to this check,
/// it only affects `compress-dir`'s own automatic directory walk.
/// The alternative — stamping a hidden marker into every output
/// file's own metadata just for pdftamp's internal bookkeeping —
/// would be a heavier, more invasive fix for a low-stakes annoyance:
/// this tool already goes out of its way not to touch anything in a
/// PDF the caller didn't explicitly ask it to (see `strip_metadata`
/// and `allow_decrypt` in [`CompressOpts`], both opt-in-only for
/// exactly that reason).
pub fn looks_like_own_output(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    // Strip one trailing " (N)" — an --if-exists=rename artifact —
    // before checking, so "report-compressed (2).pdf" is caught too.
    let stem = stem
        .rsplit_once(" (")
        .filter(|(_, rest)| {
            rest.strip_suffix(')')
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
        .map(|(base, _)| base)
        .unwrap_or(stem);
    stem.ends_with("-compressed")
}

/// How [`compress_directory`] decides where each file's compressed
/// copy goes.
#[derive(Debug)]
pub enum DestStrategy<'a> {
    /// No explicit output directory was given: each file is
    /// compressed right next to its own original, exactly like the
    /// single-file `compress` command's default
    /// (`report.pdf` → `report-compressed.pdf`, same folder). This
    /// is the default for `compress-dir` too now — a run with no
    /// extra arguments doesn't scatter output into a second,
    /// separately-structured tree the user then has to reconcile
    /// back into their library by hand.
    NextToOriginal,
    /// An explicit output directory was given: mirror `input_dir`'s
    /// structure under it instead, e.g. `books/rust/intro.pdf` →
    /// `<dir>/rust/intro.pdf`. Opt-in, for when a fully separate
    /// compressed copy of the whole tree is actually what's wanted.
    Mirror(&'a Path),
}

/// Recursively finds every `.pdf` file under `input_dir` (including
/// nested subdirectories) and compresses each one, per `dest`.
///
/// The full file list is collected *before* any compression starts,
/// rather than compressing files one at a time as `WalkDir` yields
/// them. Two reasons:
///
/// - Under [`DestStrategy::NextToOriginal`], every compressed file
///   lands inside the very tree still being walked. If we wrote
///   while still iterating, a file this run just produced could get
///   picked up and reprocessed later in the same run — depending on
///   how the filesystem happens to order directory reads, possibly
///   even cascading (`report-compressed.pdf` → `report-compressed-
///   compressed.pdf` → ...). Finalizing the list up front makes that
///   impossible: nothing written during the run can ever appear in
///   a list that was already closed before the run started.
/// - It also means the "N files" total is known and stable from the
///   very first line of output, instead of only becoming clear once
///   the whole batch has finished.
///
/// Under [`DestStrategy::NextToOriginal`], the same list is also
/// filtered through [`looks_like_own_output`] — see its doc comment
/// for why a *previous* run's leftover `*-compressed.pdf` files need
/// a separate guard from the same-run case above. Skipped files are
/// reported via [`BatchReport::skipped_own_output`], not silently
/// dropped.
///
/// A failure on one file (corrupt PDF, permission error, output
/// already exists under [`crate::paths::OnConflict::Refuse`], ...)
/// is recorded in that file's [`BatchItem`] rather than aborting the
/// whole batch — one bad file shouldn't block compressing the rest
/// of the library.
///
/// When `opts.dry_run` is set, each file still goes through the full
/// per-file `compress()` pipeline — see its own doc comment — so
/// every [`BatchItem`] carries genuine results, but under
/// [`DestStrategy::Mirror`] this function itself also skips creating
/// the mirrored subdirectories it would otherwise make room for each
/// file's (real) output in; a dry run has no output to make room for.
///
/// `on_item` is called immediately after each file finishes, so
/// results can be streamed to the user as they happen instead of
/// only appearing once the entire batch completes. Pass `|_| {}` if
/// you only care about the final [`BatchReport`].
///
/// # Errors
///
/// Returns an error only for a failure that affects the batch as a
/// whole — currently, only if an output subdirectory can't be
/// created under [`DestStrategy::Mirror`]. Per-file compression
/// failures never propagate here; they land in that file's
/// [`BatchItem::result`] instead.
pub fn compress_directory(
    input_dir: &Path,
    dest: DestStrategy,
    opts: &CompressOpts,
    mut on_item: impl FnMut(&BatchItem),
) -> Result<BatchReport> {
    let mut report = BatchReport::default();
    let skip_own_output = matches!(dest, DestStrategy::NextToOriginal);

    let files: Vec<PathBuf> = WalkDir::new(input_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .filter(|p| p.is_file() && is_pdf(p))
        .filter(|p| {
            if skip_own_output && looks_like_own_output(p) {
                report.skipped_own_output.push(p.clone());
                false
            } else {
                true
            }
        })
        .collect();

    for path in files {
        let dest_path = match dest {
            DestStrategy::NextToOriginal => crate::paths::default_output_path(&path),
            DestStrategy::Mirror(output_dir) => {
                // See `safe_dest_path` for why the failure case is
                // handled the way it is.
                let Some(d) = safe_dest_path(input_dir, output_dir, &path) else {
                    let item = BatchItem {
                        input: path.clone(),
                        output: PathBuf::new(),
                        result: Err(
                            "internal error: couldn't compute a safe output path for this file (skipped rather than risk writing outside the output directory)"
                                .to_string(),
                        ),
                    };
                    on_item(&item);
                    report.items.push(item);
                    continue;
                };
                // A dry run promises not to touch the filesystem
                // beyond what compress() itself needs — which, in
                // dry-run mode, is nothing under `output_dir` at all
                // (see `compress()`'s own doc comment). Creating this
                // subdirectory ahead of a file that's never actually
                // going to be written there would break that promise.
                if !opts.dry_run {
                    if let Some(parent) = d.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
                d
            }
        };

        let result = compress(&path, &dest_path, opts).map_err(|e| e.to_string());
        // On success this is `Report::final_output`, which is
        // `dest_path` unless `OnConflict::Rename` had to fall back to
        // a numbered alternative; on failure, `dest_path` is still
        // the most useful thing to show (what we *would* have
        // written to).
        let output = result
            .as_ref()
            .map(|r: &Report| r.final_output.clone())
            .unwrap_or_else(|_| dest_path.clone());

        let item = BatchItem {
            input: path,
            output,
            result,
        };
        on_item(&item);
        report.items.push(item);
    }

    Ok(report)
}

/// Whether `path`'s extension is `.pdf`, case-insensitively.
fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}

/// Computes where `path` (a file found while walking `input_dir`)
/// should be written under `output_dir`, preserving its position in
/// the directory tree.
///
/// Returns `None` if `path` doesn't actually live under `input_dir`.
/// That should never happen given `path` comes from walking
/// `input_dir` itself, but if it ever did, the alternative — joining
/// `output_dir` with whatever `path` turned out to be — would be
/// dangerous: `Path::join` *replaces* its base entirely when given an
/// absolute argument, so an unexpected absolute `path` would silently
/// write outside `output_dir` rather than inside it. Returning `None`
/// lets the caller skip the file instead of risking that.
fn safe_dest_path(input_dir: &Path, output_dir: &Path, path: &Path) -> Option<PathBuf> {
    let relative = path.strip_prefix(input_dir).ok()?;
    Some(output_dir.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_dest_path_mirrors_relative_structure() {
        let dest = safe_dest_path(
            Path::new("/library/books"),
            Path::new("/library/books-compressed"),
            Path::new("/library/books/rust/intro.pdf"),
        );
        assert_eq!(
            dest,
            Some(PathBuf::from("/library/books-compressed/rust/intro.pdf"))
        );
    }

    #[test]
    fn safe_dest_path_returns_none_when_path_is_not_under_input_dir() {
        // The defensive case: if `path` somehow isn't a descendant of
        // `input_dir`, we must not silently fall back to joining it
        // onto `output_dir` (see doc comment on `safe_dest_path` for
        // why that would be unsafe for an absolute `path`).
        let dest = safe_dest_path(
            Path::new("/library/books"),
            Path::new("/library/books-compressed"),
            Path::new("/etc/cron.d/evil"),
        );
        assert_eq!(dest, None);
    }

    #[test]
    fn safe_dest_path_never_escapes_output_dir_even_for_absolute_stray_path() {
        // Directly documents the exact footgun this guards against:
        // naively doing `output_dir.join(path)` for an absolute
        // `path` that failed to strip_prefix would replace
        // output_dir outright. Confirm our helper never does that.
        let escaped_dir = Path::new("/library/books-compressed");
        let stray_absolute_path = Path::new("/etc/cron.d/evil");

        // The footgun this test guards against, demonstrated directly:
        assert_eq!(escaped_dir.join(stray_absolute_path), stray_absolute_path);

        // Our helper must return None instead of reproducing it.
        assert_eq!(
            safe_dest_path(
                Path::new("/library/books"),
                escaped_dir,
                stray_absolute_path
            ),
            None
        );
    }

    #[test]
    fn looks_like_own_output_matches_the_plain_suffix() {
        assert!(looks_like_own_output(Path::new(
            "/lib/report-compressed.pdf"
        )));
    }

    #[test]
    fn looks_like_own_output_matches_a_rename_numbered_variant() {
        assert!(looks_like_own_output(Path::new(
            "/lib/report-compressed (2).pdf"
        )));
    }

    #[test]
    fn looks_like_own_output_false_for_an_ordinary_file() {
        assert!(!looks_like_own_output(Path::new("/lib/report.pdf")));
        // A parenthesized non-number shouldn't be mistaken for a
        // rename artifact.
        assert!(!looks_like_own_output(Path::new(
            "/lib/report (final draft).pdf"
        )));
    }
}
