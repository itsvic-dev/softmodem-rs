// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The MP sequences of § 10.1.3.9, which carry the data mode parameters in
//! phase 4 and in a rate renegotiation.

use std::collections::VecDeque;

use super::bits::{self, Reader, Writer};
use super::encoder::Settings;

const SYNC_ONES: usize = 17;
const WORD: usize = 16;
const TYPE_0_WORDS: usize = 4;
const TYPE_1_WORDS: usize = 10;

/// § 10.1.3.2: the ones that end MP.
pub const E_ONES: usize = 20;

/// The convolutional encoder of § 9.6.3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trellis {
    #[default]
    States16,
    States32,
    States64,
}

/// h(1) to h(3) of § 9.6.2, real and imaginary, with 14 bits after the
/// binary point.
pub type Precoding = [(i16, i16); 3];

/// What a receiver asks of the far transmitter in its MP, other than the rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Asks {
    pub trellis: Trellis,
    /// Θ = 0.3125, else 0.
    pub nonlinear: bool,
    /// Expanded shaping, else minimum.
    pub expanded_shaping: bool,
    /// All zero for none.
    pub precoding: Precoding,
}

impl Asks {
    /// `mp` with these choices.
    #[must_use]
    pub fn ask(self, mp: Mp) -> Mp {
        Mp {
            trellis: self.trellis,
            nonlinear: self.nonlinear,
            expanded_shaping: self.expanded_shaping,
            precoding: (self.precoding != Precoding::default()).then_some(self.precoding),
            ..mp
        }
    }

    /// What the far transmitter's encoder does, and so what the decoder undoes.
    #[must_use]
    pub fn settings(self) -> Settings {
        Settings {
            trellis: self.trellis,
            nonlinear: self.nonlinear,
            precoding: self.precoding,
        }
    }
}

/// Tables 20 and 21. The choices marked "far" are what this end's receiver
/// asks of the far transmitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one field per flag of tables 20 and 21"
)]
pub struct Mp {
    /// In multiples of 2400 bit/s.
    pub max_call_to_answer: u8,
    pub max_answer_to_call: u8,
    pub auxiliary: bool,
    /// Far.
    pub trellis: Trellis,
    /// Far: Θ = 0.3125, else 0.
    pub nonlinear: bool,
    /// Far: expanded shaping, else minimum.
    pub expanded_shaping: bool,
    /// Set once the far MP has arrived.
    pub acknowledge: bool,
    /// Bit n for (n + 1) · 2400 bit/s, up to 33 600.
    pub rates: u16,
    pub asymmetric: bool,
    /// Far. Type 1 carries it, type 0 leaves the far precoder as it was.
    pub precoding: Option<Precoding>,
}

impl Mp {
    fn words(&self) -> Vec<u16> {
        let trellis = match self.trellis {
            Trellis::States16 => 0,
            Trellis::States32 => 1,
            Trellis::States64 => 2,
        };
        let first = u16::from(self.precoding.is_some())
            | u16::from(self.max_call_to_answer & 0xF) << 2
            | u16::from(self.max_answer_to_call & 0xF) << 6
            | u16::from(self.auxiliary) << 10
            | trellis << 11
            | u16::from(self.nonlinear) << 13
            | u16::from(self.expanded_shaping) << 14
            | u16::from(self.acknowledge) << 15;
        let second = self.rates & 0x3FFF | u16::from(self.asymmetric) << 15;
        let mut words = vec![first, second];
        for (re, im) in self.precoding.iter().flatten() {
            words.extend([re.cast_unsigned(), im.cast_unsigned()]);
        }
        words.push(0);
        words
    }

    /// Frame sync, each word after a 0 start bit, the CRC, and the fill.
    #[must_use]
    pub fn frame(&self) -> Vec<bool> {
        let mut words = self.words();
        words.push(bits::crc(words.iter().flat_map(|&word| word_bits(word))));
        let mut writer = Writer::default();
        writer.bits.extend([true; SYNC_ONES]);
        for word in words {
            writer.flag(false);
            writer.field(word.into(), 16);
        }
        let fill = if self.precoding.is_some() { 1 } else { 3 };
        writer.field(0, fill);
        writer.bits
    }

    fn from_words(words: &[u16]) -> Option<Self> {
        let first = words[0];
        let trellis = match first >> 11 & 3 {
            0 => Trellis::States16,
            1 => Trellis::States32,
            2 => Trellis::States64,
            _ => return None,
        };
        let nibble = |shift: u16| u8::try_from(first >> shift & 0xF).ok();
        let precoding = (first & 1 == 1).then(|| {
            let h = |n: usize| {
                (
                    words[2 + 2 * n].cast_signed(),
                    words[3 + 2 * n].cast_signed(),
                )
            };
            [h(0), h(1), h(2)]
        });
        Some(Self {
            max_call_to_answer: nibble(2)?,
            max_answer_to_call: nibble(6)?,
            auxiliary: first >> 10 & 1 == 1,
            trellis,
            nonlinear: first >> 13 & 1 == 1,
            expanded_shaping: first >> 14 & 1 == 1,
            acknowledge: first >> 15 == 1,
            rates: words[1] & 0x3FFF,
            asymmetric: words[1] >> 15 == 1,
            precoding,
        })
    }
}

