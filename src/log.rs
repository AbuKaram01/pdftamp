// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Optional plain-text run log (`--log-file`, available on both
//! `compress` and `compress-dir`).
//!
//! Appends one record per run to a file — the same information
//! `render.rs` prints to the terminal, minus the ANSI colour codes,
//! so it stays readable in a plain text editor, `tail -f`, or a CI
//! log viewer, and survives after the terminal itself is closed.
//! Always appends, never truncates: each run adds a new timestamped
//! block, building up a running history instead of overwriting the
//! last one.
//!
//! Writing to the log is best-effort from the caller's point of
//! view: `main.rs` reports a failure here as a warning rather than
//! aborting the run over it — the compression work the person
//! actually asked for already happened (or didn't, for its own
//! reasons) by the time logging runs, and a log a person didn't
//! strictly need succeeding shouldn't be able to make a real
//! compression failure worse, or a real success look like a failure.
//!
//! That "best-effort" rule only covers the *real* write, though —
//! the one that happens after compression already ran. Before any of
//! that starts, `main.rs` calls [`validate_log_path`] once, up front,
//! precisely so a broken `--log-file` (most commonly: a directory
//! handed in where a file path was expected) gets caught immediately
//! instead of only surfacing as a warning after an entire — possibly
//! long — batch job has already finished. See that function's own
//! doc comment for the reasoning in full.

use crate::batch::BatchReport;
use crate::compress::{CompressOpts, Report};
use anyhow::{Context, Result};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const RULE: &str =
    "--------------------------------------------------------------------------------";
const DRULE: &str =
    "================================================================================";

/// `YYYY-MM-DD HH:MM:SS UTC`, computed from [`SystemTime`] alone.
/// Nothing else in this crate needs a date/time library — see
/// `metadata.rs`, which reads and strips PDF date fields as opaque
/// strings rather than parsing them — so a small hand-rolled
/// calendar conversion here beats adding a dependency just for a log
/// timestamp.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_timestamp(secs)
}

/// The actual `secs`-since-epoch → calendar conversion, split out
/// from [`timestamp`] purely so it can be unit-tested against known
/// reference values instead of only ever seeing whatever
/// `SystemTime::now()` happens to return.
fn format_timestamp(secs: u64) -> String {
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );

    // Howard Hinnant's `civil_from_days` (public domain): converts a
    // day count since the 1970-01-01 epoch into a proleptic
    // Gregorian year/month/day.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {hour:02}:{minute:02}:{second:02} UTC")
}

/// Appends `body` to `path`, creating it (and nothing else — the
/// parent directory must already exist) if it doesn't exist yet.
fn append(path: &Path, body: &str) -> Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("couldn't open log file '{}'", path.display()))?;
    f.write_all(body.as_bytes())
        .with_context(|| format!("couldn't write to log file '{}'", path.display()))
}

/// Confirms `--log-file`'s value is actually usable, *before* any
/// compression work starts — the exact same open-for-append-and-create
/// check [`append`] does internally, just run empty-handed and up
/// front instead of waiting until there's a real record to write.
///
/// Call this once, right after parsing arguments, for both `compress`
/// and `compress-dir`. The reason this exists as its own step instead
/// of just letting the eventual real [`write_compress`]/[`write_batch`]
/// call surface the problem: for `compress-dir` in particular, that
/// call only happens *after* the entire batch has already run — every
/// file in the tree gets compressed (or attempted) first, and only
/// then does the person learn their `--log-file` was actually a
/// directory, a typo'd path, or somewhere they don't have permission
/// to write. On a large library, that can mean minutes of real,
/// resource-intensive work happening before a preventable mistake
/// like handing it a directory instead of a file gets caught — a
/// mistake this check catches instantly, before any of that work
/// begins.
///
/// This is a genuine exception to the "logging failures are
/// best-effort, never fatal" rule described at the top of this module
/// — deliberately so: a bad path caught here has cost nothing yet, so
/// there's nothing to protect by pressing on regardless. That rule is
/// about *not* letting a logging failure retroactively spoil or block
/// compression work that already happened (or is already underway);
/// it was never meant to justify running an entire batch job first
/// just to discover its `--log-file` never had a chance of working.
///
/// # Errors
///
/// Returns an error if `path` can't be opened for appending — for
/// instance, if it's an existing directory, its parent directory
/// doesn't exist, or the process lacks permission to write there.
pub fn validate_log_path(path: &Path) -> Result<()> {
    append(path, "")
}

