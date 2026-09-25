// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.90: the digital modem, which sends PCM codewords down, and the
//! analogue modem, which receives them and sends V.34 up.

pub mod analogue;
pub mod cp;
pub mod design;
pub mod digital;
pub mod dil;
pub mod downstream;
pub mod encoder;
pub mod fallback;
pub mod frames;
pub mod info;
pub mod jd;
#[cfg(test)]
mod tests;
pub mod training;
pub mod ucode;
pub mod upstream;

use ucode::Law;

// § 9.5.1.2 and § 9.5.2.2: the far tone for more than 50 ms in data mode starts a retrain.
const RETRAIN_TONE: usize = 400;

/// One PCM symbol: a Ucode and its sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Codeword {
    pub ucode: u8,
    pub positive: bool,
}

impl Codeword {
    /// Ucode 0, the codeword nearest to no signal.
    pub const SILENCE: Self = Self {
        ucode: 0,
        positive: true,
    };

    /// The linear sample that the codec turns back into this codeword.
    #[must_use]
    pub fn linear(self, law: Law) -> i16 {
        ucode::signed(self.ucode, self.positive, law)
    }
}