fn word_bits(word: u16) -> impl Iterator<Item = bool> {
    (0..WORD).map(move |n| word >> n & 1 == 1)
}

fn words_at_end(bits: &[bool], words: usize) -> Option<Vec<u16>> {
    let length = SYNC_ONES + (words + 1) * (WORD + 1);
    let frame = bits.get(bits.len().checked_sub(length)?..)?;
    let (sync, blocks) = frame.split_at(SYNC_ONES);
    if !sync.iter().all(|&bit| bit) {
        return None;
    }
    let mut found = Vec::new();
    for block in blocks.chunks(WORD + 1) {
        if block[0] {
            return None;
        }
        let mut reader = Reader::new(&block[1..]);
        found.push(u16::try_from(reader.field(WORD)).ok()?);
    }
    let crc_ok = bits::crc(found.iter().flat_map(|&word| word_bits(word))) == 0;
    let crc = found.pop();
    (crc_ok && crc.is_some()).then_some(found)
}

/// Finds MP sequences in descrambled bits.
#[derive(Debug, Default)]
pub struct Deframer {
    window: VecDeque<bool>,
    ones: usize,
}

impl Deframer {
    pub fn push(&mut self, bit: bool) -> Option<Mp> {
        self.ones = if bit { self.ones + 1 } else { 0 };
        self.window.push_back(bit);
        let longest = SYNC_ONES + TYPE_1_WORDS * (WORD + 1);
        if self.window.len() > longest {
            self.window.pop_front();
        }
        let bits = self.window.make_contiguous();
        let mp = [TYPE_0_WORDS, TYPE_1_WORDS].into_iter().find_map(|count| {
            let words = words_at_end(bits, count - 1)?;
            let precoded = words[0] & 1 == 1;
            (precoded == (count == TYPE_1_WORDS))
                .then(|| Mp::from_words(&words))
                .flatten()
        });
        if mp.is_some() {
            self.window.clear();
        }
        mp
    }

    /// Ones in a row up to the last bit. After MP, E is `E_ONES` of them.
    #[must_use]
    pub fn ones(&self) -> usize {
        self.ones
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_0() -> Mp {
        Mp {
            max_call_to_answer: 12,
            max_answer_to_call: 14,
            trellis: Trellis::States64,
            nonlinear: true,
            acknowledge: true,
            rates: 0x3FFF,
            ..Mp::default()
        }
    }

    fn type_1() -> Mp {
        Mp {
            expanded_shaping: true,
            asymmetric: true,
            precoding: Some([(16384, -16384), (-1, 1), (i16::MIN, i16::MAX)]),
            ..type_0()
        }
    }

    fn deframe(bits: impl IntoIterator<Item = bool>) -> Vec<Mp> {
        let mut deframer = Deframer::default();
        bits.into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect()
    }

    #[test]
    fn frames_have_the_lengths_of_tables_20_and_21() {
        assert_eq!(type_0().frame().len(), 88);
        assert_eq!(type_1().frame().len(), 188);
    }

    #[test]
    fn places_the_start_bits() {
        let frame = type_1().frame();
        for n in [17, 34, 51, 68, 85, 102, 119, 136, 153, 170] {
            assert!(!frame[n], "bit {n}");
        }
        assert!(frame[..17].iter().all(|&bit| bit));
    }

    #[test]
    fn places_the_fields_of_table_20() {
        let frame = type_0().frame();
        assert!(!frame[18], "type");
        assert_eq!(frame[20..24], bits::pattern("0011"), "12 · 2400");
        assert_eq!(frame[29..31], bits::pattern("01"), "64 states");
        assert!(frame[31] && !frame[32] && frame[33]);
        assert!(frame[35..49].iter().all(|&bit| bit) && !frame[49]);
    }

    #[test]
    fn finds_each_type_after_the_ones_of_trn() {
        for mp in [type_0(), type_1()] {
            let bits = std::iter::repeat_n(true, 200).chain(mp.frame());
            assert_eq!(deframe(bits), [mp]);
        }
    }

    #[test]
    fn finds_repeated_sequences_of_both_types() {
        let bits = type_0()
            .frame()
            .into_iter()
            .chain(type_1().frame())
            .chain(type_0().frame());
        assert_eq!(deframe(bits), [type_0(), type_1(), type_0()]);
    }

    #[test]
    fn drops_a_sequence_with_a_flipped_bit() {
        for n in 18..171 {
            let mut frame = type_1().frame();
            frame[n] = !frame[n];
            assert!(deframe(frame).is_empty(), "bit {n}");
        }
    }

    #[test]
    fn counts_the_ones_of_e() {
        let mut deframer = Deframer::default();
        for bit in type_0().frame() {
            deframer.push(bit);
        }
        for _ in 0..E_ONES {
            deframer.push(true);
        }
        assert_eq!(deframer.ones(), E_ONES);
    }
}
