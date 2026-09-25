// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The digital impairment learning sequence of § 8.4.1, and the DIL
//! descriptor of § 8.3.1 that the analogue modem repeats in Ja to ask for
//! it.

use super::Codeword;
use super::frames::{self, Words};

const WORD: usize = 16;
const UCHORDS: usize = 8;

/// Table 12.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Descriptor {
    /// SP: the sign of each symbol of a segment, 1 positive. 1 to 128 bits.
    pub signs: Vec<bool>,
    /// TP: 1 for the training symbol, 0 for the reference. 1 to 128 bits.
    pub pattern: Vec<bool>,
    /// H1 to H8: a segment of training symbols from Uchord c is
    /// (Hc + 1) · 6 symbols long.
    pub lengths: [u8; UCHORDS],
    /// REF1 to REF8: the reference Ucode of each Uchord.
    pub references: [u8; UCHORDS],
    /// The training Ucode of each segment, at most 255 of them.
    pub training: Vec<u8>,
}

impl Descriptor {
    /// A descriptor that asks for no DIL.
    #[must_use]
    pub fn none() -> Self {
        Self {
            signs: vec![false],
            pattern: vec![false],
            lengths: [0; UCHORDS],
            references: [0; UCHORDS],
            training: Vec::new(),
        }
    }

    fn pairs(values: &[u8]) -> impl Iterator<Item = u16> {
        values.chunks(2).map(|pair| {
            u16::from(pair[0] & 0x7F) | u16::from(pair.get(1).map_or(0, |v| v & 0x7F)) << 8
        })
    }

    fn pattern_words(bits: &[bool]) -> impl Iterator<Item = u16> {
        bits.chunks(WORD).map(|chunk| {
            chunk
                .iter()
                .enumerate()
                .fold(0, |word, (n, &bit)| word | u16::from(bit) << n)
        })
    }

    fn words(&self) -> Vec<u16> {
        let count = u16::try_from(self.training.len()).unwrap_or(255).min(255);
        let length = |bits: &[bool]| u16::try_from(bits.len().clamp(1, 128) - 1).unwrap_or(0);
        let mut words = vec![count, length(&self.signs) | length(&self.pattern) << 8];
        words.extend(Self::pattern_words(&self.signs));
        words.extend(Self::pattern_words(&self.pattern));
        words.extend(Self::pairs(&self.lengths));
        words.extend(Self::pairs(&self.references));
        words.extend(Self::pairs(&self.training));
        words
    }

    /// Frame sync, the words, the CRC and the fill, to an even length.
    #[must_use]
    pub fn frame(&self) -> Vec<bool> {
        let mut bits = frames::frame(&self.words());
        bits.push(false);
        if bits.len() % 2 == 1 {
            bits.push(false);
        }
        bits
    }
}

fn pattern_lengths(words: &[u16]) -> Option<(usize, usize)> {
    let lengths = *words.get(1)?;
    Some((
        usize::from(lengths & 0x7F) + 1,
        usize::from(lengths >> 8 & 0x7F) + 1,
    ))
}

impl Words for Descriptor {
    fn length(words: &[u16]) -> Option<usize> {
        let count = usize::from(*words.first()? & 0xFF);
        let (signs, pattern) = pattern_lengths(words)?;
        let lengths_and_references = UCHORDS;
        Some(
            2 + signs.div_ceil(WORD)
                + pattern.div_ceil(WORD)
                + lengths_and_references
                + count.div_ceil(2),
        )
    }

    fn from_words(words: &[u16]) -> Option<Self> {
        let count = usize::from(words[0] & 0xFF);
        let (signs, pattern) = pattern_lengths(words)?;
        let mut rest = &words[2..];
        let mut take_bits = |length: usize| {
            let (taken, left) = rest.split_at(length.div_ceil(WORD));
            rest = left;
            (0..length)
                .map(|n| taken[n / WORD] >> (n % WORD) & 1 == 1)
                .collect::<Vec<bool>>()
        };
        let signs = take_bits(signs);
        let pattern = take_bits(pattern);
        let values: Vec<u8> = rest
            .iter()
            .flat_map(|&word| [word & 0x7F, word >> 8 & 0x7F])
            .map(|value| u8::try_from(value).unwrap_or(0))
            .collect();
        let mut lengths = [0; UCHORDS];
        let mut references = [0; UCHORDS];
        lengths.copy_from_slice(&values[..UCHORDS]);
        references.copy_from_slice(&values[UCHORDS..2 * UCHORDS]);
        Some(Self {
            signs,
            pattern,
            lengths,
            references,
            training: values[2 * UCHORDS..2 * UCHORDS + count].to_vec(),
        })
    }
}

