// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The data mode encoder of § 9: scrambled bits to the points x′(n) that
//! the modulator sends, eight to a mapping frame.

use super::constellation::{self, Point};
use super::framing::Framing;
use super::mp::{Precoding, Trellis};
use super::precoder::{self, Fixed, Precoder};
use super::shell::ShellMapper;
use super::trellis;
use crate::passband::Complex;

// Table 12, the first half data frame of a superframe leftmost.
const INVERSIONS_7: u16 = 0b01_11_01_11_11_11_10;
const INVERSIONS_8: u16 = 0b0111_0111_1111_1010;

/// What the far receiver asked this transmitter for in its MP, and so what
/// its decoder takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    pub trellis: Trellis,
    /// Θ = 0.3125 rather than 0.
    pub nonlinear: bool,
    pub precoding: Precoding,
}

/// Turns scrambled data bits into the points of data mode.
#[derive(Debug)]
pub struct Encoder {
    framing: Framing,
    shell: ShellMapper,
    settings: Settings,
    energy: f64,
    frame: usize,
    quadrant: u8,
    state: u8,
    precoder: Precoder,
}

impl Encoder {
    /// Starting with the first mapping frame of B1, with every delay at 0
    /// as § 10.1.3.1 asks.
    #[must_use]
    pub fn new(framing: Framing, settings: Settings) -> Self {
        let shell = ShellMapper::new(framing.rings);
        Self {
            energy: average_energy(&framing, &shell),
            precoder: Precoder::new(settings.precoding, framing.precoder_modulo()),
            framing,
            shell,
            settings,
            frame: 0,
            quadrant: 0,
            state: 0,
        }
    }

    /// The mean energy of the points before the non-linear encoder.
    #[must_use]
    pub fn energy(&self) -> f64 {
        self.energy
    }

    /// How many bits the next mapping frame takes: b, or b − 1 in a low one.
    #[must_use]
    pub fn bits(&self) -> usize {
        self.framing.b - usize::from(!self.framing.high(self.frame % self.framing.p))
    }

    /// The eight points x′(n) of the next mapping frame, from `bits()` bits.
    pub fn encode(&mut self, bits: &[bool]) -> [Complex; 8] {
        let (rings, groups) = self.parse(bits);
        let mut out = [(0.0, 0.0); 8];
        for (j, group) in groups.iter().enumerate() {
            let m = 4 * self.frame + j;
            let [first, second] = self.symbol(m, group, [rings[2 * j], rings[2 * j + 1]]);
            out[2 * j] = self.nonlinear(first);
            out[2 * j + 1] = self.nonlinear(second);
        }
        self.frame += 1;
        out
    }

    fn parse(&self, bits: &[bool]) -> ([u8; 8], [Group; 4]) {
        let framing = &self.framing;
        let mut groups = [Group::default(); 4];
        if framing.b <= 12 {
            let with_i3 = bits.len() - 8;
            let mut next = bits.iter().copied();
            for (j, group) in groups.iter_mut().enumerate() {
                group.i1 = next.next().unwrap_or(false);
                group.i2 = next.next().unwrap_or(false);
                group.i3 = j < with_i3 && next.next().unwrap_or(false);
            }
            return ([0; 8], groups);
        }
        let shell_bits = if bits.len() == framing.b {
            framing.k
        } else {
            framing.k - 1
        };
        let r0 = bits[..shell_bits]
            .iter()
            .rev()
            .fold(0u64, |r0, &bit| r0 << 1 | u64::from(bit));
        let mut next = bits[shell_bits..].iter().copied();
        let q = framing.q;
        for group in &mut groups {
            group.i1 = next.next().unwrap_or(false);
            group.i2 = next.next().unwrap_or(false);
            group.i3 = next.next().unwrap_or(false);
            for half in &mut group.q {
                *half = (0..q).fold(0, |value, n| {
                    value | usize::from(next.next().unwrap_or(false)) << n
                });
            }
        }
        (self.shell.map(r0), groups)
    }