/// Appends one `pdftamp compress` run's record: the same header
/// `render::print_compress_header` shows, followed by the outcome —
/// sizes and percentage saved on success, the full (untruncated,
/// unlike the terminal's live view) error message on failure.
pub fn write_compress(
    path: &Path,
    input: &Path,
    output: &Path,
    profile_name: &str,
    opts: &CompressOpts,
    result: &Result<Report>,
) -> Result<()> {
    use std::fmt::Write as _;
    let mut body = String::new();
    let _ = writeln!(body, "{DRULE}");
    let _ = writeln!(
        body,
        "{}  pdftamp compress{}",
        timestamp(),
        if opts.dry_run { " (dry run)" } else { "" }
    );
    let _ = writeln!(body, "  Input      : {input:?}");
    let _ = writeln!(body, "  Output     : {output:?}");
    let _ = writeln!(
        body,
        "  Profile    : {}",
        crate::render::profile_line(profile_name, opts)
    );
    let _ = writeln!(body, "  If exists  : {}", opts.on_conflict.describe());
    let _ = writeln!(body, "{RULE}");
    match result {
        Ok(r) => {
            let _ = writeln!(
                body,
                "  OK  {:.2} MB -> {:.2} MB ({:.1}% saved){}{}",
                crate::render::mb(r.input_bytes),
                crate::render::mb(r.output_bytes),
                r.saved_pct(),
                if r.kept_original {
                    " [kept original — recompressing it would have made it bigger]"
                } else {
                    ""
                },
                if r.dry_run {
                    " [dry run — nothing was actually written]"
                } else {
                    ""
                },
            );
            if r.renamed_to_avoid_conflict {
                let _ = writeln!(
                    body,
                    "  {}: {:?} (requested name was taken)",
                    if r.dry_run {
                        "Would save as"
                    } else {
                        "Saved as"
                    },
                    r.final_output
                );
            }
        }
        Err(e) => {
            let _ = writeln!(body, "  FAILED  {e}");
        }
    }
    let _ = writeln!(body, "{DRULE}\n");
    append(path, &body)
}

/// Appends one `pdftamp compress-dir` run's record: the same header
/// `render::print_batch_header` shows, one line per file (full,
/// untruncated name and — for failures — the full error message,
/// since a log file has no terminal-width limit to respect), then
/// the final totals.
pub fn write_batch(
    path: &Path,
    input_dir: &Path,
    dest_description: &str,
    profile_name: &str,
    opts: &CompressOpts,
    batch: &BatchReport,
) -> Result<()> {
    use std::fmt::Write as _;
    let mut body = String::new();
    let _ = writeln!(body, "{DRULE}");
    let _ = writeln!(
        body,
        "{}  pdftamp compress-dir{}",
        timestamp(),
        if opts.dry_run { " (dry run)" } else { "" }
    );
    let _ = writeln!(body, "  Input dir  : {input_dir:?}");
    let _ = writeln!(body, "  Output     : {dest_description}");
    let _ = writeln!(
        body,
        "  Profile    : {}",
        crate::render::profile_line(profile_name, opts)
    );
    let _ = writeln!(body, "  If exists  : {}", opts.on_conflict.describe());
    let _ = writeln!(body, "{RULE}");
    for item in &batch.items {
        match &item.result {
            Ok(r) => {
                let _ = writeln!(
                    body,
                    "  OK    {:?}  {:.2} MB -> {:.2} MB ({:.1}%){}",
                    item.input,
                    crate::render::mb(r.input_bytes),
                    crate::render::mb(r.output_bytes),
                    r.saved_pct(),
                    if r.dry_run { " [dry run]" } else { "" },
                );
            }
            Err(e) => {
                let _ = writeln!(body, "  FAIL  {:?}  {e}", item.input);
            }
        }
    }
    for skipped in &batch.skipped_own_output {
        let _ = writeln!(
            body,
            "  SKIP  {skipped:?}  looked like previous pdftamp output"
        );
    }
    let _ = writeln!(body, "{RULE}");
    let _ = writeln!(
        body,
        "  {} succeeded, {} failed — {} {:.2} MB total{}",
        batch.succeeded_count(),
        batch.failed_count(),
        if opts.dry_run { "would save" } else { "saved" },
        crate::render::mb(batch.total_bytes_saved().max(0) as u64),
        if opts.dry_run {
            " [dry run — nothing was actually written]"
        } else {
            ""
        },
    );
    let _ = writeln!(body, "{DRULE}\n");
    append(path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_timestamp_epoch_zero() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00 UTC");
    }

    #[test]
    fn format_timestamp_last_second_of_epoch_day() {
        assert_eq!(format_timestamp(86_399), "1970-01-01 23:59:59 UTC");
    }

    #[test]
    fn format_timestamp_y2k() {
        assert_eq!(format_timestamp(946_684_800), "2000-01-01 00:00:00 UTC");
    }

    #[test]
    fn format_timestamp_arbitrary_known_value() {
        // Cross-checked against Python's datetime.utcfromtimestamp.
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14 22:13:20 UTC");
    }

    #[test]
    fn format_timestamp_future_new_year() {
        assert_eq!(format_timestamp(1_893_456_000), "2030-01-01 00:00:00 UTC");
    }

    fn test_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("pdftamp_log_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn append_creates_file_and_adds_to_it_across_calls() {
        let dir = test_dir("append_creates");
        let path = dir.join("run.log");

        append(&path, "first\n").unwrap();
        append(&path, "second\n").unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first\nsecond\n");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_log_path_succeeds_and_leaves_an_empty_file_for_a_fresh_valid_path() {
        let dir = test_dir("validate_ok");
        let path = dir.join("run.log");

        validate_log_path(&path).expect("a fresh path next to an existing dir should validate");
        assert!(path.is_file());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_log_path_rejects_a_directory_with_a_clear_reason() {
        // The exact mistake from the field report this check exists
        // for: passing a directory (e.g. `--log-file ~/Downloads`)
        // instead of a file path.
        let dir = test_dir("validate_rejects_dir");

        let err = validate_log_path(&dir).expect_err("a directory should never validate");
        let full_message: String = err
            .chain()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(": ");
        assert!(full_message.contains("couldn't open log file"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_log_path_rejects_a_path_whose_parent_directory_does_not_exist() {
        let dir = test_dir("validate_rejects_missing_parent");
        let path = dir.join("nonexistent_subdir").join("run.log");

        assert!(validate_log_path(&path).is_err());
        assert!(!path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
