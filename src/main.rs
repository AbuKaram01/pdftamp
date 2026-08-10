// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! # pdftamp
//!
//! A command-line tool that shrinks PDF file size using an 80/20
//! approach: re-encode embedded images at a lower quality, and
//! Deflate-compress any content streams that aren't compressed at
//! all. Both steps are lossless to the PDF's structure — only the
//! stream *contents* change, so the document opens and reads exactly
//! as before, just smaller.
//!
//! ## Crate layout
//!
//! This is a binary-only crate (no library target, no public API) —
//! the module boundaries below exist purely to keep the codebase
//! organized, not to expose anything to other crates.
//!
//! | Module | Responsibility |
//! |---|---|
//! | [`flags`] | Command-line argument definitions (`clap`). |
//! | [`mod@analyze`] | Read-only inspection of a PDF's compressible content. |
//! | [`profiles`] | Named quality presets (`web`, `print`, `lossless`, ...). |
//! | [`mod@compress`] | The single-file compression pipeline. |
//! | [`batch`] | Recursive, whole-directory compression. |
//! | [`images`] | Image re-encoding (JPEG re-compression, PNG-style filters, LZW/Flate → JPEG). |
//! | [`streams`] | Generic (non-image) content-stream compression. |
//! | [`predictor`] | PNG row-prediction reversal, used by [`images`]. |
//! | [`loader`] | PDF loading, including the optional `qpdf`-assisted decrypt path. |
//! | [`metadata`] | `/Info` dictionary and XMP metadata stripping. |
//! | [`tools`] | Detection of optional external accelerators (`jpegoptim`, `oxipng`, `pngquant`, `qpdf`). |
//! | [`paths`] | Output-path derivation, overwrite-conflict resolution, and same-file/same-directory safety checks. |
//! | [`render`] | All terminal output — the only module allowed to call `println!`. |
//! | [`log`] | Optional plain-text run history (`--log-file`) — file output, as distinct from `render`'s terminal output. |
//!
//! [`main`] itself only routes a parsed [`Commands`] variant to the
//! matching module and hands the result to [`render`]; it holds no
//! business logic of its own.
//!
//! ## Viewing these docs
//!
//! Because this is a binary crate, `cargo doc` won't include private
//! items by default. Use:
//!
//! ```text
//! cargo doc --document-private-items --no-deps --open
//! ```

mod analyze;
mod batch;
mod compress;
mod flags;
mod images;
mod loader;
mod log;
mod metadata;
mod paths;
mod predictor;
mod profiles;
mod render;
mod streams;
mod tools;

#[cfg(test)]
mod integration_tests;

use analyze::analyze;
use anyhow::{anyhow, Result};
use batch::compress_directory;
use clap::Parser;
use compress::{compress, CompressOpts};
use flags::{Cli, Commands};
use profiles::Profile;
use tools::ToolSet;

