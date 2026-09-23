//! The self-synchronising scrambler of V.22 § 5: 1 + x⁻¹⁴ + x⁻¹⁷.

const TAPS: (u32, u32) = (13, 16);
const HISTORY: u32 = (1 << 17) - 1;
const LOCKUP_ONES: u32 = 64;

fn taps(history: u32) -> bool {
    (history >> TAPS.0 ^ history >> TAPS.1) & 1 == 1
}

fn shift(history: u32, bit: bool) -> u32 {
    (history << 1 | u32::from(bit)) & HISTORY
}

#[derive(Debug, Default)]
pub struct Scrambler {
    history: u32,
    ones: u32,
    guard: bool,
}

impl Scrambler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let out = bit ^ taps(self.history);
        self.history = shift(self.history, out);
        self.ones = if out { self.ones + 1 } else { 0 };
        out
    }
}

#[derive(Debug, Default)]
pub struct Descrambler {
    history: u32,
}

impl Descrambler {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn descramble(&mut self, bit: bool) -> bool {
        let out = bit ^ taps(self.history);
        self.history = shift(self.history, bit);
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
                !taps(scrambler.history)
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
