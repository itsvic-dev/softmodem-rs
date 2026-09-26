// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The receiver of phases 3 and 4 and of data mode: a front end that mixes
//! the far carrier down and samples the matched filter twice a symbol,
//! following the far symbol clock, and an equaliser with a phase locked
//! loop.

use std::collections::VecDeque;
use std::f64::consts::{PI, SQRT_2, TAU};

use super::SymbolRate;
use super::modulator::{ROLL_OFF, SPAN};
use crate::SAMPLE_RATE;
use crate::passband::Complex;

// Steps of the pulse table per sample.
const PHASES: usize = 256;
/// How quickly the front end follows the far symbol clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracking {
    /// On S, which carries strong timing, from any starting phase.
    Acquire,
    /// On PP and TRN.
    Train,
    /// In data mode, where Gardner's self-noise on a large constellation
    /// would limit the SNR, and only clock drift is left to follow.
    Data,
}

impl Tracking {
    fn gain(self) -> f64 {
        match self {
            Self::Acquire => 0.05,
            Self::Train => 0.01,
            Self::Data => 0.0005,
        }
    }
}
const EQUALIZER_TAPS: usize = 48;
const CENTER: usize = EQUALIZER_TAPS / 2 + 1;
// A margin for strobes that fall behind the newest sample.
const SLACK: usize = 8;

/// Symbols an equalised point lags the symbol that `Equalizer::output` takes.
pub const DELAY: usize = (EQUALIZER_TAPS - 1 - CENTER) / 2;
const EQUALIZER_STEP: f64 = 0.05;
const PHASE_GAIN: f64 = 0.05;
const FREQUENCY_GAIN: f64 = 0.002;
const ERROR_SMOOTHING: f64 = 1.0 / 256.0;
// Slow, as the power of a large constellation swings from symbol to symbol.
const LEVEL_SMOOTHING: f64 = 1.0 / 8192.0;
// Of the level heard so far, below which the far end counts as silent.
const SILENCE: f64 = 0.02;
// Below this share of the usual energy, as when the far end falls silent, nothing adapts.
const QUIET: f64 = 0.5;
// A signal this far above the level so far starts it again, as after silence.
const ONSET: f64 = 100.0;
const ENERGY_SMOOTHING: f64 = 1.0 / 1024.0;

fn root_raised_cosine(t: f64) -> f64 {
    let b = ROLL_OFF;
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

// Over −1 to 1, for a stopband deep enough to take the mixer's image.
fn blackman(x: f64) -> f64 {
    if x.abs() >= 1.0 {
        return 0.0;
    }
    let angle = PI * (x + 1.0);
    0.42 - 0.5 * angle.cos() + 0.08 * (2.0 * angle).cos()
}

fn multiply(a: Complex, b: Complex) -> Complex {
    (a.0 * b.0 - a.1 * b.1, a.0 * b.1 + a.1 * b.0)
}

fn conjugate(a: Complex) -> Complex {
    (a.0, -a.1)
}

fn power(a: Complex) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

/// One symbol from the front end: the matched filter at the symbol and half
/// a symbol before it, and when the symbol was, in samples since the front
/// end started.
#[derive(Debug, Clone, Copy)]
pub struct Symbol {
    pub middle: Complex,
    pub symbol: Complex,
    pub at: f64,
}

/// Mixes the far carrier down and samples the matched filter at the far
/// symbol rate, with Gardner timing recovery.
#[derive(Debug)]
pub struct FrontEnd {
    carrier_step: f64,
    carrier_phase: f64,
    samples_per_symbol: f64,
    half_width: usize,
    table: Vec<Vec<f64>>,
    mixed: VecDeque<Complex>,
    count: usize,
    next: f64,
    middle: Option<Complex>,
    last: Complex,
    energy: f64,
    timing_gain: f64,
}

impl FrontEnd {
    #[must_use]
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "the filter is a few dozen taps"
    )]
    pub fn new(symbol_rate: SymbolRate, high_carrier: bool) -> Self {
        let samples_per_symbol = SAMPLE_RATE / symbol_rate.baud();
        let half_width = (SPAN as f64 * samples_per_symbol).ceil() as usize;
        let table = (0..PHASES)
            .map(|phase| {
                let fraction = phase as f64 / PHASES as f64;
                (0..=2 * half_width)
                    .map(|k| {
                        let t = (k as f64 - half_width as f64 - fraction) / samples_per_symbol;
                        root_raised_cosine(t) * blackman(t / SPAN as f64) / samples_per_symbol
                    })
                    .collect()
            })
            .collect();
        Self {
            carrier_step: symbol_rate.carrier_hz(high_carrier) / SAMPLE_RATE,
            carrier_phase: 0.0,
            samples_per_symbol,
            half_width,
            table,
            mixed: VecDeque::from(vec![(0.0, 0.0); 2 * half_width + SLACK]),
            count: 0,
            next: (half_width + SLACK) as f64,
            middle: None,
            last: (0.0, 0.0),
            energy: 0.0,
            timing_gain: Tracking::Acquire.gain(),
        }
    }

    /// From now on follows the far clock as `tracking` asks. It starts with
    /// `Tracking::Acquire`.
    pub fn track(&mut self, tracking: Tracking) {
        self.timing_gain = tracking.gain();
    }

    /// The symbols whose samples `input` completes.
    pub fn process(&mut self, input: &[i16]) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        for &sample in input {
            let x = f64::from(sample);
            let angle = TAU * self.carrier_phase;
            self.carrier_phase = (self.carrier_phase + self.carrier_step).fract();
            self.mixed.pop_front();
            self.mixed.push_back((x * angle.cos(), -x * angle.sin()));
            self.count += 1;
            while let Some(symbol) = self.strobe() {
                symbols.push(symbol);
            }
        }
        symbols
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "sample positions are small and positive"
    )]
    fn sample_at(&self, t: f64) -> Complex {
        let newest = self.count - 1;
        let base = t.floor();
        let fraction = t - base;
        let phase = ((fraction * PHASES as f64).round() as usize).min(PHASES - 1);
        let base = base as usize;
        let taps = &self.table[phase];
        let first = base - self.half_width;
        let offset = self.mixed.len() - 1 - (newest - first);
        taps.iter()
            .zip(self.mixed.iter().skip(offset))
            .fold((0.0, 0.0), |(re, im), (g, x)| (re + g * x.0, im + g * x.1))
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts stay far below 2⁵²"
    )]
    fn strobe(&mut self) -> Option<Symbol> {
        let ready = self.count as f64 - 2.0 - self.half_width as f64;
        if self.next > ready {
            return None;
        }
        let at = self.next;
        let value = self.sample_at(at);
        self.next += self.samples_per_symbol / 2.0;
        let Some(middle) = self.middle.take() else {
            self.middle = Some(value);
            return None;
        };
        let step = (value.0 - self.last.0, value.1 - self.last.1);
        let energy = power(value) + power(self.last);
        self.energy += ENERGY_SMOOTHING * (energy - self.energy);
        if energy > QUIET * self.energy && energy > 0.0 {
            let error = (middle.0 * step.0 + middle.1 * step.1) / energy;
            self.next -= self.timing_gain * error * self.samples_per_symbol;
        }
        self.last = value;
        Some(Symbol {
            middle,
            symbol: value,
            at,
        })
    }
}

