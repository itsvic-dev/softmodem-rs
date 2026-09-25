// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Jd of § 8.4.2, which the digital modem repeats at the end of phase 3.

use super::frames::{self, Words};

/// Bit n for (n + 21) · 8000/6 bit/s, 28 000 to 56 000.
pub const ALL_RATES: u32 = (1 << 22) - 1;
// Table 13 ends Jd with four zeros.
const FILL: usize = 4;

/// Table 13.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Jd {
    /// The downstream rates its transmitter supports, bit 0 for 28 000 bit/s.
    pub rates: u32,
    /// CP, E and SCR on 16 points in phase 4, else 4.
    pub sixteen_points: bool,
    /// The same in a rate renegotiation.
    pub sixteen_points_renegotiating: bool,
    /// The most look-ahead frames its spectral shaper can use, 1 to 3.
    pub lookahead: u8,
}

impl Jd {
    fn words(self) -> [u16; 2] {
        let low = u16::try_from(self.rates & 0xFFFF).unwrap_or(0);
        let high = u16::try_from(self.rates >> 16 & 0x3F).unwrap_or(0)
            | u16::from(self.sixteen_points) << 12
            | u16::from(self.sixteen_points_renegotiating) << 13
            | u16::from(self.lookahead & 3) << 14;
        [low, high]
    }

    /// Frame sync, the words, the CRC and the fill: 72 bits.
    #[must_use]
    pub fn frame(self) -> Vec<bool> {
        let mut bits = frames::frame(&self.words());
        bits.extend([false; FILL]);
        bits
    }
}

impl Words for Jd {
    fn length(_words: &[u16]) -> Option<usize> {
        Some(2)
    }

    fn from_words(words: &[u16]) -> Option<Self> {
        let (low, high) = (words[0], words[1]);
        Some(Self {
            rates: u32::from(low) | u32::from(high & 0x3F) << 16,
            sixteen_points: high >> 12 & 1 == 1,
            sixteen_points_renegotiating: high >> 13 & 1 == 1,
            lookahead: u8::try_from(high >> 14).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::frames::Deframer;

    fn jd() -> Jd {
        Jd {
            rates: ALL_RATES,
            sixteen_points: false,
            sixteen_points_renegotiating: true,
            lookahead: 3,
        }
    }

    #[test]
    fn places_the_fields_of_table_13() {
        let frame = jd().frame();
        assert_eq!(frame.len(), 72);
        assert!(frame[18..34].iter().all(|&bit| bit));
        assert!(frame[35..41].iter().all(|&bit| bit));
        assert!(frame[41..47].iter().all(|&bit| !bit));
        assert!(!frame[47] && frame[48] && frame[49] && frame[50]);
        assert!(!frame[34] && !frame[51]);
        assert!(frame[68..].iter().all(|&bit| !bit));
    }

    #[test]
    fn reads_back_what_it_frames() {
        let mut deframer = Deframer::<Jd>::default();
        let found: Vec<Jd> = jd()
            .frame()
            .into_iter()
            .chain(jd().frame())
            .filter_map(|bit| deframer.push(bit))
            .collect();
        assert_eq!(found, [jd(), jd()]);
    }
}
