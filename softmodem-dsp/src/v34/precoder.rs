// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The precoder of § 9.6.2 and the non-linear encoder of § 9.7, for the
//! transmitter, and their inverses, for the receiver.

use super::constellation::Point;
use super::mp::Precoding;
use crate::passband::Complex;

// x(n) and p(n) are multiples of 2⁻⁷, and h(p) has 14 bits after the point.
const FRACTION: u32 = 7;
const COEFFICIENT_FRACTION: u32 = 14;

// § 9.7.
const THETA: f64 = 0.3125;

const UNWARP_ROUNDS: usize = 8;

/// A complex multiple of 2⁻⁷.
pub type Fixed = (i64, i64);

// § 9.6.2 rounds halves towards the smaller magnitude.
pub(crate) fn round_div(value: i64, divisor: i64) -> i64 {
    let magnitude = (value.abs() + divisor / 2 - 1) / divisor;
    if value < 0 { -magnitude } else { magnitude }
}

/// Works out c(n) and p(n) from the last three x(n), exactly as § 9.6.2.
#[derive(Debug, Clone)]
pub struct Precoder {
    coefficients: Precoding,
    modulo: i64,
    history: [Fixed; 3],
    c: Point,
    p: Fixed,
}

impl Precoder {
    /// With every x(n) before the first at 0. `modulo` is 2w.
    #[must_use]
    pub fn new(coefficients: Precoding, modulo: i64) -> Self {
        Self {
            coefficients,
            modulo,
            history: [(0, 0); 3],
            c: (0, 0),
            p: (0, 0),
        }
    }

    /// c(n), which makes the channel output y(n) = u(n) + c(n).
    #[must_use]
    pub fn c(&self) -> Point {
        self.c
    }

    /// Takes y(n), and gives x(n) = y(n) − p(n) after working out c(n + 1) and p(n + 1) from it.
    pub fn push(&mut self, y: Point) -> Fixed {
        let x = (
            (i64::from(y.0) << FRACTION) - self.p.0,
            (i64::from(y.1) << FRACTION) - self.p.1,
        );
        self.history = [x, self.history[0], self.history[1]];
        let mut sum = (0i64, 0i64);
        for ((xr, xi), (hr, hi)) in self.history.iter().zip(self.coefficients) {
            let (hr, hi) = (i64::from(hr), i64::from(hi));
            sum.0 += xr * hr - xi * hi;
            sum.1 += xr * hi + xi * hr;
        }
        let divisor = 1 << COEFFICIENT_FRACTION;
        self.p = (round_div(sum.0, divisor), round_div(sum.1, divisor));
        let step = self.modulo << FRACTION;
        let c = |value: i64| i32::try_from(round_div(value, step) * self.modulo).unwrap_or(0);
        self.c = (c(self.p.0), c(self.p.1));
        x
    }
}

/// x(n) in the units of figure 5.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "x(n) stays far below 2⁵²")]
pub fn to_complex((re, im): Fixed) -> Complex {
    let scale = f64::from(1 << FRACTION);
    (re as f64 / scale, im as f64 / scale)
}

/// x′(n) = Φ(n) x(n), for points of mean energy `energy`.
#[must_use]
pub fn warp(x: Complex, energy: f64) -> Complex {
    let phi = phi(x.0 * x.0 + x.1 * x.1, energy);
    (x.0 * phi, x.1 * phi)
}

/// The x(n) that `warp` takes to `z`.
#[must_use]
pub fn unwarp(z: Complex, energy: f64) -> Complex {
    let magnitude = z.0.hypot(z.1);
    if magnitude == 0.0 {
        return z;
    }
    // Newton's method on r Φ(r²) = |z|, which rises and bends up, so it closes in from above.
    let a = THETA / energy;
    let mut r = magnitude;
    for _ in 0..UNWARP_ROUNDS {
        let r2 = r * r;
        let f = r * phi(r2, energy) - magnitude;
        let slope = 1.0 + a * r2 / 2.0 + a * a * r2 * r2 / 24.0;
        r -= f / slope;
    }
    (z.0 * r / magnitude, z.1 * r / magnitude)
}

fn phi(power: f64, energy: f64) -> f64 {
    let zeta = THETA * power / energy;
    1.0 + zeta / 6.0 + zeta * zeta / 120.0
}

/// The receiver's side of the precoder: from what the equalizer gives to
/// the channel output y(n) that the trellis decoder reads.
///
/// The far precoder took p(n) away from y(n), so adding the same filter of
/// the past points back gives y(n) with the noise whitened, as § 9.6.2
/// intends the coefficients to do.
#[derive(Debug, Clone)]
pub struct Whitener {
    coefficients: [Complex; 3],
    nonlinear: Option<f64>,
    history: [Complex; 3],
}

impl Whitener {
    /// For a far transmitter asked for `coefficients`, and for Θ = 0.3125
    /// over points of mean energy `nonlinear`, if given.
    #[must_use]
    pub fn new(coefficients: Precoding, nonlinear: Option<f64>) -> Self {
        let scale = f64::from(1 << COEFFICIENT_FRACTION);
        Self {
            coefficients: coefficients
                .map(|(re, im)| (f64::from(re) / scale, f64::from(im) / scale)),
            nonlinear,
            history: [(0.0, 0.0); 3],
        }
    }

    /// Takes the equalized point, and gives y(n) and the point the
    /// equalizer should have given, both in the units of figure 5.
    pub fn push(&mut self, z: Complex) -> (Complex, Complex) {
        let x = self.nonlinear.map_or(z, |energy| unwarp(z, energy));
        let mut y = x;
        for ((xr, xi), (hr, hi)) in self.history.iter().zip(self.coefficients) {
            y.0 += xr * hr - xi * hi;
            y.1 += xr * hi + xi * hr;
        }
        self.history = [x, self.history[0], self.history[1]];
        let decided = (nearest_odd(y.0), nearest_odd(y.1));
        let target = (x.0 + decided.0 - y.0, x.1 + decided.1 - y.1);
        let target = self.nonlinear.map_or(target, |energy| warp(target, energy));
        (y, target)
    }
}

/// The nearest odd integer, where the points of figure 5 lie.
#[must_use]
pub fn nearest_odd(value: f64) -> f64 {
    2.0 * ((value - 1.0) / 2.0).round() + 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_halves_towards_the_smaller_magnitude() {
        assert_eq!(round_div(3, 2), 1);
        assert_eq!(round_div(-3, 2), -1);
        assert_eq!(round_div(5, 2), 2);
        assert_eq!(round_div(7, 4), 2);
        assert_eq!(round_div(-6, 4), -1);
        assert_eq!(round_div(0, 4), 0);
    }

    #[test]
    fn unwarps_what_it_warps() {
        let energy = 200.0;
        for x in [(1.0, 1.0), (-23.0, 17.0), (35.5, -40.25), (0.0, 0.0)] {
            let back = unwarp(warp(x, energy), energy);
            assert!(
                (back.0 - x.0).abs() < 1e-9 && (back.1 - x.1).abs() < 1e-9,
                "{x:?} came back as {back:?}"
            );
        }
    }
}