/// Entry point.
///
/// Parses argv as a [`Cli`] and dispatches on [`Commands`]. Each
/// match arm resolves its options, delegates to the relevant module
/// for the actual work, and hands the result to [`render`] — this
/// function contains no PDF-processing logic itself.
///
/// # Errors
///
/// Returns an error if argument resolution fails (e.g. an unknown
/// `--profile` name), if the input/output paths are invalid (e.g.
/// output would overwrite the input), or if the underlying compress
/// or analyze call fails.
fn main() -> Result<()> {
    match Cli::parse().command {
        Commands::Profiles => {
            render::print_profiles_list();
        }

        Commands::Analyze {
            input,
            allow_decrypt,
        } => {
            let result = analyze(&input, allow_decrypt)?;
            render::print_analysis(&result);
        }

        Commands::Compress {
            input,
            output,
            profile,
            quality,
            strip_metadata,
            allow_decrypt,
            verbose,
            if_exists,
            log_file,
            dry_run,
        } => {
            let opts = resolve_opts(
                &profile,
                quality,
                strip_metadata,
                allow_decrypt,
                verbose,
                if_exists,
                dry_run,
            )?;
            let output = output.unwrap_or_else(|| paths::default_output_path(&input));

            if paths::same_file(&input, &output) {
                return Err(anyhow!(
                    "Output path '{}' resolves to the same file as the input — refusing to compress a PDF onto itself.",
                    output.display()
                ));
            }

            // Catch a bad --log-file (a directory, a typo'd path, a
            // permission problem, ...) before doing any real work —
            // see `log::validate_log_path`'s own doc comment for why
            // this can't just wait for the real write at the end.
            if let Some(log_path) = &log_file {
                log::validate_log_path(log_path)?;
            }

            if verbose {
                render::print_tool_statuses(&ToolSet::detect());
            }

            let size = std::fs::metadata(&input)?.len();
            render::print_compress_header(&input, &output, size, &profile, &opts);

            // Written *before* compress() runs, not after — so if the
            // run gets interrupted partway through, the log at least
            // shows an attempt was made with these settings, instead
            // of nothing at all. See `log`'s module doc comment.
            let mut log_broken = false;
            if let Some(log_path) = &log_file {
                if let Err(e) =
                    log::write_header_compress(log_path, &input, &output, &profile, &opts)
                {
                    render::print_log_warning(&e);
                    log_broken = true;
                }
            }

            let result = compress(&input, &output, &opts);

            if !log_broken {
                if let Some(log_path) = &log_file {
                    if let Err(e) = log::write_result_compress(log_path, &result) {
                        render::print_log_warning(&e);
                    }
                }
            }

            let report = result?;

            if verbose {
                render::print_verbose_events(&report);
            }
            render::print_report(&report);
        }

        Commands::CompressDir {
            input_dir,
            output_dir,
            profile,
            quality,
            strip_metadata,
            allow_decrypt,
            verbose,
            if_exists,
            log_file,
            dry_run,
        } => {
            let opts = resolve_opts(
                &profile,
                quality,
                strip_metadata,
                allow_decrypt,
                verbose,
                if_exists,
                dry_run,
            )?;

            // An explicit output dir mirrors the tree under it (old
            // behaviour, still available on request); with none
            // given, each file now saves next to its own original —
            // see `batch::DestStrategy` for why that's the default.
            let dest = match &output_dir {
                Some(dir) => {
                    if paths::is_same_or_within(&input_dir, dir) {
                        return Err(anyhow!(
                            "Output directory '{}' is the same as, or nested inside, input directory '{}' — this would make the batch job walk into files it just wrote. Choose a separate output directory.",
                            dir.display(),
                            input_dir.display()
                        ));
                    }
                    // A dry run never creates this directory either —
                    // see `compress()`'s and `compress_directory()`'s
                    // own doc comments on why nothing gets created on
                    // disk in that mode, right down to the mirrored
                    // subdirectories each file would otherwise land in.
                    if !opts.dry_run {
                        std::fs::create_dir_all(dir)?;
                    }
                    batch::DestStrategy::Mirror(dir)
                }
                None => batch::DestStrategy::NextToOriginal,
            };

            // Same reasoning as `compress`'s check above, but it
            // matters even more here: a batch job can run for
            // minutes across a whole library before the old
            // after-the-fact warning ever got a chance to fire. Catch
            // a bad --log-file now, before a single file gets
            // touched.
            if let Some(log_path) = &log_file {
                log::validate_log_path(log_path)?;
            }

            if verbose {
                render::print_tool_statuses(&ToolSet::detect());
            }

            let dest_description = match &dest {
                batch::DestStrategy::NextToOriginal => {
                    "next to each original (<name>-compressed.pdf)".to_string()
                }
                batch::DestStrategy::Mirror(dir) => render::quoted_path(dir),
            };
            render::print_batch_header(&input_dir, &dest_description, &profile, &opts);

            // Written *before* the batch starts, then once more per
            // file (from the same `on_item` callback that already
            // streams each result to the terminal live) as each one
            // finishes, rather than only once at the very end. If the
            // process gets interrupted partway through a large
            // library, the log ends up showing every file that
            // actually finished before that point instead of nothing
            // at all — see `log`'s module doc comment for the full
            // reasoning.
            let mut log_broken = false;
            if let Some(log_path) = &log_file {
                if let Err(e) = log::write_header_batch(
                    log_path,
                    &input_dir,
                    &dest_description,
                    &profile,
                    &opts,
                ) {
                    render::print_log_warning(&e);
                    log_broken = true;
                }
            }

            let batch = compress_directory(&input_dir, dest, &opts, |item| {
                render::print_live_item(item, verbose);
                if !log_broken {
                    if let Some(log_path) = &log_file {
                        if let Err(e) = log::write_item_batch(log_path, item) {
                            render::print_log_warning(&e);
                            log_broken = true;
                        }
                    }
                }
            })?;

            render::print_batch_summary(&batch, opts.dry_run);

            if !log_broken {
                if let Some(log_path) = &log_file {
                    if let Err(e) = log::write_footer_batch(log_path, &batch, &opts) {
                        render::print_log_warning(&e);
                    }
                }
            }
        }
    }

    Ok(())
}

/// Resolves a `--profile` name (plus optional overrides) into a
/// concrete [`CompressOpts`].
///
/// `--quality` always wins over the profile's own value, and also
/// forces `lossless = false` — passing a quality number is an
/// unambiguous signal the user wants a lossy re-encode at that level.
///
/// `strip_metadata` and `allow_decrypt` are passed straight through:
/// they're opt-in-only flags, never implied by a profile choice (see
/// [`Profile::to_opts`]'s docs for why). Same for `if_exists`: which
/// existing files are safe to touch is the caller's call, never the
/// profile's. `dry_run` is equally orthogonal to the profile — it
/// changes nothing about *what* would be compressed or *how well*,
/// only whether the result actually gets written.
///
/// # Errors
///
/// Returns an error if `profile_name` doesn't match any known
/// [`Profile`].
fn resolve_opts(
    profile_name: &str,
    quality_override: Option<u8>,
    strip_metadata: bool,
    allow_decrypt: bool,
    verbose: bool,
    if_exists: paths::OnConflict,
    dry_run: bool,
) -> Result<CompressOpts> {
    let profile = Profile::parse(profile_name).ok_or_else(|| {
        anyhow!("Unknown profile '{profile_name}'. Run `pdftamp profiles` to see the list.")
    })?;

    let mut opts = profile.to_opts();
    if let Some(q) = quality_override {
        opts.quality = q;
        opts.lossless = false;
    }
    opts.strip_metadata = strip_metadata;
    opts.allow_decrypt = allow_decrypt;
    opts.emit_events = verbose;
    opts.on_conflict = if_exists;
    opts.dry_run = dry_run;

    Ok(opts)
}
