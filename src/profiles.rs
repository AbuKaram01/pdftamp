// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! Named compression presets.
//!
//! Most users don't think in terms of "JPEG quality 75" — they think
//! "I need to email this" or "this will be printed". A [`Profile`]
//! translates that intent into the actual knobs [`CompressOpts`]
//! understands.

use crate::compress::CompressOpts;

/// A named compression preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Maximum possible compression — for documents where size matters
    /// far more than visual fidelity (quick previews, bulk archiving
    /// of low-priority scans). Noticeably more aggressive than
    /// [`Profile::Archive`].
    Extreme,
    /// Smallest possible output — for documents rarely opened again.
    Archive,
    /// Small enough to comfortably attach to an email or chat message.
    Email,
    /// Tuned for reading on a phone/tablet screen.
    Ebook,
    /// Sensible default that works well for most documents.
    #[default]
    Balanced,
    /// Sharper text and images, for internal work reports.
    Office,
    /// High quality — use when the document will be printed.
    Print,
    /// No quality loss at all — never re-encodes pixel data, and
    /// deflates uncompressed streams (always lossless). Metadata is
    /// untouched here too unless separately opted into via
    /// `strip_metadata`. Intended for contracts, legal documents, and
    /// anything where even a small visual change is unacceptable.
    Lossless,
}

impl Profile {
    /// Every profile, in the order they should be presented to a user
    /// (roughly smallest-output to highest-fidelity).
    pub const ALL: [Profile; 8] = [
        Profile::Extreme,
        Profile::Archive,
        Profile::Email,
        Profile::Ebook,
        Profile::Balanced,
        Profile::Office,
        Profile::Print,
        Profile::Lossless,
    ];

