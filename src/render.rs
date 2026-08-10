// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! All terminal output lives here — `println!` is banned everywhere else
//! in this crate. Keeping rendering separate means we can later add
//! a `--json` flag or swap the colour library without touching any
//! business logic.
//!
//! Every function here takes a plain-data result (from [`mod@crate::analyze`],
//! [`mod@crate::compress`], [`crate::batch`], or [`crate::tools`]) and prints
//! it — none of them compute anything the caller couldn't already see
//! on the struct they're passed. Functions are grouped by the command
//! they serve; each group is marked with a banner comment below.

use crate::analyze::{Analysis, FilterSupport};
use crate::batch::{BatchItem, BatchReport};
use crate::compress::{CompressEvent, CompressOpts, Report};
use crate::profiles::Profile;
use crate::tools::ToolSet;
use colored::*;
use std::path::Path;

// ════════════════════════════════════════════════════════════════
//  Helpers
// ════════════════════════════════════════════════════════════════

/// Converts a byte count to megabytes (MiB, i.e. divided by 2^20) as
/// a float suitable for `{:.2}`-style formatting.
pub fn mb(bytes: u64) -> f64 {
    bytes as f64 / 1_048_576.0
}

/// Colours a percentage string green (good) above 5%, yellow
/// (marginal) at or below it. `s` is the *already-formatted* string
/// (so callers can pad it to a fixed width first — see
/// [`pct_column`] — without the colour escape codes throwing that
/// padding off).
fn ratio_colour_str(s: String, pct: f64) -> ColoredString {
    if pct > 5.0 {
        s.green().bold()
    } else {
        s.yellow()
    }
}

/// Colours a percentage green (good) above 5%, yellow (marginal) at
/// or below it. Used for one-off percentages that don't need to
/// line up under anything (single-file reports, batch totals).
fn ratio_colour(pct: f64) -> ColoredString {
    ratio_colour_str(format!("{:.1}%", pct), pct)
}

/// Right-aligns a percentage into a fixed-width `(NN.N%)` column,
/// then colours it. Padding happens on the plain string *before*
/// colouring, so the ANSI escape codes `colored` adds never get
/// counted as part of the width.
fn pct_column(pct: f64, width: usize) -> ColoredString {
    let s = format!("({:.1}%)", pct);
    ratio_colour_str(format!("{s:>width$}"), pct)
}

/// Best-effort terminal width in columns. Falls back to a sane
/// default (100) when stdout isn't a real terminal — piped into
/// `less`/a file/CI logs — or the width can't be determined, and
/// never returns anything so narrow that the fixed-width columns
/// wouldn't fit.
fn term_width() -> usize {
    terminal_size::terminal_size()
        .map(|(terminal_size::Width(w), _)| w as usize)
        .filter(|&w| w >= 20)
        .unwrap_or(100)
}

