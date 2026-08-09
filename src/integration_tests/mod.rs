// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Integration-style tests, split across multiple files for
//! readability instead of one very long one. See `common.rs` in this
//! module for shared fixture-building and assertion helpers used
//! throughout.

mod common;

mod acroform;
mod basic;
mod batch;
mod embedded_files;
mod encryption;
mod javascript;
mod metadata;
mod navigation;
mod trailer_id;
