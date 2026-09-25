// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The 16-point QAM of V.22bis at 600 baud, and its 4-point 1200 bit/s form.

use std::collections::VecDeque;

use crate::dpsk::{dibit, quarter_turns};
use crate::passband::{Complex, Receiver, Strobe, Transmitter};

// V.22bis § 3.2: off 40 to 65 ms after the signal falls.
const CARRIER_OFF_SAMPLES: u32 = 416;

const EQUALIZER_TAPS: usize = 17;
const EQUALIZER_STEP: f64 = 0.25;
const POWER_SMOOTHING: f64 = 1.0 / 64.0;
const PHASE_GAIN: f64 = 0.08;
const FREQUENCY_GAIN: f64 = 0.004;

/// How many bits each symbol carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rate {
    /// Dibits as quadrant changes, on the 01 point of each quadrant.
    Bps1200,
    /// Quadbits: a quadrant change, then one of four points in the quadrant.
    Bps2400,
}

// Figure 2 of V.22bis, in quadrant 1, for the last two bits of a quadbit.
fn inner_point(third: bool, fourth: bool) -> Complex {
    let scale = 10f64.sqrt().recip();
    let x = if fourth { 3.0 } else { 1.0 };
    let y = if third { 3.0 } else { 1.0 };
    (x * scale, y * scale)
}

fn rotate(point: Complex, quarter_turns: u8) -> Complex {
    (0..quarter_turns % 4).fold(point, |(x, y), _| (-y, x))
}