/// Shortens `name` to at most `max_width` *display* columns,
/// preserving the file extension where it reasonably can — so a
/// truncated `"quarterly-content-strategy-review.pdf"` reads as
/// `"quarterly-content-strat….pdf"`, not a dead end with no `.pdf`
/// in sight. Width is measured in `char`s, which matches actual
/// terminal columns for normal text (Arabic letters included) — it
/// under-counts only for combining marks or emoji, both rare in
/// real file names and not worth pulling in a full text-segmentation
/// crate for.
///
/// This exists so a long name gets cut short instead of wrapping
/// onto a second terminal row, which would break the line-per-file
/// layout the rest of this module relies on.
fn truncate_name(name: &str, max_width: usize) -> String {
    if max_width < 4 || name.chars().count() <= max_width {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        // Only treat it as "the extension" if it's short — otherwise
        // a name that's mostly one long dotted run (or hidden files
        // like ".gitignore") would eat the whole truncation budget.
        Some(i) if i > 0 && name[i..].chars().count() <= 6 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let stem_budget = max_width.saturating_sub(ext.chars().count() + 1); // +1 for '…'
    let cut: String = stem.chars().take(stem_budget.max(1)).collect();
    format!("{cut}…{ext}")
}

/// Shortens an arbitrary error message to at most `max_width` display
/// columns, for the same reason `truncate_name` exists: a single
/// over-long line must never wrap onto a second terminal row and
/// break the one-line-per-file batch layout. Unlike `truncate_name`,
/// there's no file extension worth preserving here — just a plain
/// cut with an ellipsis. The full, untruncated message is still
/// available via `--verbose`.
fn truncate_message(msg: &str, max_width: usize) -> String {
    if max_width < 4 || msg.chars().count() <= max_width {
        return msg.to_string();
    }
    let cut: String = msg.chars().take(max_width - 1).collect();
    format!("{cut}…")
}

/// Formats `path` quoted — for the same reason every path-shaped
/// value in this file's output is quoted: many real paths contain
/// spaces, so a plain unquoted `.display()` wouldn't mark where the
/// path ends and the rest of the line begins. Deliberately *not*
/// `format!("{path:?}")` though, despite `{:?}` also producing a
/// quoted string: `Debug`'s escaping (via `char::escape_debug`)
/// treats combining marks and bidi-control characters as needing
/// `\u{XXXX}` escapes. Combining marks are exactly how Arabic (and
/// Hebrew niqqud, Vietnamese, Thai, Devanagari, ...) diacritics are
/// encoded — completely ordinary orthography, not anything unusual —
/// so `{:?}` shredded any diacritic-bearing filename into
/// unreadable escape codes, e.g. `"شيوعا\u{64b}.pdf"` instead of the
/// filename exactly as it reads everywhere else: `"شيوعاً.pdf"`.
/// This only escapes the two characters that would otherwise make
/// the quoting itself ambiguous — a literal `"` or `\` inside the
/// path — and leaves every other character, combining marks
/// absolutely included, exactly as `.display()` would show it.
pub fn quoted_path<P: AsRef<Path>>(path: P) -> String {
    let escaped = path
        .as_ref()
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Shortens a path (already quoted by the caller, via
/// [`quoted_path`]) to at most `max_width` display columns, eliding
/// from the *front* rather than the back. Unlike `truncate_name`, the useful
/// part of a full path is usually its tail — the file name — so a
/// long one reads better as `"…/deep/nested/report.pdf"` than
/// `"/home/user/deep/nested/rep…"`, which would cut off the actual
/// file name and leave only directory noise. Used for the label:value
/// header lines (`print_compress_header`, `print_batch_header`) for
/// the same reason `truncate_name`/`truncate_message` exist: a path
/// long enough to wrap onto a second terminal row breaks that line's
/// `label : value` alignment, since the wrapped continuation starts
/// back at column 0 instead of lining up under the value.
fn truncate_path(path: &str, max_width: usize) -> String {
    if max_width < 4 || path.chars().count() <= max_width {
        return path.to_string();
    }
    let budget = max_width - 1; // reserve 1 column for '…'
    let tail: String = {
        let mut chars: Vec<char> = path.chars().rev().take(budget).collect();
        chars.reverse();
        chars.into_iter().collect()
    };
    format!("…{tail}")
}

/// One-line summary of what a profile will actually do, e.g.
/// `"balanced (quality 80)"` or `"lossless (no quality loss)"`.
pub fn profile_line(profile_name: &str, opts: &CompressOpts) -> String {
    if opts.lossless {
        format!("{profile_name}  (lossless — no quality loss)")
    } else {
        format!("{profile_name}  (quality {})", opts.quality)
    }
}

/// Prints the "nothing will actually be written" banner shown right
/// under the `pdftamp` title whenever `--dry-run` is set — shared by
/// [`print_compress_header`] and [`print_batch_header`] so the two
/// commands announce it identically. A no-op when `dry_run` is
/// `false`, so call sites can call it unconditionally.
fn print_dry_run_banner(dry_run: bool) {
    if dry_run {
        println!(
            "  {}",
            "DRY RUN — nothing will be created, overwritten, or renamed on disk"
                .yellow()
                .bold()
        );
    }
}

// ════════════════════════════════════════════════════════════════
//  Single-file compression
// ════════════════════════════════════════════════════════════════

/// Prints the pre-flight summary shown before a single-file
/// compression run starts: input/output paths, input size, and the
/// resolved profile.
pub fn print_compress_header(
    input: &std::path::Path,
    output: &std::path::Path,
    size: u64,
    profile_name: &str,
    opts: &CompressOpts,
) {
    // "  Input      : " / "  Output     : " are both 15 columns —
    // whatever's left after that is what the path itself gets, so a
    // long one gets truncated instead of wrapping onto a second row.
    let path_budget = term_width().saturating_sub(15);
    println!("{}", "pdftamp".bold().cyan());
    print_dry_run_banner(opts.dry_run);
    println!(
        "  Input      : {}",
        truncate_path(&quoted_path(input), path_budget)
    );
    println!(
        "  Output     : {}",
        truncate_path(&quoted_path(output), path_budget)
    );
    println!("  Size       : {:.2} MB", mb(size));
    println!("  Profile    : {}", profile_line(profile_name, opts));
    println!(
        "  If exists  : {} (change with --if-exists={})",
        opts.on_conflict.describe(),
        opts.on_conflict.other_values(),
    );
    println!();
}

/// Prints the full result of a single-file compression: per-category
/// counts, metadata handling, before/after size, and bytes saved. If
/// [`Report::kept_original`] is set, prints the shorter
/// "kept unchanged" summary instead.
///
/// When [`Report::dry_run`] is set, every number shown is still the
/// genuine result of actually running the pipeline (see
/// [`crate::compress::compress`]'s own doc comment) — only the
/// wording changes, from what pdftamp *did* to what it *would do*, so
/// nothing here reads as a claim that a file was written when it
/// wasn't.
pub fn print_report(report: &Report) {
    println!();
    println!(
        "{}",
        if report.dry_run {
            "══════════════════ Result (dry run) ═══════════════".dimmed()
        } else {
            "══════════════════ Result ═════════════════════".dimmed()
        }
    );

    if report.dry_run {
        println!(
            "  {} nothing written — this is a preview of what a real run would do.",
            "→".cyan().bold(),
        );
    }

    if report.renamed_to_avoid_conflict {
        println!(
            "  {} the requested output name was already taken — {} {} instead.",
            "!".cyan().bold(),
            if report.dry_run {
                "would save as"
            } else {
                "saved as"
            },
            quoted_path(&report.final_output)
        );
    }

    if report.kept_original {
        println!(
            "  {} our rewrite would've been bigger than the original — {}.",
            "!".cyan().bold(),
            if report.dry_run {
                "would keep it unchanged"
            } else {
                "kept it unchanged"
            }
        );
        println!(
            "  Size               : {:.2} MB (unchanged)",
            mb(report.input_bytes)
        );
        println!(
            "{}",
            "═══════════════════════════════════════════════".dimmed()
        );
        return;
    }

    println!("  JPEG recompressed  : {}", report.jpeg_compressed);
    println!("  FlateDecode→JPEG   : {}", report.flate_converted);
    println!("  LZW→JPEG           : {}", report.lzw_converted);
    println!("  Streams deflated   : {}", report.streams_compressed);
    print_metadata_line(report);
    println!("  Input              : {:.2} MB", mb(report.input_bytes));
    println!(
        "  {}",
        if report.dry_run {
            format!("Output (would be)  : {:.2} MB", mb(report.output_bytes))
        } else {
            format!("Output             : {:.2} MB", mb(report.output_bytes))
        }
    );
    // "Saved" vs "Would save" are different lengths, so the label is
    // padded to the same 19-column width every other label in this
    // function is hand-padded to (2-column indent + 19 + ": " = the
    // 23-column prefix every value lines up under), instead of a
    // second hardcoded string with its own manually-counted spaces.
    println!(
        "  {:<19}: {:.2} MB  ({})",
        if report.dry_run {
            "Would save"
        } else {
            "Saved"
        },
        mb(report.bytes_saved().max(0) as u64),
        ratio_colour(report.saved_pct())
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════".dimmed()
    );

    if report.saved_pct() < 5.0 {
        println!();
        println!(
            "  {} Low savings? Try: {}",
            "💡".yellow(),
            "pdftamp analyze <file>".dimmed()
        );
    }
}

/// Prints one line per `CompressEvent` — called only when `--verbose`.
pub fn print_verbose_events(report: &Report) {
    for event in &report.events {
        use CompressEvent::*;
        match event {
            JpegRecompressed {
                object_id,
                bytes_saved,
            } => println!("  [jpeg]       {object_id}  ← {bytes_saved} B"),
            FlateToJpeg {
                object_id,
                bytes_saved,
            } => println!("  [flate→jpeg] {object_id}  ← {bytes_saved} B"),
            LzwToJpeg {
                object_id,
                bytes_saved,
            } => println!("  [lzw→jpeg]   {object_id}  ← {bytes_saved} B"),
            StreamDeflated {
                object_id,
                bytes_saved,
            } => println!("  [stream]     {object_id}  ← {bytes_saved} B"),
        }
    }
}

/// Renders a one-line summary of document-level metadata: whether
/// any was found, and whether it was actually removed (only happens
/// when `--strip-metadata` was passed — otherwise it's reported as
/// "kept" even if present, since stripping is opt-in only).
fn print_metadata_line(report: &Report) {
    if report.metadata_found.is_empty() {
        println!("  Metadata           : {}", "none found".dimmed());
        return;
    }

    let was_stripped =
        !report.metadata_fields_removed.is_empty() || report.xmp_bytes_removed.is_some();

    if !was_stripped {
        let mut found = Vec::new();
        if !report.metadata_found.info_fields.is_empty() {
            found.push(report.metadata_found.info_fields.join(", "));
        }
        if let Some(bytes) = report.metadata_found.xmp_bytes {
            found.push(format!("{:.1} KB XMP", bytes as f64 / 1024.0));
        }
        // A PDF can carry a lot of /Info fields — bound the joined
        // list to what's left of the line after its fixed prefix
        // ("  Metadata           : ", 23 columns) and suffix
        // (" (kept — pass --strip-metadata to remove)", 41 columns),
        // so a long one is truncated instead of wrapping the line.
        let budget = term_width().saturating_sub(23 + 41);
        println!(
            "  Metadata           : {} (kept — pass {} to remove)",
            truncate_message(&found.join(" + "), budget).yellow(),
            "--strip-metadata".cyan()
        );
        return;
    }

    let mut removed = Vec::new();
    if !report.metadata_fields_removed.is_empty() {
        removed.push(report.metadata_fields_removed.join(", "));
    }
    if let Some(bytes) = report.xmp_bytes_removed {
        removed.push(format!("{:.1} KB XMP", bytes as f64 / 1024.0));
    }
    let budget = term_width().saturating_sub(23);
    println!(
        "  Metadata stripped  : {}",
        truncate_message(&removed.join(" + "), budget).green()
    );
}

// ════════════════════════════════════════════════════════════════
//  Batch (directory) compression
// ════════════════════════════════════════════════════════════════

/// Prints the pre-flight summary shown before a batch (directory)
/// compression run starts, followed by the header for the per-file
/// results that [`print_live_item`] will stream in underneath it.
pub fn print_batch_header(
    input_dir: &std::path::Path,
    dest_description: &str,
    profile_name: &str,
    opts: &CompressOpts,
) {
    // Same 15-column-prefix budgeting as print_compress_header. Uses
    // truncate_message (elides from the back) rather than
    // truncate_path for `dest_description`, since it isn't always a
    // path — see its "next to each original" case in main.rs, where
    // keeping the front matters more than keeping the tail.
    let width = term_width().saturating_sub(15);
    println!("{}", "pdftamp".bold().cyan());
    print_dry_run_banner(opts.dry_run);
    println!(
        "  Input dir  : {}",
        truncate_path(&quoted_path(input_dir), width)
    );
    println!(
        "  Output     : {}",
        truncate_message(dest_description, width)
    );
    println!("  Profile    : {}", profile_line(profile_name, opts));
    println!(
        "  If exists  : {} (change with --if-exists={})",
        opts.on_conflict.describe(),
        opts.on_conflict.other_values(),
    );
    println!();
    println!(
        "{}",
        "══════════════════ Files ══════════════════════".dimmed()
    );
}

// Width of the `"NNNN.NN MB → NNNN.NN MB"` size column and the
// `"(100.0%)"` percentage column that precede the file name on every
// `print_live_item` line — see its doc comment for why they come
// *before* the name rather than after it.
const SIZES_WIDTH: usize = 23;
const PCT_WIDTH: usize = 8;
const STATS_WIDTH: usize = SIZES_WIDTH + 2 + PCT_WIDTH;
const PREFIX_WIDTH: usize = 2 + 1 + 1 + STATS_WIDTH + 2; // "  " + icon + " " + stats + "  "

/// Prints a single file's result the moment it's ready — call this
/// from the `on_item` callback passed to `compress_directory` so
/// results stream to the user one at a time, instead of all
/// appearing at once after the whole batch finishes.
///
/// A failed line shows a short *category* of what went wrong — e.g.
/// `already exists` — in the same fixed-width column a success line
/// shows its sizes in, with the file name last either way. It is
/// deliberately not the full error message: that column is a fixed
/// width so the name stays aligned, and the full message can be long
/// (a whole file path) or short depending on what failed, which a
/// truncated-to-fit version of doesn't summarize well. Pass `verbose`
/// for the complete message.
///
/// When `verbose` is set, also prints the exact path the file was
/// (or would have been) written to — useful for spot-checking where
/// a deeply-nested source file landed under a mirrored output tree,
/// or whether [`crate::paths::OnConflict::Rename`] had to fall back
/// to a numbered name for this particular file.
///
/// A successful item that ran under `--dry-run` (see
/// [`crate::compress::Report::dry_run`]) gets a distinct `≈` icon
/// instead of `✓`, so a dry-run batch never reads as though files
/// were actually written — the sizes and percentage shown are still
/// the genuine simulated numbers, same as everywhere else `dry_run`
/// shows up.
pub fn print_live_item(item: &BatchItem, verbose: bool) {
    let name = item
        .input
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");

    // The fixed-width column goes *first*, right after the icon —
    // sizes and percentage for a success line, a short reason for a
    // failed one — and the file name goes last, with nothing after
    // it, on every line without exception. That's the fix for the
    // RTL alignment bug: relying on the terminal to honour Unicode
    // directional-isolate marks (an earlier attempt at this) turned
    // out to be a dead end — plenty of terminals, including GNOME
    // Terminal/VTE's own bidi mode, just don't implement isolates,
    // so the reordering bug came right back. An earlier version of
    // this function's Err arm broke this same rule by appending the
    // error message *after* the name instead of before it — same
    // bug, same fix: nothing may ever follow the name on the line.
    //
    // The name is then truncated (not wrapped) to whatever room is
    // left, so a long title can't push the line onto a second
    // terminal row and break the one-line-per-file layout.
    let name_budget = term_width().saturating_sub(PREFIX_WIDTH).max(8);
    let name = truncate_name(name, name_budget);

    match &item.result {
        Ok(r) => {
            let sizes = format!(
                "{:>7.2} MB → {:>7.2} MB",
                mb(r.input_bytes),
                mb(r.output_bytes)
            );
            let icon = if r.dry_run {
                "≈".cyan().bold()
            } else {
                "✓".green().bold()
            };
            println!(
                "  {} {}  {}  {}",
                icon,
                sizes,
                pct_column(r.saved_pct(), PCT_WIDTH),
                name,
            );
        }
        Err(e) => {
            // Padded on the plain string first, *then* colourized —
            // padding a `ColoredString` directly is unreliable, since
            // `colored`'s custom `Display` impl doesn't necessarily
            // honour the formatter's width request the way a plain
            // `&str` does (the ANSI escape bytes can end up counted
            // as part of the "width").
            let reason = format!(
                "{:<width$}",
                short_reason(e, STATS_WIDTH),
                width = STATS_WIDTH
            );
            println!("  {} {}  {}", "✗".red().bold(), reason.dimmed(), name);
        }
    }

    if verbose {
        match &item.result {
            // For the collision case, the short reason above isn't a
            // truncation of `e` — it's already the complete situation
            // ("already exists"), so printing all of `e` here would
            // just say those same two words again. Show the colliding
            // path on its own instead: new information (which exact
            // file), not a repeat. Every other error prints in full
            // here, same as before, since short_reason there *is* a
            // truncation of it — see short_reason's own doc comment.
            Err(e) => {
                let detail = e.strip_suffix(" already exists").unwrap_or(e.as_str());
                println!("      {}", detail.dimmed());
            }
            Ok(r) if !item.output.as_os_str().is_empty() => {
                let path = item.output.display().to_string();
                if r.dry_run {
                    println!("      {}", format!("(would write) {path}").dimmed());
                } else {
                    println!("      {}", path.dimmed());
                }
            }
            Ok(_) => {}
        }
    }
}

/// Boils an error down to something short enough to sit in a
/// fixed-width column without ever needing to wrap. Recognizes the
/// specific message [`crate::paths::commit`] uses for an
/// [`crate::paths::OnConflict::Refuse`] collision — by far the most
/// common failure in a `compress-dir` run — and reports it as the
/// plain, short "already exists" rather than the full colliding path
/// (which the reader can already infer: same folder, same name,
/// `-compressed` suffix, exactly as shown in the header). Anything
/// else falls back to a generic truncation of the full message.
///
/// This is a display nicety, not a type-safe error classification —
/// `BatchItem::result` deliberately stays a plain `Result<Report,
/// String>` rather than growing a whole error-kind enum (or a trait
/// object to downcast) just so this one line can be shorter; this
/// checks the tail of the message text instead, which the pairing
/// with `commit`'s exact wording keeps reliable in practice.
fn short_reason(e: &str, width: usize) -> String {
    if e.ends_with("' already exists") {
        "already exists".to_string()
    } else {
        truncate_message(e, width)
    }
}

/// Prints a non-fatal warning to stderr — e.g. a `--log-file` write
/// that failed. Kept here, rather than as a stray `eprintln!` at the
/// call site, so this module stays the *only* one that touches the
/// terminal (stderr included) — see this file's own module doc.
/// Prints a `--log-file` write failure as a non-fatal warning — see
/// [`crate::log`]'s module doc comment for why this stays a warning
/// rather than aborting the run at this point (unlike
/// [`crate::log::validate_log_path`], which runs *before* any
/// compression work and does abort, since nothing's been done yet to
/// protect at that point).
///
/// Prints the full cause chain (`context: root cause`), not just the
/// top-level message — a bare "couldn't open log file '...'" doesn't
/// tell anyone *why*, and the underlying io::Error (permission
/// denied, is a directory, ...) is exactly the part that lets someone
/// actually fix it.
pub fn print_log_warning(e: &anyhow::Error) {
    let full: String = e
        .chain()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(": ");
    eprintln!("{} {}", "Warning:".yellow().bold(), full);
}

/// Prints the final totals after a batch completes. Per-file results
/// have already streamed via `print_live_item`, so this only shows
/// the summary line.
///
/// `dry_run` only changes the wording ("would save" vs "saved") —
/// the counts and byte totals themselves are already the genuine
/// simulated numbers from each file's [`crate::compress::Report`],
/// same as [`print_report`] for the single-file case.
pub fn print_batch_summary(batch: &BatchReport, dry_run: bool) {
    let saved = batch.total_bytes_saved();
    println!(
        "{}",
        "═══════════════════════════════════════════════".dimmed()
    );
    println!(
        "  {} succeeded, {} failed — {} {:.2} MB total  ({})",
        batch.succeeded_count().to_string().green().bold(),
        batch.failed_count().to_string().red(),
        if dry_run { "would save" } else { "saved" },
        mb(saved.max(0) as u64),
        ratio_colour(if batch.total_input_bytes() > 0 {
            saved as f64 / batch.total_input_bytes() as f64 * 100.0
        } else {
            0.0
        })
    );
    if dry_run {
        println!(
            "  {} dry run — nothing above was actually written. Re-run without {} to compress for real.",
            "→".cyan().bold(),
            "--dry-run".bold(),
        );
    }
    if !batch.skipped_own_output.is_empty() {
        println!(
            "  {} skipped {} file(s) that already looked like previous pdftamp output \
             (run `pdftamp compress` on one directly if you want to redo it).",
            "!".cyan().bold(),
            batch.skipped_own_output.len(),
        );
    }
    if batch.failed_count() > 0 {
        println!(
            "  {} some files failed — re-run with {} to see the full reason for each.",
            "!".cyan().bold(),
            "--verbose".bold(),
        );
    }
}

// ════════════════════════════════════════════════════════════════
//  Analysis
// ════════════════════════════════════════════════════════════════

/// Prints the full result of `pdftamp analyze`: a breakdown of
/// image filters (with per-filter compression support), content
/// stream types, and document metadata.
pub fn print_analysis(a: &Analysis) {
    println!(
        "{}",
        "══════════════════ PDF Analysis ═══════════════".dimmed()
    );

    println!();
    println!("  {}  Image XObjects:", "📸".bold());
    if a.images.is_empty() {
        println!("     (none)");
    } else {
        let mut sorted: Vec<_> = a.images.iter().collect();
        sorted.sort_by_key(|(_, s)| std::cmp::Reverse(s.bytes));
        for (filter, stats) in sorted {
            println!(
                "   {:<22}  {:>4} image(s)  {:>7.2} MB   {}",
                filter,
                stats.count,
                mb(stats.bytes),
                support_badge(filter)
            );
        }
    }

    println!();
    println!("  {}  Content Streams:", "📄".bold());
    println!(
        "   {:<22}  {:>4} stream(s) {:>7.2} MB   {}",
        "No filter (raw)",
        a.raw_streams.count,
        mb(a.raw_streams.bytes),
        if a.raw_streams.count > 0 {
            "✓ compressible".green().to_string()
        } else {
            String::new()
        }
    );
    println!(
        "   {:<22}  {:>4} stream(s) {:>7.2} MB",
        "FlateDecode",
        a.flate_streams.count,
        mb(a.flate_streams.bytes)
    );
    println!(
        "   {:<22}  {:>4} stream(s) {:>7.2} MB",
        "Other",
        a.other_streams.count,
        mb(a.other_streams.bytes)
    );

    print_metadata_summary(a);

    println!();
    println!(
        "{}",
        "═══════════════════════════════════════════════".dimmed()
    );
}

/// Prints the "Document Metadata" section of [`print_analysis`]'s
/// output — what was found, with no mention of removal (analysis
/// never modifies anything).
fn print_metadata_summary(a: &Analysis) {
    println!();
    println!("  {}  Document Metadata:", "🪪".bold());
    if a.metadata.is_empty() {
        println!("     (none found)");
        return;
    }
    if !a.metadata.info_fields.is_empty() {
        // Same reasoning as the budget in `print_metadata_line`: bound
        // the joined field list to what's left after the "   /Info
        // fields : " prefix (18 columns) so a PDF with a lot of them
        // gets truncated instead of wrapping the line.
        let budget = term_width().saturating_sub(18);
        println!(
            "   /Info fields : {}",
            truncate_message(&a.metadata.info_fields.join(", "), budget)
        );
    }
    if let Some(bytes) = a.metadata.xmp_bytes {
        println!("   XMP stream   : {:.1} KB", bytes as f64 / 1024.0);
    }
    println!(
        "   {} kept by default — pass {} if you want this removed",
        "→".dimmed(),
        "--strip-metadata".cyan()
    );
}

/// Maps a filter name to a coloured "supported"/"not yet
/// supported"/"unknown" badge, via [`Analysis::support_for`].
fn support_badge(filter: &str) -> String {
    match Analysis::support_for(filter) {
        FilterSupport::Supported => "✓ supported".green().to_string(),
        FilterSupport::KnownUnsupported => "⚠️  not yet supported".yellow().to_string(),
        FilterSupport::Unknown => "⚠️  unknown".dimmed().to_string(),
    }
}

// ════════════════════════════════════════════════════════════════
//  External tool availability (--verbose)
// ════════════════════════════════════════════════════════════════

/// Prints one line per optional external accelerator (`jpegoptim`,
/// `oxipng`, `pngquant`, `qpdf`), showing whether each was found and,
/// if not, how to install it. Only printed when `--verbose` is passed.
pub fn print_tool_statuses(tools: &ToolSet) {
    println!("  External tools:");
    for status in tools.statuses() {
        if status.found {
            println!("    {}  {}", "✓".green().bold(), status.name);
        } else {
            println!(
                "    {}  {:<10} (not found — {})",
                "✗".red().bold(),
                status.name,
                status.install_hint.dimmed()
            );
        }
    }
    println!();
}

// ════════════════════════════════════════════════════════════════
//  Profiles list (`pdftamp profiles`)
// ════════════════════════════════════════════════════════════════

/// Prints every [`Profile`] with its quality/losslessness and
/// description — the full output of `pdftamp profiles`.
pub fn print_profiles_list() {
    println!(
        "{}",
        "══════════════════ Compression Profiles ═══════".dimmed()
    );
    println!();
    // "  {name:<10} {(quality label):<12} " is a fixed 26-column
    // prefix — each description is short enough by design to fit
    // after it on an 80-column terminal, but this is still a backstop
    // for narrower ones, same reasoning as everywhere else in this
    // module.
    let desc_budget = term_width().saturating_sub(26);
    for profile in Profile::ALL {
        let quality_label = if profile.is_lossless() {
            "lossless".to_string()
        } else {
            format!("quality {}", profile.quality())
        };
        println!(
            "  {:<10} {:<12} {}",
            profile.name().bold().cyan(),
            format!("({quality_label})").dimmed(),
            truncate_message(profile.description(), desc_budget)
        );
    }
    println!();
    println!(
        "  {}",
        "Usage: pdftamp compress in.pdf out.pdf --profile email".dimmed()
    );
    println!(
        "{}",
        "═════════════════════════════════════════════════".dimmed()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_path_wraps_a_plain_path_in_quotes() {
        assert_eq!(
            quoted_path(Path::new("/home/user/report.pdf")),
            "\"/home/user/report.pdf\""
        );
    }

    #[test]
    fn quoted_path_preserves_arabic_combining_diacritics_unescaped() {
        // The exact bug this function exists to fix: `{:?}` (Debug)
        // shreds combining marks like Arabic tashkeel into `\u{XXXX}`
        // escapes. "شيوعاً" is "شيوعا" + a combining FATHATAN
        // (U+064B) — completely ordinary Arabic orthography.
        let path = Path::new("شيوعاً.pdf");
        assert_eq!(quoted_path(path), "\"شيوعاً.pdf\"");
        // Confirm this would *not* have been true via Debug, so the
        // test actually exercises the distinction it claims to.
        assert_ne!(quoted_path(path), format!("{path:?}"));
    }

    #[test]
    fn quoted_path_escapes_an_embedded_literal_quote() {
        let path = Path::new("my \"quoted\" file.pdf");
        assert_eq!(quoted_path(path), "\"my \\\"quoted\\\" file.pdf\"");
    }

    #[test]
    fn quoted_path_escapes_an_embedded_literal_backslash() {
        let path = Path::new("weird\\name.pdf");
        assert_eq!(quoted_path(path), "\"weird\\\\name.pdf\"");
    }
}
