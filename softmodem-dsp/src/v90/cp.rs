//! CP of § 8.5.2: the constellations and spectral shaping that the analogue
//! modem asks the digital modem to send with.

use super::frames::{self, Words};
use super::ucode::{COUNT, Law};

// Words before the first constellation, and the words of one.
const HEAD_WORDS: usize = 7;
const MASK_WORDS: usize = 8;
const FILL: usize = 3;

/// A constellation: the Ucodes of its positive points.
pub type Constellation = u128;

/// The Ucodes of `constellation`, from the largest down, as § 5.4.4 labels
/// them.
#[must_use]
pub fn labels(constellation: Constellation) -> Vec<u8> {
    (0..COUNT)
        .rev()
        .filter(|&ucode| constellation >> ucode & 1 == 1)
        .collect()
}

/// A spectral shaping filter parameter, in Q1.6.
pub type Coefficient = i8;

/// Table 14.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cp {
    /// CPt, for phase 4 training, else CP for data mode.
    pub training: bool,
    /// drn: (drn + 20) · 8000/6 bit/s in CP, (drn + 8) · 8000/6 in CPt. 0 clears down.
    pub rate: u8,
    /// CPs, which asks for silence in a rate renegotiation.
    pub silence: bool,
    /// Sr, the sign bits of each data frame that shape the spectrum.
    pub redundancy: u8,
    pub acknowledge: bool,
    pub law: Law,
    /// Bit n for (n + 2) · 2400 bit/s upstream, 4800 to 33 600.
    pub upstream_rates: u16,
    /// ld.
    pub lookahead: u8,
    /// The RMS of TRN1d at the transmitter over that at the D/A, in Q3.13.
    pub rms_ratio: u16,
    /// a1, a2, b1 and b2 of § 5.4.5.6.
    pub filter: [Coefficient; 4],
    /// The constellation of each data frame interval, by index.
    pub intervals: [u8; 6],
    pub constellations: Vec<Constellation>,
    /// The constellations at the D/A, if they differ.
    pub at_codec: Option<Vec<Constellation>>,
}

fn mask_words(constellation: Constellation) -> impl Iterator<Item = u16> {
    (0..MASK_WORDS).map(move |n| u16::try_from(constellation >> (16 * n) & 0xFFFF).unwrap_or(0))
}

fn from_mask_words(words: &[u16]) -> Constellation {
    words
        .iter()
        .rev()
        .fold(0, |value, &word| value << 16 | Constellation::from(word))
}

impl Cp {
    fn words(&self) -> Vec<u16> {
        let first = u16::from(!self.training) << 1
            | u16::from(self.rate & 0x1F) << 2
            | u16::from(self.silence) << 12
            | u16::from(self.redundancy & 3) << 13
            | u16::from(self.acknowledge) << 15;
        let second = u16::from(self.law == Law::A)
            | (self.upstream_rates & 0x1FFF) << 1
            | u16::from(self.lookahead & 3) << 14;
        let byte = |c: Coefficient| u16::from(c.cast_unsigned());
        let index = |n: usize| u16::from(self.intervals[n] & 0xF);
        let mut words = vec![
            first,
            second,
            self.rms_ratio,
            byte(self.filter[0]) | byte(self.filter[1]) << 8,
            byte(self.filter[2]) | byte(self.filter[3]) << 8,
            index(0) | index(1) << 4 | index(2) << 8 | index(3) << 12,
            index(4) | index(5) << 4 | u16::from(self.at_codec.is_some()) << 8,
        ];
        for &constellation in self
            .constellations
            .iter()
            .chain(self.at_codec.iter().flatten())
        {
            words.extend(mask_words(constellation));
        }
        words
    }

    /// Frame sync, the words, the CRC and the fill.
    #[must_use]
    pub fn frame(&self) -> Vec<bool> {
        let mut bits = frames::frame(&self.words());
        bits.extend([false; FILL]);
        bits
    }

    /// D, the bits of each data frame of six symbols.
    #[must_use]
    pub fn frame_bits(&self) -> usize {
        let offset = if self.training { 8 } else { 20 };
        usize::from(self.rate) + offset
    }

    /// The downstream rate, in bit/s, rounded to the nearest.
    #[must_use]
    pub fn bit_rate(&self) -> u32 {
        (u32::try_from(self.frame_bits()).unwrap_or(0) * 8000 + 3) / 6
    }

    /// The constellation of data frame interval `interval`.
    #[must_use]
    pub fn constellation(&self, interval: usize) -> Constellation {
        self.constellations
            .get(usize::from(self.intervals[interval]))
            .copied()
            .unwrap_or(0)
    }
}

