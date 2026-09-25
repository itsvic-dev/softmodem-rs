// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Self-synchronising scramblers: 1 + x⁻¹⁴ + x⁻¹⁷ of V.22 § 5, and the two of
//! V.34 § 7.

const LOCKUP_ONES: u32 = 64;

/// A generating polynomial 1 + x⁻ᵃ + x⁻ᵇ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polynomial {
    a: u32,
    b: u32,
}

impl Polynomial {
    pub const V22: Self = Self { a: 14, b: 17 };
    /// From the call modem.
    pub const V34_CALL: Self = Self { a: 18, b: 23 };
    /// From the answer modem.
    pub const V34_ANSWER: Self = Self { a: 5, b: 23 };

    fn taps(self, history: u32) -> bool {
        (history >> (self.a - 1) ^ history >> (self.b - 1)) & 1 == 1
    }

    fn shift(self, history: u32, bit: bool) -> u32 {
        (history << 1 | u32::from(bit)) & ((1 << self.b) - 1)
    }
}

impl Default for Polynomial {
    fn default() -> Self {
        Self::V22
    }
}

#[derive(Debug, Default)]
pub struct Scrambler {
    polynomial: Polynomial,
    history: u32,
    ones: u32,
    guard: bool,
}

impl Scrambler {
    /// V.22's.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(polynomial: Polynomial) -> Self {
        Self {
            polynomial,
            ..Self::default()
        }
    }

    /// From now on, inverts the next input after 64 ones in a row at the
    /// output, so that a run of ones cannot lock the scrambler. V.22 keeps
    /// this off during the handshake.
    pub fn guard_against_lockup(&mut self) {
        self.guard = true;
    }

    pub fn scramble(&mut self, bit: bool) -> bool {
        let bit = if self.guard && self.ones >= LOCKUP_ONES {
            self.ones = 0;
            !bit
        } else {
            bit
        };
        let out = bit ^ self.polynomial.taps(self.history);
        self.history = self.polynomial.shift(self.history, out);
        self.ones = if out { self.ones + 1 } else { 0 };
        out
    }
}

#[derive(Debug, Default)]
pub struct Descrambler {
    polynomial: Polynomial,
    history: u32,
}

impl Descrambler {
    /// V.22's.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(polynomial: Polynomial) -> Self {
        Self {
            polynomial,
            history: 0,
        }
    }

    pub fn descramble(&mut self, bit: bool) -> bool {
        let out = bit ^ self.polynomial.taps(self.history);
        self.history = self.polynomial.shift(self.history, bit);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Vec<bool> {
        (0u32..2000)
            .map(|n| n.wrapping_mul(2_654_435_761) >> 31 == 1)
            .collect()
    }

    #[test]
    fn descrambles_what_it_scrambles() {
        let mut scrambler = Scrambler::new();
        let mut descrambler = Descrambler::new();
        let out: Vec<bool> = data()
            .into_iter()
            .map(|b| descrambler.descramble(scrambler.scramble(b)))
            .collect();
        assert_eq!(out, data());
    }

    #[test]
    fn synchronises_itself_within_17_bits() {
        let mut scrambler = Scrambler::new();
        for bit in data() {
            scrambler.scramble(bit);
        }
        let mut descrambler = Descrambler::new();
        let out: Vec<bool> = data()
            .into_iter()
            .map(|b| descrambler.descramble(scrambler.scramble(b)))
            .collect();
        assert_eq!(out[17..], data()[17..]);
    }

    #[test]
    fn each_v34_polynomial_synchronises_itself_within_23_bits() {
        for polynomial in [Polynomial::V34_CALL, Polynomial::V34_ANSWER] {
            let mut scrambler = Scrambler::with(polynomial);
            for bit in data() {
                scrambler.scramble(bit);
            }
            let mut descrambler = Descrambler::with(polynomial);
            let out: Vec<bool> = data()
                .into_iter()
                .map(|b| descrambler.descramble(scrambler.scramble(b)))
                .collect();
            assert_eq!(out[23..], data()[23..], "{polynomial:?}");
        }
    }

    #[test]
    fn the_v34_directions_scramble_differently() {
        let run = |polynomial| {
            let mut scrambler = Scrambler::with(polynomial);
            (0..100)
                .map(|_| scrambler.scramble(true))
                .collect::<Vec<_>>()
        };
        assert_ne!(run(Polynomial::V34_CALL), run(Polynomial::V34_ANSWER));
    }

    #[test]
    fn divides_by_its_polynomial() {
        let mut scrambler = Scrambler::with(Polynomial::V34_ANSWER);
        let out: Vec<bool> = data().into_iter().map(|b| scrambler.scramble(b)).collect();
        for n in 23..out.len() {
            assert_eq!(out[n], data()[n] ^ out[n - 5] ^ out[n - 23], "bit {n}");
        }
    }

    #[test]
    fn scrambles_idle_mark_into_both_values() {
        let mut scrambler = Scrambler::new();
        let out: Vec<bool> = (0..200).map(|_| scrambler.scramble(true)).collect();
        assert!(out.contains(&true) && out.contains(&false));
    }

    fn longest_run_of_ones(guard: bool) -> usize {
        let mut scrambler = Scrambler::new();
        if guard {
            scrambler.guard_against_lockup();
        }
        let mut run = 0;
        let mut longest = 0;
        for n in 0..300 {
            let bit = if n < 17 {
                !scrambler.polynomial.taps(scrambler.history)
            } else {
                true
            };
            if scrambler.scramble(bit) {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        longest
    }

    #[test]
    fn locks_up_on_ones_without_the_guard() {
        assert_eq!(longest_run_of_ones(false), 300);
    }

    #[test]
    fn the_guard_breaks_a_run_of_64_ones() {
        assert_eq!(longest_run_of_ones(true), 64);
    }
}
