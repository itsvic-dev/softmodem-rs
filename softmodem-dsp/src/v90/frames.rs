// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The framing of Ja, Jd and CP: 17 ones, then 16-bit words, each after a
//! 0 start bit, and a CRC word over the words, as MP has in V.34.

use crate::v34::bits::{self, Writer};

const SYNC_ONES: usize = 17;
const WORD: usize = 16;

fn word_bits(word: u16) -> impl Iterator<Item = bool> {
    (0..WORD).map(move |n| word >> n & 1 == 1)
}

/// Frame sync, the words, and their CRC, with no fill.
#[must_use]
pub fn frame(words: &[u16]) -> Vec<bool> {
    let crc = bits::crc(words.iter().flat_map(|&word| word_bits(word)));
    let mut writer = Writer::default();
    writer.bits.extend([true; SYNC_ONES]);
    for &word in words.iter().chain([&crc]) {
        writer.flag(false);
        writer.field(word.into(), 16);
    }
    writer.bits
}

/// A sequence carried in words, which says from its first words how many
/// it has.
pub trait Words: Sized {
    /// How many words the sequence has, CRC aside, once `words` are enough
    /// to tell.
    fn length(words: &[u16]) -> Option<usize>;

    fn from_words(words: &[u16]) -> Option<Self>;
}

/// Finds sequences of one kind in received bits, and keeps those whose CRC
/// holds.
#[derive(Debug)]
pub struct Deframer<T> {
    ones: usize,
    block: Vec<bool>,
    words: Option<Vec<u16>>,
    kind: std::marker::PhantomData<T>,
}

impl<T> Default for Deframer<T> {
    fn default() -> Self {
        Self {
            ones: 0,
            block: Vec::new(),
            words: None,
            kind: std::marker::PhantomData,
        }
    }
}

impl<T: Words> Deframer<T> {
    pub fn push(&mut self, bit: bool) -> Option<T> {
        let synced = self.ones >= SYNC_ONES && !bit;
        self.ones = if bit { self.ones + 1 } else { 0 };
        if synced {
            self.words = Some(Vec::new());
            self.block.clear();
        }
        let words = self.words.as_mut()?;
        self.block.push(bit);
        if self.block.len() < WORD + 1 {
            return None;
        }
        let block = std::mem::take(&mut self.block);
        if block[0] {
            self.words = None;
            return None;
        }
        let word = block[1..]
            .iter()
            .rev()
            .fold(0u16, |value, &bit| value << 1 | u16::from(bit));
        words.push(word);
        let length = T::length(words)?;
        if words.len() <= length {
            return None;
        }
        let words = self.words.take()?;
        let crc_ok = bits::crc(words.iter().flat_map(|&word| word_bits(word))) == 0;
        crc_ok.then(|| T::from_words(&words[..length])).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A first word that counts the words after it.
    #[derive(Debug, PartialEq, Eq)]
    struct Counted(Vec<u16>);

    impl Words for Counted {
        fn length(words: &[u16]) -> Option<usize> {
            words.first().map(|&count| usize::from(count) + 1)
        }

        fn from_words(words: &[u16]) -> Option<Self> {
            Some(Self(words.to_vec()))
        }
    }

    fn deframe(bits: impl IntoIterator<Item = bool>) -> Vec<Counted> {
        let mut deframer = Deframer::<Counted>::default();
        bits.into_iter()
            .filter_map(|bit| deframer.push(bit))
            .collect()
    }

    #[test]
    fn places_the_sync_and_the_start_bits() {
        let bits = frame(&[0xFFFF, 0]);
        assert_eq!(bits.len(), 17 + 3 * 17);
        assert!(bits[..17].iter().all(|&bit| bit));
        for n in [17, 34, 51] {
            assert!(!bits[n]);
        }
        assert!(bits[18..34].iter().all(|&bit| bit));
    }

    #[test]
    fn finds_sequences_of_any_length_after_ones() {
        let short = [0];
        let long = [3, 0x1234, 0xFFFF, 0];
        let bits = std::iter::repeat_n(true, 40)
            .chain(frame(&short))
            .chain([false, false])
            .chain(frame(&long))
            .chain(frame(&long));
        assert_eq!(
            deframe(bits),
            [
                Counted(short.to_vec()),
                Counted(long.to_vec()),
                Counted(long.to_vec())
            ]
        );
    }

    #[test]
    fn drops_a_sequence_with_a_flipped_bit() {
        for n in 18..(17 + 5 * 17) {
            let mut bits = frame(&[3, 0x1234, 0xFFFF, 0]);
            bits[n] = !bits[n];
            assert!(deframe(bits).is_empty(), "bit {n}");
        }
    }
}
