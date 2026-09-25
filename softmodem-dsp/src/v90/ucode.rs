// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The universal codes of table 1: one number for each positive PCM
//! codeword, in the order of its magnitude, for A-law and µ-law alike.

/// The Ucodes run from 0 to 127.
pub const COUNT: u8 = 128;

/// The PCM coding that the digital modem uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Law {
    Mu,
    #[default]
    A,
}

fn segment_and_step(ucode: u8) -> (u32, u32) {
    assert!(ucode < COUNT, "Ucode {ucode} is out of table 1");
    (u32::from(ucode >> 4), u32::from(ucode & 0x0F))
}

/// The linear value of the positive codeword of `ucode`.
///
/// # Panics
///
/// If `ucode` is not below [`COUNT`].
#[must_use]
pub fn linear(ucode: u8, law: Law) -> i16 {
    let (segment, step) = segment_and_step(ucode);
    let value = match law {
        Law::A if segment == 0 => (2 * step + 1) * 8,
        Law::A => (2 * step + 33) << (segment + 2),
        Law::Mu => (((2 * step + 33) << segment) - 33) * 4,
    };
    i16::try_from(value).unwrap_or(i16::MAX)
}

/// The signed linear value of `ucode`, positive or negative.
///
/// # Panics
///
/// If `ucode` is not below [`COUNT`].
#[must_use]
pub fn signed(ucode: u8, positive: bool, law: Law) -> i16 {
    let value = linear(ucode, law);
    if positive { value } else { -value }
}

/// The octet of the positive codeword of `ucode`.
///
/// # Panics
///
/// If `ucode` is not below [`COUNT`].
#[must_use]
pub fn octet(ucode: u8, law: Law) -> u8 {
    let (segment, step) = segment_and_step(ucode);
    match law {
        Law::A => 0x80 | (u8::try_from(segment << 4 | step).unwrap_or(0) ^ 0x55),
        Law::Mu => 0xFF - ucode,
    }
}

/// The Ucode whose magnitude is nearest `magnitude`.
#[must_use]
pub fn nearest(magnitude: f64, law: Law) -> u8 {
    let distance = |u: u8| (f64::from(linear(u, law)) - magnitude).abs();
    (0..COUNT)
        .min_by(|&a, &b| distance(a).total_cmp(&distance(b)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gives_the_linear_values_of_table_1() {
        for (ucode, mu, a) in [
            (0, 0, 8),
            (1, 8, 24),
            (15, 120, 248),
            (16, 132, 264),
            (31, 372, 504),
            (32, 396, 528),
            (47, 876, 1008),
            (63, 1884, 2016),
            (64, 1980, 2112),
            (66, 2236, 2368),
            (100, 10364, 10496),
            (112, 16764, 16896),
            (127, 32124, 32256),
        ] {
            assert_eq!(
                (ucode, linear(ucode, Law::Mu), linear(ucode, Law::A)),
                (ucode, mu, a)
            );
        }
    }

    #[test]
    fn gives_the_octets_of_table_1() {
        for (ucode, mu, a) in [
            (0, 0xFF, 0xD5),
            (3, 0xFC, 0xD6),
            (16, 0xEF, 0xC5),
            (42, 0xD5, 0xFF),
            (64, 0xBF, 0x95),
            (101, 0x9A, 0xB0),
            (127, 0x80, 0xAA),
        ] {
            assert_eq!(
                (ucode, octet(ucode, Law::Mu), octet(ucode, Law::A)),
                (ucode, mu, a)
            );
        }
    }

    #[test]
    fn rises_with_the_ucode() {
        for law in [Law::A, Law::Mu] {
            for ucode in 1..COUNT {
                assert!(linear(ucode, law) > linear(ucode - 1, law));
            }
        }
    }

    #[test]
    fn finds_the_nearest_ucode() {
        assert_eq!(nearest(0.0, Law::A), 0);
        assert_eq!(nearest(2370.0, Law::A), 66);
        assert_eq!(nearest(40_000.0, Law::A), 127);
    }
}