fn multiply(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

fn conjugate(a: Complex) -> Complex {
    (a.0, -a.1)
}

fn scale(a: Complex, k: f64) -> Complex {
    (a.0 * k, a.1 * k)
}

fn power(a: Complex) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

/// Turns bits into V.22bis QAM on one carrier. Its mean power at 2400 bit/s
/// is that of a sine at the level it is given.
#[derive(Debug)]
pub struct Modulator {
    transmitter: Transmitter,
    quadrant: u8,
}

impl Modulator {
    #[must_use]
    pub fn new(carrier_hz: f64, level_dbm0: f64) -> Self {
        Self {
            transmitter: Transmitter::new(carrier_hz, level_dbm0),
            quadrant: 0,
        }
    }

    /// Fills `out`, taking two or four bits from `next_bit` for each symbol.
    pub fn render(&mut self, out: &mut [i16], rate: Rate, mut next_bit: impl FnMut() -> bool) {
        let quadrant = &mut self.quadrant;
        self.transmitter.render(out, || {
            let first = next_bit();
            let second = next_bit();
            *quadrant = (*quadrant + quarter_turns(first, second)) % 4;
            let inner = match rate {
                Rate::Bps1200 => inner_point(false, true),
                Rate::Bps2400 => {
                    let third = next_bit();
                    inner_point(third, next_bit())
                }
            };
            rotate(inner, *quadrant)
        });
    }
}

#[derive(Debug, Clone, Copy)]
struct Decision {
    point: Complex,
    quadrant: u8,
    inner: (bool, bool),
}

fn decide(z: Complex, rate: Rate) -> Decision {
    let inners: &[(bool, bool)] = match rate {
        Rate::Bps1200 => &[(false, true)],
        Rate::Bps2400 => &[(false, false), (false, true), (true, false), (true, true)],
    };
    let distance = |d: &Decision| power((z.0 - d.point.0, z.1 - d.point.1));
    (0..4)
        .flat_map(|quadrant| {
            inners.iter().map(move |&inner| Decision {
                point: rotate(inner_point(inner.0, inner.1), quadrant),
                quadrant,
                inner,
            })
        })
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
        .expect("every rate has points")
}

/// Turns V.22bis QAM on one carrier back into bits. An adaptive equaliser and
/// a phase locked loop follow the channel and the carrier, trained by
/// decisions, so it needs a little of the 1200 bit/s signal before it can
/// decide between 16 points.
#[derive(Debug)]
pub struct Demodulator {
    receiver: Receiver,
    rate: Rate,
    line: VecDeque<Complex>,
    taps: Vec<Complex>,
    symbols: u32,
    power: f64,
    phase: f64,
    frequency: f64,
    quadrant: u8,
    error: f64,
}

impl Demodulator {
    #[must_use]
    pub fn new(carrier_hz: f64) -> Self {
        let mut taps = vec![(0.0, 0.0); EQUALIZER_TAPS];
        taps[EQUALIZER_TAPS / 2] = (1.0, 0.0);
        Self {
            receiver: Receiver::new(carrier_hz, CARRIER_OFF_SAMPLES),
            rate: Rate::Bps1200,
            line: VecDeque::from(vec![(0.0, 0.0); EQUALIZER_TAPS]),
            taps,
            symbols: 0,
            power: 0.0,
            phase: 0.0,
            frequency: 0.0,
            quadrant: 0,
            error: 1.0,
        }
    }

    /// Whether carrier has been on the line long enough to count, with the
    /// V.22bis thresholds and response times.
    #[must_use]
    pub fn carrier(&self) -> bool {
        self.receiver.carrier()
    }

    pub fn set_rate(&mut self, rate: Rate) {
        self.rate = rate;
    }

    /// Forgets what the equaliser learned and goes back to 1200 bit/s, for a
    /// training that starts at the end of S1.
    pub fn restart(&mut self) {
        self.taps.fill((0.0, 0.0));
        self.taps[EQUALIZER_TAPS / 2] = (1.0, 0.0);
        self.rate = Rate::Bps1200;
        self.error = 1.0;
    }

    /// The mean square distance of recent symbols from their decisions, on a
    /// constellation of unit mean power.
    #[must_use]
    pub fn error(&self) -> f64 {
        self.error
    }

    /// Appends the bits found in `input` to `bits`, two or four for each
    /// symbol while any signal is on the line.
    pub fn process(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        for &sample in input {
            if let Some(strobe) = self.receiver.push(sample) {
                self.symbol(strobe, bits);
            }
        }
    }

    fn symbol(&mut self, strobe: Strobe, bits: &mut Vec<bool>) {
        if !strobe.present {
            return;
        }
        self.symbols = self.symbols.saturating_add(1);
        let weight = f64::from(self.symbols).recip().max(POWER_SMOOTHING);
        self.power += weight * (power(strobe.symbol) - self.power);
        let norm = self.power.sqrt().recip();
        for sample in [strobe.middle, strobe.symbol] {
            self.line.pop_front();
            self.line.push_back(scale(sample, norm));
        }

        let y = self
            .taps
            .iter()
            .zip(&self.line)
            .fold((0.0, 0.0), |acc, (&w, &x)| {
                let p = multiply(w, x);
                (acc.0 + p.0, acc.1 + p.1)
            });
        let rotation = (self.phase.cos(), -self.phase.sin());
        let z = multiply(y, rotation);
        let decision = decide(z, self.rate);

        let turn = multiply(z, conjugate(decision.point));
        let phase_error = turn.1.atan2(turn.0);
        self.frequency += FREQUENCY_GAIN * phase_error;
        self.phase += PHASE_GAIN * phase_error + self.frequency;

        let error = (decision.point.0 - z.0, decision.point.1 - z.1);
        self.error += POWER_SMOOTHING * (power(error) - self.error);
        let error = multiply(error, conjugate(rotation));
        let energy: f64 = self.line.iter().map(|&x| power(x)).sum();
        let gain = EQUALIZER_STEP / energy.max(f64::EPSILON);
        for (w, &x) in self.taps.iter_mut().zip(&self.line) {
            let step = scale(multiply(error, conjugate(x)), gain);
            *w = (w.0 + step.0, w.1 + step.1);
        }

        let (first, second) = dibit((decision.quadrant + 4 - self.quadrant) % 4);
        self.quadrant = decision.quadrant;
        bits.push(first);
        bits.push(second);
        if self.rate == Rate::Bps2400 {
            bits.push(decision.inner.0);
            bits.push(decision.inner.1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_constellation_is_figure_2() {
        let at = |bits: (bool, bool), quadrant| {
            let (x, y) = rotate(inner_point(bits.0, bits.1), quadrant);
            let grid = 10f64.sqrt();
            ((x * grid).round(), (y * grid).round())
        };
        assert_eq!(at((false, false), 0), (1.0, 1.0));
        assert_eq!(at((false, true), 0), (3.0, 1.0));
        assert_eq!(at((true, false), 0), (1.0, 3.0));
        assert_eq!(at((true, true), 0), (3.0, 3.0));
        assert_eq!(at((true, true), 1), (-3.0, 3.0));
        assert_eq!(at((false, true), 1), (-1.0, 3.0));
        assert_eq!(at((false, true), 2), (-3.0, -1.0));
        assert_eq!(at((false, true), 3), (1.0, -3.0));
        assert_eq!(at((true, false), 3), (3.0, -1.0));
    }

    #[test]
    fn the_16_points_have_unit_mean_power() {
        let total: f64 = (0..4)
            .flat_map(|q| {
                [(false, false), (false, true), (true, false), (true, true)]
                    .map(|(a, b)| power(rotate(inner_point(a, b), q)))
            })
            .sum();
        assert!((total / 16.0 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn decides_the_nearest_point() {
        let decision = decide((0.9, 0.35), Rate::Bps2400);
        assert_eq!((decision.quadrant, decision.inner), (0, (false, true)));
        let decision = decide((-0.2, 0.9), Rate::Bps1200);
        assert_eq!(decision.quadrant, 1);
    }
}
