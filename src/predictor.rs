// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 AbuKaram01

//! PDF reuses PNG's row-prediction filters before applying `FlateDecode`
//! to improve compression ratio. This module reverses that step,
//! returning the raw pixel bytes.
//!
//! See: PNG spec, section "Filtering" (filter types 0-4).

/// Reverses PNG row prediction on data that was decompressed via
/// `FlateDecode`.
///
/// `bpp` = bytes per pixel (channels * bytes_per_channel). Each row in
/// `data` is prefixed by one filter-type byte (0-4), as defined by the
/// PNG spec.
///
/// Returns `None` if `data` is shorter than expected for the given
/// dimensions, or if a row uses an unrecognized filter type.
pub fn undo(data: &[u8], width: u32, height: u32, bpp: usize) -> Option<Vec<u8>> {
    let stride = width as usize * bpp;
    let expected = (stride + 1) * height as usize; // +1 per row for the filter-type byte

    if data.len() < expected {
        return None;
    }

    let mut out = Vec::with_capacity(stride * height as usize);
    let mut prev = vec![0u8; stride]; // previous (already-decoded) row; zero for the first row
    let mut pos = 0;

    for _ in 0..height {
        let filter = data[pos];
        pos += 1;

        let raw = &data[pos..pos + stride];
        pos += stride;

        let mut row = vec![0u8; stride];

        for i in 0..stride {
            let x = raw[i];
            let a = if i >= bpp { row[i - bpp] } else { 0 }; // left pixel (same row)
            let b = prev[i]; // above pixel (previous row)
            let c = if i >= bpp { prev[i - bpp] } else { 0 }; // upper-left pixel

            row[i] = match filter {
                0 => x,                                                 // None
                1 => x.wrapping_add(a),                                 // Sub
                2 => x.wrapping_add(b),                                 // Up
                3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8), // Average
                4 => x.wrapping_add(paeth(a, b, c)),                    // Paeth
                _ => return None, // unknown filter type — bail rather than guess
            };
        }

        out.extend_from_slice(&row);
        prev = row;
    }

    Some(out)
}

/// PNG's Paeth predictor: picks whichever neighbor (left, above, or
/// upper-left) best predicts the current byte.
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (a as i16, b as i16, c as i16);
    let p = a + b - c;
    let pa = (p - a).abs();
    let pb = (p - b).abs();
    let pc = (p - c).abs();
    if pa <= pb && pa <= pc {
        a as u8
    } else if pb <= pc {
        b as u8
    } else {
        c as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward-encodes pixels using the same logic as the PNG spec.
    /// Only used in tests, to generate valid input for `undo`.
    fn encode(pixels: &[Vec<u8>], bpp: usize, filter: u8) -> Vec<u8> {
        let stride = pixels[0].len();
        let mut out = Vec::new();
        let mut prev = vec![0u8; stride];

        for row in pixels {
            out.push(filter);
            for i in 0..stride {
                let x = row[i];
                let a = if i >= bpp { row[i - bpp] } else { 0 };
                let b = prev[i];
                let c = if i >= bpp { prev[i - bpp] } else { 0 };

                let filtered = match filter {
                    0 => x,
                    1 => x.wrapping_sub(a),
                    2 => x.wrapping_sub(b),
                    3 => x.wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                    4 => x.wrapping_sub(paeth(a, b, c)),
                    _ => panic!("unsupported filter in test helper"),
                };
                out.push(filtered);
            }
            prev = row.clone();
        }
        out
    }

    /// Concatenates a list of per-row pixel byte vectors into one
    /// flat buffer, matching `undo`'s return shape.
    fn flatten(pixels: &[Vec<u8>]) -> Vec<u8> {
        pixels.iter().flatten().copied().collect()
    }

    #[test]
    fn none_filter_roundtrip() {
        let pixels = vec![vec![10, 20], vec![30, 40]];
        let encoded = encode(&pixels, 1, 0);
        let decoded = undo(&encoded, 2, 2, 1).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn sub_filter_roundtrip() {
        let pixels = vec![vec![5, 200, 60], vec![1, 2, 250]];
        let encoded = encode(&pixels, 1, 1);
        let decoded = undo(&encoded, 3, 2, 1).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn up_filter_roundtrip() {
        let pixels = vec![vec![100, 150], vec![90, 5], vec![255, 0]];
        let encoded = encode(&pixels, 1, 2);
        let decoded = undo(&encoded, 2, 3, 1).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn average_filter_roundtrip() {
        let pixels = vec![vec![10, 20, 30], vec![200, 100, 50]];
        let encoded = encode(&pixels, 1, 3);
        let decoded = undo(&encoded, 3, 2, 1).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn paeth_filter_roundtrip() {
        let pixels = vec![vec![10, 20, 30], vec![15, 25, 35], vec![1, 254, 128]];
        let encoded = encode(&pixels, 1, 4);
        let decoded = undo(&encoded, 3, 3, 1).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn rgb_multi_byte_pixels_roundtrip() {
        // bpp = 3 (RGB) — verifies left/upper-left indexing is correct
        // when pixels span multiple bytes.
        let pixels = vec![
            vec![10, 20, 30, 40, 50, 60], // 2 RGB pixels
            vec![70, 80, 90, 100, 110, 120],
        ];
        let encoded = encode(&pixels, 3, 4); // Paeth
        let decoded = undo(&encoded, 2, 2, 3).unwrap();
        assert_eq!(decoded, flatten(&pixels));
    }

    #[test]
    fn rejects_truncated_data() {
        // One byte short of what 2x2 (bpp=1) should require.
        let data = vec![0, 10, 20, 0, 30];
        assert!(undo(&data, 2, 2, 1).is_none());
    }

    #[test]
    fn rejects_unknown_filter_byte() {
        // Filter byte 9 is not defined by the PNG spec (only 0-4).
        let data = vec![9, 10, 20, 0, 30, 40];
        assert!(undo(&data, 2, 2, 1).is_none());
    }

    #[test]
    fn empty_image_returns_empty() {
        let decoded = undo(&[], 0, 0, 1).unwrap();
        assert!(decoded.is_empty());
    }
}