/// A fractionally spaced equaliser and a phase locked loop, for points of
/// unit mean power. The caller gives each symbol's target: a known point in
/// training, a decision after it.
#[derive(Debug)]
pub struct Equalizer {
    line: VecDeque<Complex>,
    taps: Vec<Complex>,
    input_power: f64,
    symbols: u32,
    phase: f64,
    frequency: f64,
    output: Complex,
    rotation: Complex,
    error: f64,
}

impl Default for Equalizer {
    fn default() -> Self {
        let mut taps = vec![(0.0, 0.0); EQUALIZER_TAPS];
        taps[CENTER] = (1.0, 0.0);
        Self {
            line: VecDeque::from(vec![(0.0, 0.0); EQUALIZER_TAPS]),
            taps,
            input_power: 0.0,
            symbols: 0,
            phase: 0.0,
            frequency: 0.0,
            output: (0.0, 0.0),
            rotation: (1.0, 0.0),
            error: 1.0,
        }
    }
}

impl Equalizer {
    /// Learns the level of the input again from the next symbol, as when
    /// the far end starts after only this end's own echo was heard.
    pub fn relevel(&mut self) {
        self.symbols = 0;
    }

    /// The equalised point for `symbol`.
    pub fn output(&mut self, symbol: &Symbol) -> Complex {
        let heard = power(symbol.symbol);
        if heard > ONSET * self.input_power {
            self.symbols = 0;
        }
        if heard > SILENCE * self.input_power {
            self.symbols = self.symbols.saturating_add(1);
            let weight = f64::from(self.symbols).recip().max(LEVEL_SMOOTHING);
            self.input_power += weight * (heard - self.input_power);
        }
        let norm = self.input_power.sqrt().max(f64::MIN_POSITIVE).recip();
        for sample in [symbol.middle, symbol.symbol] {
            self.line.pop_front();
            self.line.push_back((sample.0 * norm, sample.1 * norm));
        }
        let y = self
            .taps
            .iter()
            .zip(&self.line)
            .fold((0.0, 0.0), |acc, (&w, &x)| {
                let p = multiply(w, x);
                (acc.0 + p.0, acc.1 + p.1)
            });
        self.rotation = (self.phase.cos(), -self.phase.sin());
        self.output = multiply(y, self.rotation);
        self.output
    }

    /// Learns from the last output against what it should have been, unless
    /// the far end is silent.
    pub fn adapt(&mut self, target: Complex) {
        let energy: f64 = self.line.iter().map(|&x| power(x)).sum();
        #[expect(clippy::cast_precision_loss, reason = "48 taps")]
        if energy < QUIET * EQUALIZER_TAPS as f64 {
            return;
        }
        let z = self.output;
        // Weighted by the point's power, so the inner points do not jitter it.
        let phase_error = multiply(z, conjugate(target)).1;
        self.frequency += FREQUENCY_GAIN * phase_error;
        self.phase += PHASE_GAIN * phase_error + self.frequency;

        let error = (target.0 - z.0, target.1 - z.1);
        self.error += ERROR_SMOOTHING * (power(error) - self.error);
        let error = multiply(error, conjugate(self.rotation));
        let gain = EQUALIZER_STEP / energy;
        for (w, &x) in self.taps.iter_mut().zip(&self.line) {
            let step = multiply(error, conjugate(x));
            *w = (w.0 + gain * step.0, w.1 + gain * step.1);
        }
    }

    /// The mean square error of recent outputs against their targets.
    #[must_use]
    pub fn error(&self) -> f64 {
        self.error
    }
}
