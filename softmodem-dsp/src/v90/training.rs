// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The PCM signals of phases 3 and 4 (§ 8.4 and § 8.6): Sd and S̄d, TRN1d,
//! Jd and Jd′ as signs of UINFO, and R and R̄, with the receiver's
//! inverse of the sign modulation.

use super::Codeword;
use super::encoder::FRAME;
use crate::scrambler::{Descrambler, Polynomial, Scrambler};

/// § 8.4.4: 64 and 8 repetitions of six symbols.
pub const SD_SYMBOLS: usize = 384;
pub const SD_BAR_SYMBOLS: usize = 48;
/// § 9.3.1.4: TRN1d for at least 2040T.
pub const TRN1D_SYMBOLS: usize = 2040;
/// § 8.4.3: Jd′ is 12 zeros.
pub const JD_PRIME_BITS: usize = 12;
/// § 9.4.1.1 and § 8.6.4: Ri for 192T at least, R̄ for 24T.
pub const RI_SYMBOLS: usize = 192;
pub const R_BAR_SYMBOLS: usize = 24;

const SD_SIGNS: [bool; FRAME] = [true, true, true, false, false, false];
const R_SIGNS: [bool; FRAME] = [true, true, true, false, false, false];

/// Sd, or S̄d when `bar`: W, 0, W with the signs of + + + − − −, where W is
/// Ucode 16 + UINFO.
#[must_use]
pub fn sd(uinfo: u8, bar: bool) -> Vec<Codeword> {
    let w = (uinfo + 16).min(127);
    let count = if bar { SD_BAR_SYMBOLS } else { SD_SYMBOLS };
    (0..count)
        .map(|n| Codeword {
            ucode: if n % 3 == 1 { 0 } else { w },
            positive: SD_SIGNS[n % FRAME] != bar,
        })
        .collect()
}

/// R, or R̄ when `bar`, on the Ucode of each data frame interval.
#[must_use]
pub fn r(ucodes: [u8; FRAME], bar: bool, symbols: usize) -> Vec<Codeword> {
    (0..symbols)
        .map(|n| Codeword {
            ucode: ucodes[n % FRAME],
            positive: R_SIGNS[n % FRAME] != bar,
        })
        .collect()
}

/// TRN1d, then Jd and Jd′, as signs of UINFO from the scrambler of the
/// digital modem.
#[derive(Debug)]
pub struct Signs {
    uinfo: u8,
    scrambler: Scrambler,
    last: bool,
}

impl Signs {
    /// For TRN1d, which starts the scrambler at zero.
    #[must_use]
    pub fn new(uinfo: u8) -> Self {
        Self {
            uinfo,
            scrambler: Scrambler::with(Polynomial::V34_CALL),
            last: false,
        }
    }

    /// One symbol of TRN1d: a scrambled one.
    pub fn trn(&mut self) -> Codeword {
        self.last = self.scrambler.scramble(true);
        Codeword {
            ucode: self.uinfo,
            positive: self.last,
        }
    }

    /// The symbols of `bits`, scrambled and differentially encoded from the
    /// last symbol.
    pub fn sequence(&mut self, bits: &[bool]) -> Vec<Codeword> {
        bits.iter()
            .map(|&bit| {
                self.last ^= self.scrambler.scramble(bit);
                Codeword {
                    ucode: self.uinfo,
                    positive: self.last,
                }
            })
            .collect()
    }
}

/// Reads the bits of Jd and Jd′ back from the signs of the symbols.
#[derive(Debug)]
pub struct SignReader {
    descrambler: Descrambler,
    last: bool,
}

impl Default for SignReader {
    fn default() -> Self {
        Self {
            descrambler: Descrambler::with(Polynomial::V34_CALL),
            last: false,
        }
    }
}

impl SignReader {
    /// A symbol of TRN1d, whose sign is the scrambled bit itself. It gives
    /// back the descrambled bit, a one unless the sign was wrong.
    pub fn trn(&mut self, positive: bool) -> bool {
        self.last = positive;
        self.descrambler.descramble(positive)
    }

    /// A symbol of Jd or Jd′, and the bit it carries.
    pub fn push(&mut self, positive: bool) -> bool {
        let bit = positive ^ self.last;
        self.last = positive;
        self.descrambler.descramble(bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::jd::{ALL_RATES, Jd};

    #[test]
    fn builds_sd_from_w_and_ucode_0() {
        let sd = sd(70, false);
        assert_eq!(sd.len(), 384);
        let ucodes: Vec<u8> = sd[..6].iter().map(|c| c.ucode).collect();
        assert_eq!(ucodes, [86, 0, 86, 86, 0, 86]);
        let signs: Vec<bool> = sd[..6].iter().map(|c| c.positive).collect();
        assert_eq!(signs, [true, true, true, false, false, false]);
        let bar = super::sd(70, true);
        assert_eq!(bar.len(), 48);
        assert!(!bar[0].positive && bar[3].positive);
    }

    #[test]
    fn reads_jd_back_after_trn1d() {
        let jd = Jd {
            rates: ALL_RATES,
            sixteen_points: false,
            sixteen_points_renegotiating: false,
            lookahead: 1,
        };
        let mut signs = Signs::new(75);
        let trn: Vec<Codeword> = (0..TRN1D_SYMBOLS).map(|_| signs.trn()).collect();
        let mut symbols = signs.sequence(&jd.frame());
        symbols.extend(signs.sequence(&jd.frame()));
        assert!(trn.iter().chain(&symbols).all(|c| c.ucode == 75));
        let mut reader = SignReader::default();
        let ones = trn.iter().filter(|c| reader.trn(c.positive)).count();
        assert!(ones >= TRN1D_SYMBOLS - 23);
        let mut deframer = crate::v90::frames::Deframer::<Jd>::default();
        let found: Vec<Jd> = symbols
            .iter()
            .filter_map(|c| deframer.push(reader.push(c.positive)))
            .collect();
        assert_eq!(found, [jd, jd]);
    }

    #[test]
    fn trn1d_is_balanced() {
        let mut signs = Signs::new(75);
        let positive = (0..TRN1D_SYMBOLS).filter(|_| signs.trn().positive).count();
        assert!(positive.abs_diff(TRN1D_SYMBOLS / 2) < 100);
    }
}
