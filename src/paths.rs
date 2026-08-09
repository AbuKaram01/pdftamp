// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Default output path conventions, and the one place in the crate
//! that's allowed to decide whether an existing file gets
//! overwritten (see [`commit`]).

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Default output path for a single file: same directory as the
/// input, same extension, with `-compressed` appended to the stem.
///
/// `report.pdf` → `report-compressed.pdf` (next to the original).
pub fn default_output_path(input: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("pdf");
    let filename = format!("{stem}-compressed.{ext}");

    match input.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join(filename),
        None => PathBuf::from(filename),
    }
}

/// What to do when the path a file *would* be written to already
/// exists. Every write in the whole crate funnels through [`commit`],
/// which is the only place this decision actually gets made — so
/// whichever policy is picked here is enforced consistently for a
/// single `compress` run and for every file in a `compress-dir`
/// batch alike.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnConflict {
    // Doc comments on these variants double as their `--help`
    // possible-value descriptions (clap shows one line per variant,
    // indented under "Possible values:") — kept short for the same
    // reason the flag docs above it are, and don't repeat "default"
    // since clap already prints `[default: refuse]` right above that
    // list on its own line. Fuller reasoning: pdftamp's job is to
    // shrink a PDF, not to make a judgment call about which of two
    // same-named files you meant to keep, so the default never
    // silently destroys one.
    /// Skip with an error — never overwrites or renames.
    #[default]
    Refuse,
    /// Replace the existing file.
    Overwrite,
    /// Save alongside it as `name (1).pdf`, `(2)`, and so on.
    Rename,
}

impl OnConflict {
    /// Short, human-readable description for the pre-flight header
    /// ([`crate::render::print_compress_header`] /
    /// [`crate::render::print_batch_header`]), so the policy in
    /// effect is always visible up front rather than only showing up
    /// as a surprise later if a name happens to collide.
    pub fn describe(&self) -> &'static str {
        match self {
            OnConflict::Refuse => "refuse (skip) if the output already exists",
            OnConflict::Overwrite => "overwrite if the output already exists",
            OnConflict::Rename => "save under a new name if the output already exists",
        }
    }

    /// The `--if-exists` values *other* than this one, for the "how
    /// to change this" hint printed alongside [`describe`](Self::describe)
    /// — so that hint only has to be stated once, in the header,
    /// instead of repeated on every colliding line of a `compress-dir`
    /// batch (see [`commit`]'s doc comment on why its own error
    /// messages deliberately don't carry this advice themselves).
    pub fn other_values(&self) -> &'static str {
        match self {
            OnConflict::Refuse => "overwrite|rename",
            OnConflict::Overwrite => "refuse|rename",
            OnConflict::Rename => "refuse|overwrite",
        }
    }
}

/// Where a file ended up after [`commit`], and whether it had to
/// pick a different name to get there.
#[derive(Debug)]
pub struct Committed {
    /// The path the file was actually written to. Under
    /// [`OnConflict::Rename`] this can differ from the path that was
    /// originally requested.
    pub path: PathBuf,
    /// `true` if [`OnConflict::Rename`] had to fall back to a
    /// numbered name because the plain one was already taken.
    pub renamed: bool,
}