impl Words for Cp {
    fn length(words: &[u16]) -> Option<usize> {
        let (&fifth, &sixth) = (words.get(5)?, words.get(6)?);
        let indices = [fifth, fifth >> 4, fifth >> 8, fifth >> 12, sixth, sixth >> 4];
        let count = usize::from(indices.iter().map(|&i| i & 0xF).max().unwrap_or(0)) + 1;
        let copies = if sixth >> 8 & 1 == 1 { 2 } else { 1 };
        Some(HEAD_WORDS + MASK_WORDS * count * copies)
    }

    fn from_words(words: &[u16]) -> Option<Self> {
        let (first, second) = (words[0], words[1]);
        let signed = |word: u16, shift: u16| u8::try_from(word >> shift & 0xFF).unwrap_or(0).cast_signed();
        let nibble = |word: u16, shift: u16| u8::try_from(word >> shift & 0xF).unwrap_or(0);
        let intervals = [
            nibble(words[5], 0),
            nibble(words[5], 4),
            nibble(words[5], 8),
            nibble(words[5], 12),
            nibble(words[6], 0),
            nibble(words[6], 4),
        ];
        let masks: Vec<Constellation> = words[HEAD_WORDS..]
            .chunks(MASK_WORDS)
            .map(from_mask_words)
            .collect();
        let count = usize::from(*intervals.iter().max()?) + 1;
        let (constellations, codec) = masks.split_at(count.min(masks.len()));
        Some(Self {
            training: first >> 1 & 1 == 0,
            rate: u8::try_from(first >> 2 & 0x1F).ok()?,
            silence: first >> 12 & 1 == 1,
            redundancy: u8::try_from(first >> 13 & 3).ok()?,
            acknowledge: first >> 15 == 1,
            law: if second & 1 == 1 { Law::A } else { Law::Mu },
            upstream_rates: second >> 1 & 0x1FFF,
            lookahead: u8::try_from(second >> 14).ok()?,
            rms_ratio: words[2],
            filter: [
                signed(words[3], 0),
                signed(words[3], 8),
                signed(words[4], 0),
                signed(words[4], 8),
            ],
            intervals,
            constellations: constellations.to_vec(),
            at_codec: (!codec.is_empty()).then(|| codec.to_vec()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::frames::Deframer;

    fn cp() -> Cp {
        Cp {
            training: false,
            rate: 22,
            silence: false,
            redundancy: 1,
            acknowledge: true,
            law: Law::A,
            upstream_rates: 0x1FFF,
            lookahead: 1,
            rms_ratio: 1 << 13,
            filter: [-64, 32, 63, 0],
            intervals: [0, 1, 0, 1, 0, 1],
            constellations: vec![(1 << 64) - 1, 0xAAAA << 100 | 1],
            at_codec: None,
        }
    }

    fn deframe(bits: Vec<bool>) -> Vec<Cp> {
        let mut deframer = Deframer::<Cp>::default();
        bits.into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect()
    }

    #[test]
    fn places_the_fields_of_table_14() {
        let frame = cp().frame();
        assert_eq!(frame.len(), 272 + 136 + 20);
        assert!(frame[19]);
        let field = |from: usize, to: usize| {
            (from..=to)
                .rev()
                .fold(0u32, |value, n| value << 1 | u32::from(frame[n]))
        };
        assert_eq!(field(20, 24), 22);
        assert_eq!(field(31, 32), 1);
        assert!(frame[33] && frame[35]);
        assert_eq!(field(36, 48), 0x1FFF);
        assert_eq!(field(49, 50), 1);
        assert_eq!(field(52, 67), 1 << 13);
        assert_eq!(field(69, 76), 0xC0);
        assert_eq!(field(107, 110), 1);
        assert!(!frame[128]);
        assert!(frame[137..153].iter().all(|&bit| bit));
        assert!(frame[154..169].iter().all(|&bit| bit) && !frame[170]);
        assert_eq!(cp().bit_rate(), 56_000);
        let cpt = Cp {
            training: true,
            rate: 15,
            ..cp()
        };
        assert_eq!((cpt.frame_bits(), cpt.bit_rate()), (23, 30_667));
    }

    #[test]
    fn reads_back_what_it_frames() {
        assert_eq!(deframe(cp().frame()), [cp()]);
        let codec = Cp {
            training: true,
            at_codec: Some(vec![1, 2]),
            ..cp()
        };
        assert_eq!(deframe(codec.frame()), [codec]);
    }

    #[test]
    fn labels_from_the_largest_ucode_down() {
        assert_eq!(labels(0b1011), [3, 1, 0]);
        assert_eq!(cp().constellation(1), 0xAAAA << 100 | 1);
    }
}