    // Steps 1 to 9 of table 11 for 4D symbol m.
    fn symbol(&mut self, m: usize, group: &Group, rings: [u8; 2]) -> [Fixed; 2] {
        let q = self.framing.q;
        self.quadrant = (self.quadrant + u8::from(group.i2) + 2 * u8::from(group.i3)) % 4;
        let label = |half: usize| group.q[half] + (usize::from(rings[half]) << q);
        let u_first = constellation::rotate(constellation::point(label(0)), self.quadrant);

        let c_first = self.precoder.c();
        let y_first = add(u_first, c_first);
        let x_first = self.precoder.push(y_first);

        let c_second = self.precoder.c();
        let c0 = parity(c_first) != parity(c_second);
        let u0 = self.settings.trellis.output(self.state) ^ c0 ^ self.inversion(m);
        let turns = self.quadrant + 2 * u8::from(group.i1) + u8::from(u0);
        let u_second = constellation::rotate(constellation::point(label(1)), turns);
        let y_second = add(u_second, c_second);
        let x_second = self.precoder.push(y_second);

        let inputs = trellis::inputs(trellis::subset(y_first), trellis::subset(y_second));
        self.state = self.settings.trellis.next(self.state, inputs);
        [x_first, x_second]
    }

    // V0(m) of table 12, with B1 as the last data frame of a superframe.
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

