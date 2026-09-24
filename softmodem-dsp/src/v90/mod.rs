//! V.90: the digital modem, which sends PCM codewords down, and the
//! analogue modem, which receives them and sends V.34 up.

pub mod analogue;
pub mod cp;
pub mod design;
pub mod digital;
pub mod dil;
pub mod downstream;
pub mod encoder;
pub mod frames;
pub mod info;
pub mod jd;
pub mod training;
pub mod ucode;
pub mod upstream;

use ucode::Law;

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
