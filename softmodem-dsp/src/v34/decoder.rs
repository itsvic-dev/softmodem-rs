// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The data mode decoder: a Viterbi decoder over the trellis of § 9.6.3,
//! then the inverse of the mapper, the differential encoder, the shell
//! mapper and the parser. It takes points with no precoding and Θ = 0, as
//! this modem's receiver asks for.

use std::collections::VecDeque;

use super::constellation::{self, Point};
use super::framing::Framing;
use super::mp::Trellis;
use super::shell::ShellMapper;
use super::trellis;
use crate::passband::Complex;

// Table 12, as the encoder has it.
const INVERSIONS_7: u16 = 0b01_11_01_11_11_11_10;
const INVERSIONS_8: u16 = 0b0111_0111_1111_1010;

/// 4D symbols a decision waits for.
pub const DEPTH: usize = 24;

// Subsets of figure 9 are these points plus four times the checkerboard.
const REPRESENTATIVES: [Point; 8] = [
    (1, 1),
    (1, -1),
    (-1, -1),
    (-1, 1),
    (-3, 1),
    (1, 3),
    (3, -1),
    (-1, -3),
];

fn nearest(point: Complex, label: u8) -> (Point, f64) {
    let base = REPRESENTATIVES[usize::from(label)];
    let (dx, dy) = (
        (point.0 - f64::from(base.0)) / 4.0,
        (point.1 - f64::from(base.1)) / 4.0,
    );
    let (mut i, mut j) = (dx.round(), dy.round());
    if (i + j).rem_euclid(2.0) > 0.5 {
        if (dx - i).abs() > (dy - j).abs() {
            i += (dx - i).signum();
        } else {
            j += (dy - j).signum();
        }
    }
    #[expect(clippy::cast_possible_truncation, reason = "grid points are small")]
    let found = (base.0 + 4 * i as i32, base.1 + 4 * j as i32);
    let distance = (point.0 - f64::from(found.0)).powi(2) + (point.1 - f64::from(found.1)).powi(2);
    (found, distance)
}

#[derive(Debug, Clone, Copy)]
struct Survivor {
    from: u8,
    points: [Point; 2],
}

/// Turns received points back into data bits, `DEPTH` 4D symbols late.
#[derive(Debug)]
pub struct Decoder {
    framing: Framing,
    shell: ShellMapper,
    trellis: Trellis,
    metrics: Vec<f64>,
    survivors: VecDeque<Vec<Survivor>>,
    received: usize,
    decided: Vec<[Point; 2]>,
    frames_out: usize,
    quadrant: u8,
}

impl Decoder {
    /// Starting with the first mapping frame of B1, in state 0.
    #[must_use]
    pub fn new(framing: Framing, trellis: Trellis) -> Self {
        let mut metrics = vec![f64::INFINITY; trellis.states()];
        metrics[0] = 0.0;
        Self {
            shell: ShellMapper::new(framing.rings),
            framing,
            trellis,
            metrics,
            survivors: VecDeque::new(),
            received: 0,
            decided: Vec::new(),
            frames_out: 0,
            quadrant: 0,
        }
    }

    /// Takes the eight points of a mapping frame, in the units of figure 5,
    /// and gives the bits of whole mapping frames decided so far.
    pub fn decode(&mut self, points: [Complex; 8]) -> Vec<bool> {
        let mut bits = Vec::new();
        for &pair in points.as_chunks::<2>().0 {
            self.step(pair);
            if self.survivors.len() > DEPTH {
                let decision = self.trace_back();
                self.decided.push(decision);
                if self.decided.len() == 4 {
                    bits.extend(self.unmap());
                    self.decided.clear();
                }
            }
        }
        bits
    }

    fn inversion(&self, m: usize) -> bool {
        let half = 2 * self.framing.p;
        if !m.is_multiple_of(half) {
            return false;
        }
        let (pattern, length) = if self.framing.j == 8 {
            (INVERSIONS_8, 16)
        } else {
            (INVERSIONS_7, 14)
        };
        let index = (m / half + length - 2) % length;
        pattern >> (length - 1 - index) & 1 == 1
    }