/// Atomically moves a finished temp file into place at `dest`,
/// honouring `policy`.
///
/// This is the *only* function in the whole crate that decides
/// whether an existing file at `dest` gets overwritten — every write
/// path (the single-file `compress` command, and every file in a
/// `compress-dir` batch) funnels through it, so "never overwrite by
/// surprise" only has to be enforced correctly in one place.
///
/// `tmp` must already live in `dest`'s own parent directory — that's
/// what makes the final move atomic rather than a copy: it's always
/// a same-filesystem rename under the hood, so a crash, Ctrl-C, or a
/// full disk partway through compression can never leave a
/// half-written file sitting at `dest`. Either the old file (if any)
/// is still there completely untouched, or the new one is there
/// complete — never something in between, and never a window where
/// `dest` is briefly missing.
///
/// # Errors
///
/// Under [`OnConflict::Refuse`], returns an error — without touching
/// `dest` — if it already exists. Under [`OnConflict::Rename`],
/// returns an error only if 1000 numbered candidates are all already
/// taken (not something a real run should ever hit). Any other I/O
/// failure (permissions, a full disk, ...) is passed through as-is.
pub fn commit(tmp: NamedTempFile, dest: &Path, policy: OnConflict) -> Result<Committed> {
    match policy {
        OnConflict::Overwrite => tmp
            .persist(dest)
            .map(|_| Committed {
                path: dest.to_path_buf(),
                renamed: false,
            })
            .map_err(|e| anyhow!("couldn't write '{}': {}", dest.display(), e.error)),

        OnConflict::Refuse => match tmp.persist_noclobber(dest) {
            Ok(_) => Ok(Committed {
                path: dest.to_path_buf(),
                renamed: false,
            }),
            // Deliberately just the fact, no "pass --if-exists=... to
            // change this" advice baked in here: that's CLI-flag
            // knowledge, which belongs to the presentation layer
            // (the pre-flight header already states the active
            // policy once — see `render::print_compress_header` /
            // `print_batch_header`), not to a low-level path error.
            // Folding it in here also reads fine for a single
            // `compress` run, but repeats verbatim on every colliding
            // line of a `compress-dir` batch — exactly the kind of
            // noise a good CLI avoids.
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(anyhow!("'{}' already exists", dest.display()))
            }
            Err(e) => Err(anyhow!("couldn't write '{}': {}", dest.display(), e.error)),
        },

        OnConflict::Rename => {
            let mut tmp = tmp;
            let mut candidate = dest.to_path_buf();
            let mut n = 0u32;
            loop {
                match tmp.persist_noclobber(&candidate) {
                    Ok(_) => {
                        return Ok(Committed {
                            path: candidate,
                            renamed: n > 0,
                        })
                    }
                    Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => {
                        tmp = e.file;
                        n += 1;
                        if n > 1000 {
                            return Err(anyhow!(
                                "couldn't find a free name for '{}' — even '{}' is taken",
                                dest.display(),
                                numbered_candidate(dest, n - 1).display()
                            ));
                        }
                        candidate = numbered_candidate(dest, n);
                    }
                    Err(e) => {
                        return Err(anyhow!(
                            "couldn't write '{}': {}",
                            candidate.display(),
                            e.error
                        ))
                    }
                }
            }
        }
    }
}

/// Read-only counterpart to [`commit`], used by a `--dry-run` so it
/// can report exactly what a real run *would* do to `dest` —
/// including refusing or erring in precisely the same cases — without
/// creating, overwriting, or renaming anything on disk.
///
/// Mirrors [`commit`]'s three policies, but by checking [`Path::exists`]
/// instead of actually persisting a temp file:
/// - [`OnConflict::Overwrite`]: always resolves to `dest` itself —
///   overwriting never fails just because something is already there,
///   so there's nothing to check.
/// - [`OnConflict::Refuse`]: errors — with the exact same message
///   [`commit`] would produce — if `dest` already exists.
/// - [`OnConflict::Rename`]: probes `dest`, `dest (1)`, `dest (2)`, …
///   with plain existence checks (never creating any of them) until
///   it finds the first free name, giving up after 1000 candidates
///   the same way [`commit`] does.
///
/// Best-effort like the rest of this module's existence checks: a
/// dry run can't account for something that changes on disk between
/// this check and a later real run (another process creating the
/// file a moment later, say) — same inherent limitation any "would
/// this succeed?" preview has, not something specific to pdftamp.
///
/// # Errors
///
/// Same conditions as [`commit`]: a [`OnConflict::Refuse`] collision,
/// or exhausting 1000 [`OnConflict::Rename`] candidates.
pub fn simulate_commit(dest: &Path, policy: OnConflict) -> Result<Committed> {
    match policy {
        OnConflict::Overwrite => Ok(Committed {
            path: dest.to_path_buf(),
            renamed: false,
        }),

        OnConflict::Refuse => {
            if dest.exists() {
                // Same wording as `commit`'s Refuse-collision error —
                // see its doc comment for why no "pass --if-exists=..."
                // advice is folded in here either.
                Err(anyhow!("'{}' already exists", dest.display()))
            } else {
                Ok(Committed {
                    path: dest.to_path_buf(),
                    renamed: false,
                })
            }
        }

        OnConflict::Rename => {
            let mut candidate = dest.to_path_buf();
            let mut n = 0u32;
            loop {
                if !candidate.exists() {
                    return Ok(Committed {
                        path: candidate,
                        renamed: n > 0,
                    });
                }
                n += 1;
                if n > 1000 {
                    return Err(anyhow!(
                        "couldn't find a free name for '{}' — even '{}' is taken",
                        dest.display(),
                        numbered_candidate(dest, n - 1).display()
                    ));
                }
                candidate = numbered_candidate(dest, n);
            }
        }
    }
}

