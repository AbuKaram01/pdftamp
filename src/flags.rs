// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! CLI surface defined with `clap` — argument parsing only, no logic.
//! The actual work happens in `main.rs` after dispatching on
//! [`Commands`].
//!
//! The `///` doc comments on [`Commands`] and its fields below serve
//! double duty: `clap`'s `derive` macro turns them into the `--help`
//! text users see at the terminal, and rustdoc turns them into this
//! page. Keep that in mind when editing — wording changes here are
//! user-facing.
//!
//! Two flags below are deliberately **opt-in, not opt-out**:
//! `--strip-metadata` and `--allow-decrypt`. pdftamp's job is to
//! change file size — nothing else. Removing document metadata or
//! bypassing a file's encryption are separate decisions the user has
//! to ask for explicitly; neither happens just because you asked for
//! compression.
//!
//! Keep every field's doc comment to one short line where at all
//! possible. `clap` prints it right after the flag on the same
//! terminal row, so a long line just wraps onto a second row indented
//! under the first — legal, but easy to avoid by staying concise.
//! Longer explanations belong in the *command*-level doc comment
//! (shown separately, in full, by `<command> --help`), not repeated
//! on every flag. [`Commands::CompressDir`] additionally uses
//! `#[command(verbatim_doc_comment)]` because its doc comment has a
//! fixed-format example block — `clap` reflows plain doc comments to
//! the terminal width by default, which would otherwise merge that
//! example's lines into one run-on paragraph.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Top-level argument parser. `pdftamp <subcommand> ...`.
#[derive(Parser)]
#[command(
    name = "pdftamp",
    version,
    about = "🗜  Compress PDF files — see subcommands below"
)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Commands,
}

/// Every subcommand pdftamp supports.
#[derive(Subcommand)]
pub enum Commands {
    /// Compress a single PDF file.
    Compress {
        /// Path to the PDF file to compress.
        input: PathBuf,

        /// Defaults to `<name>-compressed.pdf` next to the input.
        output: Option<PathBuf>,

        /// Profile: extreme/archive/email/ebook/balanced/office/print/lossless.
        #[arg(short, long, default_value = "balanced")]
        profile: String,

        /// JPEG quality override, 1-95 (ignored by the lossless profile).
        #[arg(short, long)]
        quality: Option<u8>,

        /// Also strip metadata: author, timestamps, EXIF. Off by default.
        #[arg(short = 's', long)]
        strip_metadata: bool,

        /// Allow decrypting PDFs with no real password set. Off by default.
        #[arg(long)]
        allow_decrypt: bool,

        /// Print one line per modified object.
        #[arg(short, long)]
        verbose: bool,

        /// What to do when the output already exists.
        #[arg(long, value_enum, default_value = "refuse")]
        if_exists: crate::paths::OnConflict,

        /// Append a plain-text run record to this file (created if needed).
        #[arg(long)]
        log_file: Option<PathBuf>,

        /// Show what would happen, but don't write, overwrite, or rename anything.
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
    },

    /// Recursively compress every PDF in a directory tree
    ///
    /// Each file saves next to its own original by default, the same
    /// as `compress`. Pass an output directory to mirror the whole
    /// tree there instead.
    ///
    /// Examples:
    ///   pdftamp compress-dir ./books --profile email
    ///     books/programming/rust.pdf -> books/programming/rust-compressed.pdf
    ///
    ///   pdftamp compress-dir ./books ./books-compressed --profile email
    ///     books/programming/rust.pdf -> books-compressed/programming/rust.pdf
    #[command(verbatim_doc_comment)]
    CompressDir {
        /// Root of the source directory tree.
        input_dir: PathBuf,

        /// Mirror into this dir instead of saving next to each original.
        output_dir: Option<PathBuf>,

        /// Compression profile — see `pdftamp profiles`.
        #[arg(short, long, default_value = "balanced")]
        profile: String,

        /// Override the profile's JPEG quality (1-95).
        #[arg(short, long)]
        quality: Option<u8>,

        /// Strip document/image metadata for every file (see `compress --help`).
        #[arg(short = 's', long)]
        strip_metadata: bool,

        /// Allow auto-decrypting files where possible (see `compress --help`).
        #[arg(long)]
        allow_decrypt: bool,

        /// Print one line per modified object, per file.
        #[arg(short, long)]
        verbose: bool,

        /// Same as `compress --if-exists`, applied per file.
        #[arg(long, value_enum, default_value = "refuse")]
        if_exists: crate::paths::OnConflict,

        /// Append a run record — one line per file — to this file.
        #[arg(long)]
        log_file: Option<PathBuf>,

        /// Same as `compress --dry-run`, applied to every file (see `compress --help`).
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,
    },

    /// Analyse a PDF's contents without modifying it.
    ///
    /// Shows the image filters and stream types behind the
    /// compression result.
    Analyze {
        /// Path to the PDF file to inspect.
        input: PathBuf,

        /// Allow decrypting when there's no real password. Off by default.
        #[arg(long)]
        allow_decrypt: bool,
    },

    /// List every available compression profile and what it's for.
    Profiles,
}