    fn step(&mut self, received: [Complex; 2]) {
        let inversion = self.inversion(self.received);
        self.received += 1;
        let best: [[(Point, f64); 8]; 2] = received.map(|point| {
            let mut best = [((0, 0), 0.0); 8];
            for (label, slot) in (0u8..).zip(&mut best) {
                *slot = nearest(point, label);
            }
            best
        });
        let states = self.trellis.states();
        let mut metrics = vec![f64::INFINITY; states];
        let mut survivors = vec![
            Survivor {
                from: 0,
                points: [(1, 1); 2],
            };
            states
        ];
        for (state, &metric) in (0u8..).zip(&self.metrics) {
            if metric.is_infinite() {
                continue;
            }
            let parity = self.trellis.output(state) ^ inversion;
            for first in 0..8u8 {
                for second in 0..8u8 {
                    if ((first ^ second) & 1 == 1) != parity {
                        continue;
                    }
                    let next =
                        usize::from(self.trellis.next(state, trellis::inputs(first, second)));
                    let (a, b) = (best[0][usize::from(first)], best[1][usize::from(second)]);
                    let candidate = metric + a.1 + b.1;
                    if candidate < metrics[next] {
                        metrics[next] = candidate;
                        survivors[next] = Survivor {
                            from: state,
                            points: [a.0, b.0],
                        };
                    }
                }
            }
        }
        let floor = metrics.iter().copied().fold(f64::INFINITY, f64::min);
        self.metrics = metrics.into_iter().map(|m| m - floor).collect();
        self.survivors.push_back(survivors);
    }

    fn trace_back(&mut self) -> [Point; 2] {
        let mut state = (0u8..)
            .zip(&self.metrics)
            .min_by(|a, b| a.1.total_cmp(b.1))
            .map_or(0, |(state, _)| state);
        let mut decision = [(1, 1); 2];
        for survivors in self.survivors.iter().rev() {
            let survivor = survivors[usize::from(state)];
            decision = survivor.points;
            state = survivor.from;
        }
        self.survivors.pop_front();
        decision
    }

    fn unmap(&mut self) -> Vec<bool> {
        let framing = self.framing;
        let high = framing.high(self.frames_out % framing.p);
        self.frames_out += 1;
        let q = framing.q;
        let mask = (1 << q) - 1;
        let mut rings = [0u8; 8];
        let mut groups = Vec::new();
        for (j, points) in self.decided.clone().into_iter().enumerate() {
            let [(first, first_turns), (second, second_turns)] =
                points.map(|point| constellation::label(point).unwrap_or((0, 0)));
            let i = (4 + first_turns - self.quadrant) % 4;
            self.quadrant = first_turns;
            let i1 = (4 + second_turns - first_turns) % 4 >= 2;
            let ring = |label: usize| {
                u8::try_from(label >> q)
                    .unwrap_or(u8::MAX)
                    .min(framing.rings - 1)
            };
            rings[2 * j] = ring(first);
            rings[2 * j + 1] = ring(second);
            groups.push((i1, i & 1 == 1, i & 2 == 2, [first & mask, second & mask]));
        }
        let mut bits = Vec::new();
        if framing.b <= 12 {
            let with_i3 = framing.b - usize::from(!high) - 8;
            for (j, (i1, i2, i3, _)) in groups.into_iter().enumerate() {
                bits.extend([i1, i2]);
                if j < with_i3 {
                    bits.push(i3);
                }
            }
            return bits;
        }
        let r0 = self.shell.unmap(rings);
        let shell_bits = if high { framing.k } else { framing.k - 1 };
        bits.extend((0..shell_bits).map(|n| r0 >> n & 1 == 1));
        for (i1, i2, i3, halves) in groups {
            bits.extend([i1, i2, i3]);
            for half in halves {
                bits.extend((0..q).map(|n| half >> n & 1 == 1));
            }
        }
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::super::encoder::{Encoder, Settings};
    use super::super::{SymbolRate, rates};
    use super::*;

    #[test]
    fn representatives_have_their_labels() {
        for (label, &point) in (0u8..).zip(&REPRESENTATIVES) {
            assert_eq!(trellis::subset(point), label);
        }
    }

    #[test]
    fn finds_the_nearest_point_of_each_subset() {
        for label in 0..8 {
            for (re, im) in [(0.3, -7.2), (12.9, 4.4), (-20.0, -20.0), (2.0, 2.0)] {
                let (found, distance) = nearest((re, im), label);
                assert_eq!(trellis::subset(found), label);
                for dx in (-12..=12).step_by(2) {
                    for dy in (-12..=12).step_by(2) {
                        let other = (found.0 + dx, found.1 + dy);
                        if trellis::subset(other) == label {
                            let d = (re - f64::from(other.0)).powi(2)
                                + (im - f64::from(other.1)).powi(2);
                            assert!(d >= distance - 1e-9, "{other:?} is nearer than {found:?}");
                        }
                    }
                }
            }
        }
    }

    struct Noise(u64);

    impl Noise {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            f64::from(u32::try_from(self.0 >> 32).unwrap()) / f64::from(u32::MAX) - 0.5
        }

        fn gaussian(&mut self) -> f64 {
            (0..12).map(|_| self.uniform()).sum()
        }

        fn bit(&mut self) -> bool {
            self.uniform() > 0.0
        }
    }