/// `report-compressed.pdf` + `2` → `report-compressed (2).pdf`.
fn numbered_candidate(dest: &Path, n: u32) -> PathBuf {
    let stem = dest
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let filename = match dest.extension().and_then(|s| s.to_str()) {
        Some(ext) => format!("{stem} ({n}).{ext}"),
        None => format!("{stem} ({n})"),
    };
    match dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join(filename),
        None => PathBuf::from(filename),
    }
}

/// Best-effort check for whether two paths point at the *same*
/// underlying file, even when one or both don't exist yet (the usual
/// case for a fresh output path) or go through a symlink.
///
/// Existing paths are canonicalized directly. For a path that
/// doesn't exist yet, its parent is canonicalized instead and the
/// file name re-attached, so e.g. a relative path and an absolute
/// path (or a path that passes through a symlinked directory)
/// pointing at the same not-yet-created file still compare equal.
pub fn same_file(a: &Path, b: &Path) -> bool {
    fn resolve(p: &Path) -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        match (
            p.parent().filter(|p| !p.as_os_str().is_empty()),
            p.file_name(),
        ) {
            (Some(parent), Some(name)) => parent
                .canonicalize()
                .map(|c| c.join(name))
                .unwrap_or_else(|_| p.to_path_buf()),
            _ => p.to_path_buf(),
        }
    }
    resolve(a) == resolve(b)
}

