//! Symbols shaped by a square root raised cosine on one carrier, as the V.22
//! family and V.34 send them, and the receiver front end that mixes them
//! down, filters them, detects carrier and recovers symbol timing.

use std::collections::VecDeque;
use std::f64::consts::{PI, SQRT_2, TAU};

use crate::{SAMPLE_RATE, sine_peak, to_sample};

pub(crate) const BAUD: f64 = 600.0;

/// The shape of each symbol: a square root raised cosine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Pulse {
    pub(crate) baud: f64,
    pub(crate) roll_off: f64,
    /// Symbols each side of the one being sent that still shape the output.
    pub(crate) span: usize,
}

/// V.22 and V.22bis § 2.4.
pub(crate) const V22_PULSE: Pulse = Pulse {
    baud: BAUD,
    roll_off: 0.75,
    span: 3,
};

// V.22 and V.22bis § 3.3.
const CARRIER_ON_DBM0: f64 = -43.0;
const CARRIER_OFF_DBM0: f64 = -48.0;
const CARRIER_ON_SAMPLES: u32 = 1200;
const LEVEL_WINDOW: usize = 40;
const TIMING_GAIN: f64 = 0.05;

pub(crate) type Complex = (f64, f64);

fn lerp(a: Complex, b: Complex, t: f64) -> Complex {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

// `t` in symbols; the energy of the pulse is one symbol.
fn root_raised_cosine(t: f64, roll_off: f64) -> f64 {
    let b = roll_off;
    if t.abs() < 1e-9 {
        return 1.0 - b + 4.0 * b / PI;
    }
    if ((4.0 * b * t).abs() - 1.0).abs() < 1e-9 {
        let angle = PI / (4.0 * b);
        return b / SQRT_2 * ((1.0 + 2.0 / PI) * angle.sin() + (1.0 - 2.0 / PI) * angle.cos());
    }
    let numerator = (PI * t * (1.0 - b)).sin() + 4.0 * b * t * (PI * t * (1.0 + b)).cos();
    numerator / (PI * t * (1.0 - (4.0 * b * t).powi(2)))
}

/// Shapes symbols onto one carrier. Symbols of unit mean power give the mean
/// power of a sine at the level it is given.
#[derive(Debug)]
pub(crate) struct Transmitter {
    pulse: Pulse,
    carrier_step: f64,
    carrier_phase: f64,
    symbol_step: f64,
    amplitude: f64,
    symbols: VecDeque<Complex>,
    elapsed: f64,
}

impl Transmitter {
    /// With the V.22 pulse.
    pub(crate) fn new(carrier_hz: f64, level_dbm0: f64) -> Self {
        Self::with_pulse(carrier_hz, level_dbm0, V22_PULSE)
    }

    pub(crate) fn with_pulse(carrier_hz: f64, level_dbm0: f64, pulse: Pulse) -> Self {
        Self {
            pulse,
            carrier_step: carrier_hz / SAMPLE_RATE,
            carrier_phase: 0.0,
            symbol_step: pulse.baud / SAMPLE_RATE,
            amplitude: sine_peak(level_dbm0),
            symbols: VecDeque::from(vec![(0.0, 0.0); 2 * pulse.span + 1]),
            elapsed: 0.0,
        }
    }

    /// Fills `out`, taking a symbol from `next_symbol` each symbol period.
    pub(crate) fn render(&mut self, out: &mut [i16], mut next_symbol: impl FnMut() -> Complex) {
        for sample in out {
            if self.elapsed >= 1.0 {
                self.elapsed -= 1.0;
                self.symbols.pop_front();
                self.symbols.push_back(next_symbol());
            }

            let Pulse { span, roll_off, .. } = self.pulse;
            #[expect(
                clippy::cast_precision_loss,
                reason = "the window is a few dozen symbols"
            )]
            let (re, im) =
                self.symbols
                    .iter()
                    .enumerate()
                    .fold((0.0, 0.0), |(re, im), (i, &(a, b))| {
                        let t = self.elapsed + span as f64 - i as f64;
                        let g = root_raised_cosine(t, roll_off);
                        (re + a * g, im + b * g)
                    });
            let angle = TAU * self.carrier_phase;
            *sample = to_sample(self.amplitude * (re * angle.cos() - im * angle.sin()));

            self.carrier_phase = (self.carrier_phase + self.carrier_step).fract();
            self.elapsed += self.symbol_step;
        }
    }
}

/// One symbol from the receiver front end, with the sample half a symbol
/// before it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Strobe {
    pub(crate) middle: Complex,
    pub(crate) symbol: Complex,
    /// Whether any signal was on the line, before carrier detect agrees.
    pub(crate) present: bool,
}

/// Mixes one channel down to baseband, applies the matched filter, detects
/// carrier and recovers symbol timing.
#[derive(Debug)]
pub(crate) struct Receiver {
    carrier_step: f64,
    carrier_phase: f64,
    taps: Vec<f64>,
    mixed: VecDeque<Complex>,
    powers: VecDeque<f64>,
    power_sum: f64,
    symbol_step: f64,
    symbol_phase: f64,
    previous: Complex,
    middle: Complex,
    last_symbol: Complex,
    present: bool,
    carrier: bool,
    carrier_count: u32,
    carrier_on: f64,
    carrier_off: f64,
    carrier_off_samples: u32,
}

