//! The training signals of phases 3 and 4 (§ 10.1.3): S and S̄, PP, TRN,
//! and the sequences J, J′, MP and E, as points of unit mean power.

use super::constellation::{self, Point};
use crate::passband::Complex;
use crate::pump::Role;
use crate::scrambler::{Polynomial, Scrambler};

/// Table 18: J asks for a 4-point constellation, or a 16-point one.
pub const J_4: &str = "0000100110010001";
pub const J_16: &str = "0000110110010001";
/// Table 19.
pub const J_PRIME: &str = "1111100110010001";

/// 6 periods of the 48-symbol sequence of § 10.1.3.6.
pub const PP_SYMBOLS: usize = 288;

fn unit(point: Point, energy: f64) -> Complex {
    let scale = energy.sqrt().recip();
    (f64::from(point.0) * scale, f64::from(point.1) * scale)
}

/// Turned counterclockwise, as § 10.1.3.7 turns the points of S.
fn counterclockwise(point: Point, quarter_turns: u8) -> Point {
    constellation::rotate(point, (4 - quarter_turns % 4) % 4)
}

/// Symbol `n` of S: point 0, then point 0 turned 90° counterclockwise.
#[must_use]
pub fn s(n: usize) -> Complex {
    unit(counterclockwise((1, 1), u8::from(n % 2 == 1)), 2.0)
}

/// Symbol `n` of S̄: S turned by 180°.
#[must_use]
pub fn s_bar(n: usize) -> Complex {
    unit(counterclockwise((1, 1), 2 + u8::from(n % 2 == 1)), 2.0)
}

/// Symbol `i` of PP, below `PP_SYMBOLS`.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "small indices")]
pub fn pp(i: usize) -> Complex {
    let (k, l) = (i / 4 % 72, i % 4);
    let turns = if k % 3 == 1 { k * l + 4 } else { k * l };
    let angle = std::f64::consts::PI * turns as f64 / 6.0;
    (angle.cos(), angle.sin())
}

/// How many points TRN, MP and E use in phase 4, as J asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Points {
    Four,
    Sixteen,
}

/// Sends TRN, then J, J′, MP and E, through the scrambler of the end in
/// `role`, as § 10.1.3.3 to § 10.1.3.9 build them.
#[derive(Debug)]
pub struct Sender {
    scrambler: Scrambler,
    quadrant: u8,
}

impl Sender {
    /// For TRN, which starts the scrambler at 0.
    #[must_use]
    pub fn new(role: Role) -> Self {
        let polynomial = match role {
            Role::Originate => Polynomial::V34_CALL,
            Role::Answer => Polynomial::V34_ANSWER,
        };
        Self {
            scrambler: Scrambler::with(polynomial),
            quadrant: 0,
        }
    }

    fn two_bits(&mut self, first: bool, second: bool) -> u8 {
        let i1 = self.scrambler.scramble(first);
        let i2 = self.scrambler.scramble(second);
        u8::from(i1) + 2 * u8::from(i2)
    }

    /// One symbol of TRN: scrambled ones, 4 or 16 points.
    pub fn trn(&mut self, points: Points) -> Complex {
        let turns = self.two_bits(true, true);
        self.quadrant = turns;
        match points {
            Points::Four => unit(constellation::rotate((1, 1), turns), 2.0),
            Points::Sixteen => {
                let q = self.two_bits(true, true);
                let label = usize::from(q);
                unit(
                    constellation::rotate(constellation::point(label), turns),
                    10.0,
                )
            }
        }
    }

    /// The symbols for `bits`: J, J′, MP or E, differentially encoded from
    /// the last TRN symbol.
    pub fn sequence(&mut self, bits: &[bool], points: Points) -> Vec<Complex> {
        let per_symbol = match points {
            Points::Four => 2,
            Points::Sixteen => 4,
        };
        bits.chunks(per_symbol)
            .map(|chunk| {
                let bit = |n: usize| chunk.get(n).copied().unwrap_or(true);
                let turns = self.two_bits(bit(0), bit(1));
                self.quadrant = (self.quadrant + turns) % 4;
                match points {
                    Points::Four => unit(constellation::rotate((1, 1), self.quadrant), 2.0),
                    Points::Sixteen => {
                        let q = self.two_bits(bit(2), bit(3));
                        let point = constellation::point(usize::from(q));
                        unit(constellation::rotate(point, self.quadrant), 10.0)
                    }
                }
            })
            .collect()
    }
}

/// Bits of a pattern of tables 18 and 19, left first.
#[must_use]
pub fn pattern(pattern: &str) -> Vec<bool> {
    pattern.chars().map(|c| c == '1').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power(points: impl Iterator<Item = Complex>) -> f64 {
        let (sum, count) = points.fold((0.0, 0.0), |(sum, count), p| {
            (sum + p.0 * p.0 + p.1 * p.1, count + 1.0)
        });
        sum / count
    }

    #[test]
    fn s_ends_as_s_bar_begins() {
        assert_eq!(s(127), unit((-1, 1), 2.0));
        assert_eq!(s_bar(0), unit((-1, -1), 2.0));
        assert_eq!(s(0), unit((1, 1), 2.0));
        assert_eq!(s_bar(1), unit((1, -1), 2.0));
    }

    #[test]
    fn pp_has_unit_power_and_a_period_of_48() {
        for i in 0..PP_SYMBOLS - 48 {
            let (a, b) = (pp(i), pp(i + 48));
            assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9);
            assert!((a.0 * a.0 + a.1 * a.1 - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn pp_is_flat_across_its_period() {
        for shift in 1..48 {
            let (re, im) = (0..48).fold((0.0, 0.0), |(re, im), i| {
                let (a, b) = (pp(i), pp((i + shift) % 48));
                (re + a.0 * b.0 + a.1 * b.1, im + a.1 * b.0 - a.0 * b.1)
            });
            assert!(
                (re * re + im * im).sqrt() < 1e-9,
                "PP would not train an equaliser evenly: correlation at {shift}"
            );
        }
    }

    #[test]
    fn every_signal_has_unit_mean_power() {
        let mut sender = Sender::new(Role::Answer);
        assert!((power((0..2000).map(|_| sender.trn(Points::Four))) - 1.0).abs() < 1e-9);
        let sixteen = power((0..20_000).map(|_| sender.trn(Points::Sixteen)));
        assert!((sixteen - 1.0).abs() < 0.03);
        assert!((power((0..128).map(s)) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn trn_starts_from_a_scrambler_at_zero() {
        let mut sender = Sender::new(Role::Originate);
        let first = sender.trn(Points::Four);
        let mut scrambler = Scrambler::with(Polynomial::V34_CALL);
        let turns = u8::from(scrambler.scramble(true)) + 2 * u8::from(scrambler.scramble(true));
        assert_eq!(first, unit(constellation::rotate((1, 1), turns), 2.0));
    }

    #[test]
    fn sends_j_as_two_bits_a_symbol() {
        let mut sender = Sender::new(Role::Answer);
        assert_eq!(sender.sequence(&pattern(J_4), Points::Four).len(), 8);
        assert_eq!(sender.sequence(&pattern(J_4), Points::Sixteen).len(), 4);
    }
}
