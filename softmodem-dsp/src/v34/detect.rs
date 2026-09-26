// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finds S and the start of S̄ in front-end symbols, before the equaliser
//! has trained. S repeats every two symbols and S̄ is S turned by 180°,
//! which a linear channel and any timing phase keep.

use super::receiver::Symbol;
use crate::passband::Complex;

// Symbols of S in a row before S counts as heard.
const S_SYMBOLS: usize = 32;
const LIKENESS: f64 = 0.7;
// Neighbours of S differ, so a steady tone is not S.
const SAMENESS: f64 = 0.97;

fn product(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 + a.1 * b.1, a.1 * b.0 - a.0 * b.1)
}

fn magnitude(a: Complex) -> f64 {
    (a.0 * a.0 + a.1 * a.1).sqrt()
}

/// What `SDetector` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    S,
    /// S̄ began at this symbol, counted from the first the detector took.
    SBar(usize),
}

/// Hears S, then the S̄ that follows it.
#[derive(Debug, Default)]
pub struct SDetector {
    history: [Complex; 2],
    count: usize,
    run: usize,
    reversed: usize,
}

impl SDetector {
    /// Takes the next front-end symbol when it is too faint to be the far
    /// end's, as the echo of this end's own S is: it ends any S heard so far.
    pub fn skip(&mut self, symbol: &Symbol) {
        let [_, old] = self.history;
        self.history = [old, symbol.symbol];
        self.count += 1;
        self.run = 0;
        self.reversed = 0;
    }

    /// Takes the next front-end symbol.
    pub fn push(&mut self, symbol: &Symbol) -> Option<Heard> {
        let z = symbol.symbol;
        let [older, old] = self.history;
        self.history = [old, z];
        let n = self.count;
        self.count += 1;
        let (two, one) = (product(z, older), product(z, old));
        let scale = magnitude(z) * magnitude(older);
        if scale <= 0.0 || n < 2 {
            return None;
        }
        let repeat = two.0 / scale;
        let turn = one.0 / (magnitude(z) * magnitude(old)).max(f64::MIN_POSITIVE);
        if self.run >= S_SYMBOLS && repeat < -LIKENESS {
            self.reversed += 1;
            if self.reversed == 2 {
                self.run = 0;
                self.reversed = 0;
                return Some(Heard::SBar(n - 1));
            }
            return None;
        }
        self.reversed = 0;
        if repeat > LIKENESS && turn < SAMENESS {
            self.run += 1;
            if self.run == S_SYMBOLS {
                return Some(Heard::S);
            }
        } else {
            self.run = 0;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heard(points: impl Iterator<Item = Complex>) -> Vec<Heard> {
        let mut detector = SDetector::default();
        points
            .filter_map(|symbol| {
                detector.push(&Symbol {
                    middle: (0.0, 0.0),
                    symbol,
                    at: 0.0,
                })
            })
            .collect()
    }

    fn s_then_s_bar(turn: Complex, s: usize) -> impl Iterator<Item = Complex> {
        let rotate = move |p: Complex| (p.0 * turn.0 - p.1 * turn.1, p.0 * turn.1 + p.1 * turn.0);
        (0..s)
            .map(|n| if n % 2 == 0 { (1.0, 1.0) } else { (-1.0, 1.0) })
            .chain((0..16).map(|n| {
                if n % 2 == 0 {
                    (-1.0, -1.0)
                } else {
                    (1.0, -1.0)
                }
            }))
            .map(rotate)
    }

    #[test]
    fn hears_s_then_where_s_bar_begins_on_any_channel_phase() {
        for turn in [(1.0, 0.0), (0.3, -2.0), (-0.01, 0.02)] {
            assert_eq!(
                heard(s_then_s_bar(turn, 128)),
                [Heard::S, Heard::SBar(128)],
                "phase 3 would train on the wrong symbols"
            );
        }
    }

    #[test]
    fn does_not_take_a_steady_tone_or_its_reversal_for_s() {
        let steady = (0..200).map(|n| if n < 100 { (1.0, 0.5) } else { (-1.0, -0.5) });
        assert!(heard(steady).is_empty());
    }

    #[test]
    fn needs_enough_of_s_first() {
        assert!(heard(s_then_s_bar((1.0, 0.0), 20)).is_empty());
    }
}