    fn errors(
        symbol_rate: SymbolRate,
        bit_rate: u32,
        trellis: Trellis,
        sigma: f64,
    ) -> (usize, usize) {
        let framing = Framing::new(symbol_rate, bit_rate, false).unwrap();
        let settings = Settings {
            trellis,
            ..Settings::default()
        };
        let mut encoder = Encoder::new(framing, settings);
        let mut decoder = Decoder::new(framing, trellis);
        let mut noise = Noise(0x2545_F491_4F6C_DD1D);
        let mut sent: Vec<bool> = Vec::new();
        let mut received: Vec<bool> = Vec::new();
        for _ in 0..300 {
            let bits: Vec<bool> = (0..encoder.bits()).map(|_| noise.bit()).collect();
            sent.extend(&bits);
            let points = encoder
                .encode(&bits)
                .map(|(re, im)| (re + sigma * noise.gaussian(), im + sigma * noise.gaussian()));
            received.extend(decoder.decode(points));
        }
        let wrong = sent.iter().zip(&received).filter(|(a, b)| a != b).count();
        (wrong, received.len())
    }

    #[test]
    fn decodes_what_the_encoder_sends() {
        for (symbol_rate, bit_rate) in [
            (SymbolRate::S2400, 2400),
            (SymbolRate::S2400, 4800),
            (SymbolRate::S3000, 7200),
            (SymbolRate::S2800, 14_400),
            (SymbolRate::S3200, 28_800),
            (SymbolRate::S3429, 33_600),
        ] {
            for trellis in [Trellis::States16, Trellis::States32, Trellis::States64] {
                let (wrong, decided) = errors(symbol_rate, bit_rate, trellis, 0.0);
                assert!(decided > 0);
                assert_eq!(
                    wrong, 0,
                    "{symbol_rate:?} at {bit_rate} with {trellis:?} would lose data"
                );
            }
        }
    }

    #[test]
    fn decodes_as_well_as_the_rate_choice_assumes() {
        for (symbol_rate, bit_rate) in [(SymbolRate::S3429, 33_600), (SymbolRate::S2400, 9_600)] {
            let framing = Framing::new(symbol_rate, bit_rate, false).unwrap();
            let energy = Encoder::new(framing, Settings::default()).energy();
            let bits = f64::from(bit_rate) / symbol_rate.baud();
            let snr_db = rates::DECODED_DB + rates::DB_PER_BIT * bits;
            let sigma = (energy / 2.0 / 10f64.powf(snr_db / 10.0)).sqrt();
            let (wrong, decided) = (0..4)
                .map(|_| errors(symbol_rate, bit_rate, Trellis::States16, sigma))
                .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
            assert!(
                wrong * 100_000 <= decided,
                "at {snr_db:.1} dB, {bit_rate} bit/s would lose {wrong} of {decided} bits"
            );
        }
    }

    #[test]
    fn corrects_errors_that_slicing_alone_would_make() {
        let sigma = 0.35;
        let (coded, decided) = errors(SymbolRate::S3200, 24_000, Trellis::States16, sigma);
        let mut noise = Noise(7);
        let flips = (0..100_000)
            .filter(|_| {
                let (re, im) = (sigma * noise.gaussian(), sigma * noise.gaussian());
                re.abs() > 1.0 || im.abs() > 1.0
            })
            .count();
        assert!(flips > 500, "the test line would be too clean: {flips}");
        assert!(
            coded * 1000 < decided,
            "the trellis would not correct errors: {coded} of {decided} bits wrong"
        );
    }
}