    /// Stable, lowercase identifier — used for `--profile` flag values
    /// and for round-tripping through [`Profile::parse`].
    pub fn name(&self) -> &'static str {
        match self {
            Profile::Extreme => "extreme",
            Profile::Archive => "archive",
            Profile::Email => "email",
            Profile::Ebook => "ebook",
            Profile::Balanced => "balanced",
            Profile::Office => "office",
            Profile::Print => "print",
            Profile::Lossless => "lossless",
        }
    }

    /// One-line, human-readable explanation of when to use this profile.
    pub fn description(&self) -> &'static str {
        match self {
            Profile::Extreme => "Maximum compression — quick previews, bulk archiving",
            Profile::Archive => "Smallest size — documents rarely opened again",
            Profile::Email => "Small enough to email or message",
            Profile::Ebook => "Good for reading on phone/tablet screens",
            Profile::Balanced => "Sensible default for most documents",
            Profile::Office => "Sharper text/images for work reports",
            Profile::Print => "High quality — document will be printed",
            Profile::Lossless => "Zero quality loss whatsoever (contracts, legal)",
        }
    }

    /// JPEG re-encode quality this profile maps to.
    ///
    /// These aren't arbitrary — each value is anchored to a
    /// perceptual quality band from established JPEG rate-distortion
    /// behavior (roughly: quality's relationship to both file size and
    /// visible artifacting is non-linear, with big perceptual jumps
    /// below ~50 and rapidly diminishing visual returns above ~90):
    ///
    /// | Band | Quality | Character |
    /// |---|---|---|
    /// | Poor | ~20-40 | Visible blocking; fine for thumbnails/previews only |
    /// | Acceptable | ~40-60 | Visible under normal viewing, fine when rarely revisited |
    /// | Good | ~60-80 | Minor loss only on close inspection; typical web/casual default |
    /// | Very good | ~80-90 | Minor loss only on zoomed inspection |
    /// | Excellent | 90+ | Visually indistinguishable, but file size grows sharply per point |
    ///
    /// Two values are the load-bearing anchors the rest are pinned
    /// around, and shouldn't move without a real reason to:
    /// [`Profile::Balanced`]'s 80 sits at the widely-cited "sweet
    /// spot" for JPEG (the point past which visual gains slow but
    /// file size doesn't yet grow sharply), and [`Profile::Print`]'s
    /// 90 is the practical ceiling for a *lossy* preset — past this
    /// point the Excellent band's returns are so diminished that
    /// [`Profile::Lossless`] is the more honest option, not a 93 or a
    /// 96 preset.
    ///
    /// [`Profile::Ebook`] and [`Profile::Office`] necessarily land
    /// close to their neighbors ([`Profile::Balanced`] on one side,
    /// [`Profile::Email`]/[`Profile::Print`] on the other) — that
    /// tightness is a property of the Good/Very-good bands genuinely
    /// being narrow, not an oversight; forcing wider gaps there would
    /// mean moving one of the two anchors above for no technical
    /// reason.
    ///
    /// Meaningless for [`Profile::Lossless`], which never re-encodes
    /// pixel data at all.
    pub fn quality(&self) -> u8 {
        match self {
            Profile::Extreme => 25, // Poor band: max compression is *supposed* to look compressed.
            Profile::Archive => 50, // Acceptable band, upper edge: still visibly lossy, fine unrevisited.
            Profile::Email => 65, // Good band, lower half: casual-viewing default for attachments.
            Profile::Ebook => 72, // Good band, upper half: a bit crisper for on-screen reading.
            Profile::Balanced => 80, // Anchor — the standard JPEG "sweet spot".
            Profile::Office => 85, // Very-good band, midpoint between the two anchors below.
            Profile::Print => 90, // Anchor — ceiling for a lossy preset; beyond this, use Lossless.
            Profile::Lossless => 100,
        }
    }

    /// `true` only for [`Profile::Lossless`].
    pub fn is_lossless(&self) -> bool {
        matches!(self, Profile::Lossless)
    }

    /// Builds the [`CompressOpts`] this profile corresponds to.
    /// Only sets `quality`/`lossless` — `strip_metadata` and
    /// `allow_decrypt` are deliberately **not** tied to any profile.
    /// They're orthogonal, opt-in-only decisions (privacy and
    /// encryption-removal respectively) that a profile choice
    /// shouldn't silently imply; the caller (a CLI flag) sets them
    /// explicitly on top of whatever this returns.
    ///
    /// Takes `self` by value (not `&self`) to match the `to_*`
    /// naming convention for a `Copy` type — see clippy's
    /// `wrong_self_convention`. Free either way since `Profile` is a
    /// unit-only enum.
    pub fn to_opts(self) -> CompressOpts {
        CompressOpts {
            quality: self.quality(),
            lossless: self.is_lossless(),
            ..Default::default()
        }
    }

    /// Looks up a profile by name, case-insensitively. Returns `None`
    /// for anything that doesn't match one of [`Profile::ALL`].
    pub fn parse(name: &str) -> Option<Profile> {
        Self::ALL
            .into_iter()
            .find(|p| p.name().eq_ignore_ascii_case(name))
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(Profile::parse("EMAIL"), Some(Profile::Email));
        assert_eq!(Profile::parse("Print"), Some(Profile::Print));
    }

    #[test]
    fn parse_rejects_unknown_names() {
        assert_eq!(Profile::parse("ultra-mega-compress"), None);
    }

    #[test]
    fn lossless_profile_sets_lossless_flag() {
        let opts = Profile::Lossless.to_opts();
        assert!(opts.lossless);
    }

    #[test]
    fn quality_profiles_increase_monotonically() {
        // extreme < archive < email < ebook < balanced < office < print
        // (lossless is excluded — its quality() value is unused)
        let ordered = [
            Profile::Extreme,
            Profile::Archive,
            Profile::Email,
            Profile::Ebook,
            Profile::Balanced,
            Profile::Office,
            Profile::Print,
        ];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].quality() < pair[1].quality(),
                "{:?} ({}) should be < {:?} ({})",
                pair[0],
                pair[0].quality(),
                pair[1],
                pair[1].quality()
            );
        }
    }

    #[test]
    fn anchor_qualities_are_unchanged() {
        // Guards the two values the rest of the quality scale is
        // pinned around (see `Profile::quality`'s docs). A change to
        // either number is a deliberate design decision, not an
        // accidental tweak — this test forces it to show up as an
        // explicit diff here rather than slipping through unnoticed.
        assert_eq!(
            Profile::Balanced.quality(),
            80,
            "Balanced is anchored to the standard JPEG sweet spot"
        );
        assert_eq!(
            Profile::Print.quality(),
            90,
            "Print is anchored to the practical ceiling before Lossless"
        );
    }
}