impl Receiver {
    /// A receiver whose carrier detect goes off after `carrier_off_samples`
    /// below the threshold.
    pub(crate) fn new(carrier_hz: f64, carrier_off_samples: u32) -> Self {
        Self::with_pulse(carrier_hz, carrier_off_samples, V22_PULSE)
    }

    /// Whose matched filter and symbol clock follow `pulse`.
    pub(crate) fn with_pulse(carrier_hz: f64, carrier_off_samples: u32, pulse: Pulse) -> Self {
        let samples_per_symbol = SAMPLE_RATE / pulse.baud;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "the filter is a few hundred taps"
        )]
        let half = (pulse.span as f64 * samples_per_symbol).round() as i32;
        let taps: Vec<f64> = (-half..=half)
            .map(|n| root_raised_cosine(f64::from(n) / samples_per_symbol, pulse.roll_off))
            .collect();
        let gain: f64 = taps.iter().sum();
        let taps: Vec<f64> = taps.into_iter().map(|g| g / gain).collect();
        Self {
            carrier_step: carrier_hz / SAMPLE_RATE,
            carrier_phase: 0.0,
            mixed: VecDeque::from(vec![(0.0, 0.0); taps.len()]),
            taps,
            powers: VecDeque::from(vec![0.0; LEVEL_WINDOW]),
            power_sum: 0.0,
            symbol_step: pulse.baud / SAMPLE_RATE,
            symbol_phase: 0.0,
            previous: (0.0, 0.0),
            middle: (0.0, 0.0),
            last_symbol: (0.0, 0.0),
            present: false,
            carrier: false,
            carrier_count: 0,
            carrier_on: sine_peak(CARRIER_ON_DBM0),
            carrier_off: sine_peak(CARRIER_OFF_DBM0),
            carrier_off_samples,
        }
    }

    pub(crate) fn carrier(&self) -> bool {
        self.carrier
    }

    /// Takes one sample, and gives a symbol when one falls due.
    pub(crate) fn push(&mut self, sample: i16) -> Option<Strobe> {
        let x = f64::from(sample);
        let angle = TAU * self.carrier_phase;
        self.carrier_phase = (self.carrier_phase + self.carrier_step).fract();
        self.mixed.pop_front();
        self.mixed.push_back((x * angle.cos(), -x * angle.sin()));
        let y = self
            .taps
            .iter()
            .zip(&self.mixed)
            .fold((0.0, 0.0), |(re, im), (g, (a, b))| (re + g * a, im + g * b));

        let power = y.0 * y.0 + y.1 * y.1;
        self.power_sum += power - self.powers.pop_front().unwrap_or_default();
        self.powers.push_back(power);
        #[expect(clippy::cast_precision_loss, reason = "the window is 40")]
        let level = 2.0 * (self.power_sum.max(0.0) / LEVEL_WINDOW as f64).sqrt();
        self.present = level >= self.carrier_off;
        self.track_carrier(level);

        let strobe = self.clock(y);
        self.previous = y;
        strobe
    }

    fn clock(&mut self, y: Complex) -> Option<Strobe> {
        let before = self.symbol_phase;
        let after = before + self.symbol_step;
        if before < 0.5 && after >= 0.5 {
            self.middle = lerp(self.previous, y, (0.5 - before) / self.symbol_step);
        }
        if after < 1.0 {
            self.symbol_phase = after;
            return None;
        }
        let symbol = lerp(self.previous, y, (1.0 - before) / self.symbol_step);
        self.symbol_phase = after - 1.0;

        let last = self.last_symbol;
        self.last_symbol = symbol;
        // Gardner timing error.
        let step = (symbol.0 - last.0, symbol.1 - last.1);
        let energy = symbol.0 * symbol.0 + symbol.1 * symbol.1 + last.0 * last.0 + last.1 * last.1;
        if energy > 0.0 {
            let error = (self.middle.0 * step.0 + self.middle.1 * step.1) / energy;
            self.symbol_phase += TIMING_GAIN * error;
        }
        Some(Strobe {
            middle: self.middle,
            symbol,
            present: self.present,
        })
    }

    fn track_carrier(&mut self, level: f64) {
        let flipping = if self.carrier {
            level < self.carrier_off
        } else {
            level > self.carrier_on
        };
        if !flipping {
            self.carrier_count = 0;
            return;
        }
        self.carrier_count += 1;
        let needed = if self.carrier {
            self.carrier_off_samples
        } else {
            CARRIER_ON_SAMPLES
        };
        if self.carrier_count >= needed {
            self.carrier = !self.carrier;
            self.carrier_count = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_has_the_energy_of_one_symbol() {
        let step = 1e-3;
        for roll_off in [V22_PULSE.roll_off, 0.1] {
            let energy: f64 = (-40_000..=40_000)
                .map(|i| root_raised_cosine(f64::from(i) * step, roll_off).powi(2) * step)
                .sum();
            assert!((energy - 1.0).abs() < 1e-3, "{roll_off}");
        }
    }

    #[test]
    fn the_pulse_is_continuous_where_the_formula_divides_by_zero() {
        for roll_off in [V22_PULSE.roll_off, 0.1] {
            let t = 1.0 / (4.0 * roll_off);
            let step = root_raised_cosine(t + 1e-6, roll_off) - root_raised_cosine(t, roll_off);
            assert!(step.abs() < 1e-4);
        }
    }
}