/// True if `candidate` is the same directory as `root`, or nested
/// anywhere inside it. Used to stop a batch job's *explicit* output
/// directory from being placed inside (or equal to) its own input
/// directory, which would otherwise make `compress_directory` walk
/// into files it just wrote — reprocessing its own output, in the
/// worst case indefinitely. (When no explicit output directory is
/// given at all — the default — `compress_directory` sidesteps this
/// a different way; see its own doc comment.)
///
/// Best-effort like `same_file`: falls back to comparing the paths
/// as given when one side doesn't exist yet (a fresh output
/// directory), rather than requiring both to already exist.
pub fn is_same_or_within(root: &Path, candidate: &Path) -> bool {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let candidate = candidate
        .canonicalize()
        .unwrap_or_else(|_| candidate.to_path_buf());
    candidate.starts_with(&root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_path_appends_suffix_next_to_input() {
        let result = default_output_path(Path::new("/home/user/books/report.pdf"));
        assert_eq!(
            result,
            PathBuf::from("/home/user/books/report-compressed.pdf")
        );
    }

    #[test]
    fn default_output_path_handles_bare_filename() {
        // No parent directory at all — stay in the current directory
        // rather than panicking or producing an absolute-looking path.
        let result = default_output_path(Path::new("report.pdf"));
        assert_eq!(result, PathBuf::from("report-compressed.pdf"));
    }

    #[test]
    fn numbered_candidate_inserts_before_extension() {
        let result = numbered_candidate(Path::new("/lib/report-compressed.pdf"), 2);
        assert_eq!(result, PathBuf::from("/lib/report-compressed (2).pdf"));
    }

    #[test]
    fn numbered_candidate_handles_no_extension() {
        let result = numbered_candidate(Path::new("/lib/README"), 1);
        assert_eq!(result, PathBuf::from("/lib/README (1)"));
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pdftamp_paths_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn commit_refuse_errors_and_leaves_existing_file_untouched() {
        let dir = test_dir("commit_refuse");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();

        let mut tmp = NamedTempFile::new_in(&dir).unwrap();
        use std::io::Write;
        tmp.write_all(b"new content").unwrap();

        let err = commit(tmp, &dest, OnConflict::Refuse).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        // The pre-existing file must be completely unchanged.
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_refuse_succeeds_when_nothing_is_there_yet() {
        let dir = test_dir("commit_refuse_ok");
        let dest = dir.join("out.pdf");

        let mut tmp = NamedTempFile::new_in(&dir).unwrap();
        use std::io::Write;
        tmp.write_all(b"new content").unwrap();

        let committed = commit(tmp, &dest, OnConflict::Refuse).unwrap();
        assert_eq!(committed.path, dest);
        assert!(!committed.renamed);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_overwrite_replaces_existing_file() {
        let dir = test_dir("commit_overwrite");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();

        let mut tmp = NamedTempFile::new_in(&dir).unwrap();
        use std::io::Write;
        tmp.write_all(b"new content").unwrap();

        let committed = commit(tmp, &dest, OnConflict::Overwrite).unwrap();
        assert_eq!(committed.path, dest);
        assert_eq!(std::fs::read(&dest).unwrap(), b"new content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_rename_picks_a_numbered_name_on_collision() {
        let dir = test_dir("commit_rename");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();
        std::fs::write(dir.join("out (1).pdf"), b"also taken").unwrap();

        let mut tmp = NamedTempFile::new_in(&dir).unwrap();
        use std::io::Write;
        tmp.write_all(b"new content").unwrap();

        let committed = commit(tmp, &dest, OnConflict::Rename).unwrap();
        assert_eq!(committed.path, dir.join("out (2).pdf"));
        assert!(committed.renamed);
        // Both pre-existing files are untouched.
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(
            std::fs::read(dir.join("out (1).pdf")).unwrap(),
            b"also taken"
        );
        assert_eq!(
            std::fs::read(dir.join("out (2).pdf")).unwrap(),
            b"new content"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn simulate_commit_refuse_errors_without_touching_anything() {
        let dir = test_dir("simulate_refuse");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();

        let err = simulate_commit(&dest, OnConflict::Refuse).unwrap_err();
        assert!(err.to_string().contains("already exists"));
        // Not just "no new file appeared" — the existing one must be
        // byte-for-byte the same, since a dry run never opens it.
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn simulate_commit_refuse_succeeds_when_nothing_is_there_yet() {
        let dir = test_dir("simulate_refuse_ok");
        let dest = dir.join("out.pdf");

        let committed = simulate_commit(&dest, OnConflict::Refuse).unwrap();
        assert_eq!(committed.path, dest);
        assert!(!committed.renamed);
        // A dry run must never create the file it's only reporting on.
        assert!(!dest.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn simulate_commit_overwrite_reports_dest_without_touching_the_existing_file() {
        let dir = test_dir("simulate_overwrite");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();

        let committed = simulate_commit(&dest, OnConflict::Overwrite).unwrap();
        assert_eq!(committed.path, dest);
        assert!(!committed.renamed);
        // Simulated, not real: the pre-existing content must survive.
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn simulate_commit_rename_picks_a_numbered_name_without_creating_it() {
        let dir = test_dir("simulate_rename");
        let dest = dir.join("out.pdf");
        std::fs::write(&dest, b"original").unwrap();
        std::fs::write(dir.join("out (1).pdf"), b"also taken").unwrap();

        let committed = simulate_commit(&dest, OnConflict::Rename).unwrap();
        assert_eq!(committed.path, dir.join("out (2).pdf"));
        assert!(committed.renamed);
        // The winning candidate name is reported, but never created.
        assert!(!dir.join("out (2).pdf").exists());
        // And the two pre-existing files are untouched.
        assert_eq!(std::fs::read(&dest).unwrap(), b"original");
        assert_eq!(
            std::fs::read(dir.join("out (1).pdf")).unwrap(),
            b"also taken"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_file_true_for_identical_existing_path() {
        let dir = std::env::temp_dir().join(format!("pdftamp_paths_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.pdf");
        std::fs::write(&f, b"x").unwrap();
        assert!(same_file(&f, &f));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_file_true_for_not_yet_created_relative_vs_absolute() {
        let dir = std::env::temp_dir().join(format!("pdftamp_paths_test2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let abs = dir.join("out.pdf");
        // Same file, but reached with a "./" prefix instead of the
        // plain absolute path -- should still be recognised as equal.
        let via_dot = dir.join(".").join("out.pdf");
        assert!(same_file(&abs, &via_dot));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn same_file_false_for_different_files() {
        let dir = std::env::temp_dir().join(format!("pdftamp_paths_test3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.pdf");
        let b = dir.join("b.pdf");
        assert!(!same_file(&a, &b));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_same_or_within_detects_nested_and_equal_dirs() {
        let dir = std::env::temp_dir().join(format!("pdftamp_paths_test4_{}", std::process::id()));
        let nested = dir.join("out");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(is_same_or_within(&dir, &dir));
        assert!(is_same_or_within(&dir, &nested));
        assert!(!is_same_or_within(&nested, &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_same_or_within_false_for_sibling_dirs() {
        let base = std::env::temp_dir().join(format!("pdftamp_paths_test5_{}", std::process::id()));
        let a = base.join("books");
        let b = base.join("books-compressed");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        assert!(!is_same_or_within(&a, &b));
        let _ = std::fs::remove_dir_all(&base);
    }
}