/// Sends the DIL that a descriptor asks for, all its segments over and
/// over.
#[derive(Debug)]
pub struct Dil {
    descriptor: Descriptor,
    segment: usize,
    symbol: usize,
}

impl Dil {
    #[must_use]
    pub fn new(descriptor: Descriptor) -> Self {
        Self {
            descriptor,
            segment: 0,
            symbol: 0,
        }
    }

    /// Whether the descriptor asks for no DIL.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.descriptor.training.is_empty()
    }

    /// Whether the next symbol starts a segment, where DIL may end.
    #[must_use]
    pub fn at_boundary(&self) -> bool {
        self.symbol == 0
    }

    fn uchord(ucode: u8) -> usize {
        usize::from(ucode / 16).min(UCHORDS - 1)
    }
}

impl Iterator for Dil {
    type Item = Codeword;

    fn next(&mut self) -> Option<Codeword> {
        let d = &self.descriptor;
        let &training = d.training.get(self.segment)?;
        let uchord = Self::uchord(training);
        let length = (usize::from(d.lengths[uchord]) + 1) * 6;
        let k = self.symbol;
        let positive = d.signs[k % d.signs.len()];
        let ucode = if d.pattern[k % d.pattern.len()] {
            training
        } else {
            d.references[uchord]
        };
        self.symbol += 1;
        if self.symbol == length {
            self.symbol = 0;
            self.segment = (self.segment + 1) % d.training.len();
        }
        Some(Codeword { ucode, positive })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::frames::Deframer;

    fn descriptor() -> Descriptor {
        Descriptor {
            signs: (0..20).map(|n| n % 3 == 0).collect(),
            pattern: vec![false, true, true],
            lengths: [0, 1, 2, 3, 4, 5, 6, 127],
            references: [15, 20, 40, 50, 70, 90, 100, 120],
            training: vec![3, 17, 127],
        }
    }

    #[test]
    fn has_the_length_of_table_12() {
        let frame = descriptor().frame();
        let alpha = 2 * 17;
        let beta = alpha + 17;
        assert_eq!(frame.len(), 204 + beta + 2 * 17 + 1);
        assert!(!frame[51] && !frame[51 + alpha] && !frame[51 + beta]);
    }

    #[test]
    fn reads_back_what_it_frames() {
        let mut deframer = Deframer::<Descriptor>::default();
        let found: Vec<Descriptor> = [descriptor().frame(), Descriptor::none().frame()]
            .concat()
            .into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect();
        assert_eq!(found, [descriptor(), Descriptor::none()]);
    }

    #[test]
    fn sends_each_segment_with_its_reference_and_length() {
        let mut dil = Dil::new(descriptor());
        let first: Vec<Codeword> = dil.by_ref().take(6).collect();
        assert_eq!(
            first.iter().map(|c| c.ucode).collect::<Vec<_>>(),
            [15, 3, 3, 15, 3, 3]
        );
        assert_eq!(
            first.iter().map(|c| c.positive).collect::<Vec<_>>(),
            [true, false, false, true, false, false]
        );
        assert!(dil.at_boundary());
        let second: Vec<Codeword> = dil.by_ref().take(12).collect();
        assert_eq!((second[0].ucode, second[1].ucode), (20, 17));
        assert!(dil.at_boundary());
        assert_eq!(dil.by_ref().take(128 * 6).count(), 128 * 6);
        assert!(dil.at_boundary());
        assert_eq!(dil.next().map(|c| c.ucode), Some(15));
        assert_eq!(Dil::new(Descriptor::none()).next(), None);
    }
}