    fn nonlinear(&self, x: Fixed) -> Complex {
        let x = precoder::to_complex(x);
        if self.settings.nonlinear {
            precoder::warp(x, self.energy)
        } else {
            x
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Group {
    i1: bool,
    i2: bool,
    i3: bool,
    q: [usize; 2],
}

fn add(u: Point, c: Point) -> Point {
    (u.0 + c.0, u.1 + c.1)
}

// For the modulo encoder: whether c/2 has an odd sum of components.
fn parity(c: Point) -> bool {
    (c.0 / 2 + c.1 / 2).rem_euclid(2) == 1
}

// Rings as the shell mapper spreads evenly spread bits, in the high and the low mapping frames.
#[expect(clippy::cast_precision_loss, reason = "small counts")]
pub(crate) fn average_energy(framing: &Framing, shell: &ShellMapper) -> f64 {
    let per_ring = 1 << framing.q;
    let ring_energy: Vec<f64> = (0..usize::from(framing.rings))
        .map(|ring| {
            (ring * per_ring..(ring + 1) * per_ring)
                .map(|label| {
                    let (re, im) = constellation::point(label);
                    f64::from(re * re + im * im)
                })
                .sum::<f64>()
                / per_ring as f64
        })
        .collect();
    if framing.k == 0 {
        return ring_energy[0];
    }
    let samples = 16_384u64;
    let mean = |shell_bits: usize| {
        let span = 1u64 << shell_bits;
        let total: f64 = (0..samples)
            .map(|n| {
                let r0 = (2 * n + 1) * span / (2 * samples);
                shell
                    .map(r0)
                    .iter()
                    .map(|&ring| ring_energy[usize::from(ring)])
                    .sum::<f64>()
            })
            .sum();
        total / (8 * samples) as f64
    };
    let high = framing.r as f64 / framing.p as f64;
    high * mean(framing.k) + (1.0 - high) * mean(framing.k - 1)
}

#[cfg(test)]
mod tests {
    use super::super::SymbolRate;
    use super::*;

    fn bits_for(encoder: &Encoder, seed: &mut u64) -> Vec<bool> {
        (0..encoder.bits())
            .map(|_| {
                *seed ^= *seed << 13;
                *seed ^= *seed >> 7;
                *seed ^= *seed << 17;
                *seed & 1 == 1
            })
            .collect()
    }

    fn encoder(symbol_rate: SymbolRate, bit_rate: u32, settings: Settings) -> Encoder {
        Encoder::new(
            Framing::new(symbol_rate, bit_rate, false).unwrap(),
            settings,
        )
    }

    #[expect(clippy::cast_possible_truncation, reason = "grid points are small")]
    fn grid(point: Complex) -> Point {
        (point.0 as i32, point.1 as i32)
    }

    #[test]
    fn sends_only_points_of_its_constellation() {
        for (symbol_rate, bit_rate) in [
            (SymbolRate::S2400, 2400),
            (SymbolRate::S3000, 7200),
            (SymbolRate::S3200, 28_800),
            (SymbolRate::S3429, 33_600),
        ] {
            let mut encoder = encoder(symbol_rate, bit_rate, Settings::default());
            let quarter = Framing::new(symbol_rate, bit_rate, false).unwrap().points / 4;
            let mut seed = 1;
            for _ in 0..200 {
                let bits = bits_for(&encoder, &mut seed);
                for point in encoder.encode(&bits) {
                    let (label, _) = constellation::label(grid(point)).unwrap();
                    assert!(
                        label < quarter,
                        "{symbol_rate:?} at {bit_rate} would send a point the far end lacks"
                    );
                }
            }
        }
    }

    #[test]
    fn takes_n_bits_in_each_data_frame() {
        let mut encoder = encoder(SymbolRate::S3000, 19_200, Settings::default());
        let mut seed = 7;
        let taken: usize = (0..15)
            .map(|_| {
                let bits = bits_for(&encoder, &mut seed);
                encoder.encode(&bits);
                bits.len()
            })
            .sum();
        assert_eq!(taken, 19_200 * 28 / 100 / 7);
    }

    #[test]
    fn its_points_have_the_energy_it_reports() {
        for (symbol_rate, bit_rate) in [(SymbolRate::S3200, 26_400), (SymbolRate::S3429, 33_600)] {
            let mut encoder = encoder(symbol_rate, bit_rate, Settings::default());
            let mut seed = 3;
            let mut total = 0.0;
            let frames = 12_000;
            for _ in 0..frames {
                let bits = bits_for(&encoder, &mut seed);
                total += encoder
                    .encode(&bits)
                    .iter()
                    .map(|p| p.0 * p.0 + p.1 * p.1)
                    .sum::<f64>();
            }
            let mean = total / f64::from(8 * frames);
            assert!(
                (mean / encoder.energy() - 1.0).abs() < 0.01,
                "{bit_rate} bit/s would be sent at a level off by more than 1% from what a far receiver expects: {mean:.2} against {:.2}",
                encoder.energy()
            );
        }
    }

    #[test]
    fn precodes_onto_the_modulo_grid() {
        let settings = Settings {
            precoding: [(4096, -2048), (-1024, 512), (300, 200)],
            ..Settings::default()
        };
        let mut plain = encoder(SymbolRate::S3000, 24_000, Settings::default());
        let mut precoded = encoder(SymbolRate::S3000, 24_000, settings);
        let mut seed = 5;
        let mut differs = false;
        for _ in 0..100 {
            let bits = bits_for(&plain, &mut seed);
            let a = plain.encode(&bits);
            let b = precoded.encode(&bits);
            differs |= a != b;
            for point in b {
                assert!(
                    point.0.abs() <= 255.0 && point.1.abs() <= 255.0,
                    "the precoder would leave the bounds of § 9.6.2: {point:?}"
                );
            }
        }
        assert!(differs, "the precoder would do nothing");
    }

    #[test]
    fn inverts_u0_at_each_half_data_frame_as_table_12() {
        let encoder = encoder(SymbolRate::S3000, 19_200, Settings::default());
        let half = 2 * 15;
        let pattern: Vec<bool> = (0..14).map(|h| encoder.inversion(h * half)).collect();
        let expected = [
            true, false, false, true, true, true, false, true, true, true, true, true, true, true,
        ];
        assert_eq!(pattern, expected);
        assert!(!encoder.inversion(1));
    }

    #[test]
    fn grows_large_points_under_theta() {
        let settings = Settings {
            nonlinear: true,
            ..Settings::default()
        };
        let mut plain = encoder(SymbolRate::S3200, 28_800, Settings::default());
        let mut warped = encoder(SymbolRate::S3200, 28_800, settings);
        let mut seed = 9;
        for _ in 0..50 {
            let bits = bits_for(&plain, &mut seed);
            for (a, b) in plain.encode(&bits).into_iter().zip(warped.encode(&bits)) {
                assert!(b.0 * b.0 + b.1 * b.1 > a.0 * a.0 + a.1 * a.1);
            }
        }
    }
}
