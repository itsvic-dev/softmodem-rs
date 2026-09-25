// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The trellis code of § 9.6.3: the subset labels of figure 9, the bits of
//! table 13, and the convolutional encoders of figures 10 to 12.

use super::constellation::Point;
use super::mp::Trellis;

// Figure 9, Im from 3 down and Re from −3 up; it repeats every 8.
const LABELS: [[u8; 4]; 4] = [
    [0b001, 0b110, 0b101, 0b010],
    [0b100, 0b011, 0b000, 0b111],
    [0b101, 0b010, 0b001, 0b110],
    [0b000, 0b111, 0b100, 0b011],
];

// Table 13: Y4 Y3 Y2 Y1, by s(2m) and s(2m + 1).
const INPUTS: [[u8; 8]; 8] = [
    [
        0b0000, 0b0000, 0b0001, 0b0001, 0b1000, 0b1000, 0b1001, 0b1001,
    ],
    [
        0b0011, 0b0010, 0b0010, 0b0011, 0b1011, 0b1010, 0b1010, 0b1011,
    ],
    [
        0b0101, 0b0101, 0b0100, 0b0100, 0b1101, 0b1101, 0b1100, 0b1100,
    ],
    [
        0b0110, 0b0111, 0b0111, 0b0110, 0b1110, 0b1111, 0b1111, 0b1110,
    ],
    [
        0b1000, 0b1000, 0b1001, 0b1001, 0b0000, 0b0000, 0b0001, 0b0001,
    ],
    [
        0b1011, 0b1010, 0b1010, 0b1011, 0b0011, 0b0010, 0b0010, 0b0011,
    ],
    [
        0b1101, 0b1101, 0b1100, 0b1100, 0b0101, 0b0101, 0b0100, 0b0100,
    ],
    [
        0b1110, 0b1111, 0b1111, 0b1110, 0b0110, 0b0111, 0b0111, 0b0110,
    ],
];

/// The subset label s(n) of a channel output point on the odd grid.
#[must_use]
pub fn subset((re, im): Point) -> u8 {
    let column = match re.rem_euclid(8) {
        5 => 0,
        7 => 1,
        1 => 2,
        _ => 3,
    };
    let row = match im.rem_euclid(8) {
        3 => 0,
        1 => 1,
        7 => 2,
        _ => 3,
    };
    LABELS[row][column]
}

/// Y1 to Y4, as bits 0 to 3, from the labels of the two 2D points of a 4D
/// symbol.
#[must_use]
pub fn inputs(first: u8, second: u8) -> u8 {
    INPUTS[usize::from(first & 7)][usize::from(second & 7)]
}

fn bit(value: u8, n: u8) -> bool {
    value >> n & 1 == 1
}

fn pack(bits: &[bool]) -> u8 {
    bits.iter()
        .rev()
        .fold(0, |value, &bit| value << 1 | u8::from(bit))
}

impl Trellis {
    /// How many states the encoder has. A state holds its delays, the
    /// leftmost of its figure in bit 0.
    #[must_use]
    pub fn states(self) -> usize {
        match self {
            Self::States16 => 16,
            Self::States32 => 32,
            Self::States64 => 64,
        }
    }

    /// Y0 in a state: the last delay, so it does not depend on the inputs
    /// that arrive with it.
    #[must_use]
    pub fn output(self, state: u8) -> bool {
        match self {
            Self::States16 => bit(state, 3),
            Self::States32 => bit(state, 4),
            Self::States64 => bit(state, 5),
        }
    }

    /// The state after `state` takes Y1 to Y4 as bits 0 to 3 of `inputs`.
    #[must_use]
    pub fn next(self, state: u8, inputs: u8) -> u8 {
        let s = |n| bit(state, n);
        let (y1, y2, y3, y4) = (
            bit(inputs, 0),
            bit(inputs, 1),
            bit(inputs, 2),
            bit(inputs, 3),
        );
        let y0 = self.output(state);
        match self {
            Self::States16 => pack(&[y0, s(0) ^ y2 ^ y0, s(1) ^ y2, s(2) ^ y1]),
            Self::States32 => pack(&[y0, s(0) ^ y2, s(1) ^ y1, s(2) ^ y4, s(3) ^ y2]),
            Self::States64 => pack(&[
                s(0) ^ s(1) ^ y4 ^ (s(2) & (s(1) ^ y1)),
                s(0) ^ s(1) ^ y3 ^ (y2 & s(2)) ^ s(3),
                s(1) ^ y1 ^ s(2),
                s(2),
                y0,
                s(4) ^ y2 ^ s(2),
            ]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_the_points_of_figure_9() {
        let row = |im| [-3, -1, 1, 3].map(|re| subset((re, im)));
        assert_eq!(row(3), [0b001, 0b110, 0b101, 0b010]);
        assert_eq!(row(1), [0b100, 0b011, 0b000, 0b111]);
        assert_eq!(row(-1), [0b101, 0b010, 0b001, 0b110]);
        assert_eq!(row(-3), [0b000, 0b111, 0b100, 0b011]);
    }

    #[test]
    fn keeps_points_of_one_subset_apart() {
        let grid: Vec<Point> = (-15..=15)
            .step_by(2)
            .flat_map(|re| (-15..=15).step_by(2).map(move |im| (re, im)))
            .collect();
        for &a in &grid {
            for &b in &grid {
                let distance = (a.0 - b.0).pow(2) + (a.1 - b.1).pow(2);
                if a != b && subset(a) == subset(b) {
                    assert!(distance >= 32, "{a:?} and {b:?} would be confused");
                }
            }
        }
    }

    #[test]
    fn reads_table_13() {
        assert_eq!(inputs(0b000, 0b000), 0b0000);
        assert_eq!(inputs(0b011, 0b101), 0b1111);
        assert_eq!(inputs(0b110, 0b100), 0b0101);
        assert_eq!(inputs(0b111, 0b111), 0b0110);
    }

    #[test]
    fn each_encoder_reaches_all_of_its_states() {
        for trellis in [Trellis::States16, Trellis::States32, Trellis::States64] {
            let mut reached = vec![false; trellis.states()];
            let mut frontier = vec![0u8];
            reached[0] = true;
            while let Some(state) = frontier.pop() {
                for inputs in 0..16 {
                    let next = trellis.next(state, inputs);
                    if !reached[usize::from(next)] {
                        reached[usize::from(next)] = true;
                        frontier.push(next);
                    }
                }
            }
            assert!(reached.iter().all(|&r| r), "{trellis:?} would lose states");
        }
    }

    #[test]
    fn ignores_the_inputs_its_rate_does_not_use() {
        for state in 0..16 {
            for inputs in 0..4 {
                let next = Trellis::States16.next(state, inputs);
                for unused in [0b0100, 0b1000, 0b1100] {
                    assert_eq!(Trellis::States16.next(state, inputs | unused), next);
                }
            }
        }
        for state in 0..32 {
            for inputs in 0..16 {
                assert_eq!(
                    Trellis::States32.next(state, inputs),
                    Trellis::States32.next(state, inputs ^ 0b0100)
                );
            }
        }
    }

    #[test]
    fn stays_in_state_0_on_zero_inputs() {
        for trellis in [Trellis::States16, Trellis::States32, Trellis::States64] {
            assert_eq!(trellis.next(0, 0), 0);
            assert!(!trellis.output(0));
        }
    }
}
