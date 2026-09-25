// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The line signal of phases 3 and 4 and of data mode: points shaped at the
//! symbol rate onto the carrier of table 2, through the pre-emphasis of
//! § 5.4.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use super::SymbolRate;
use crate::passband::{Complex, Pulse, Transmitter};
use crate::{SAMPLE_RATE, to_sample};

/// Flat to ±0.45 S around the carrier, as the templates of figures 1 and 2
/// ask.
pub(crate) const ROLL_OFF: f64 = 0.1;
/// Symbols each side of a symbol that its pulse reaches.
pub const SPAN: usize = 12;
const TAPS: usize = 31;
/// Samples the pre-emphasis filter delays the signal by.
pub const FILTER_DELAY: usize = TAPS / 2;

pub(crate) fn pulse(symbol_rate: SymbolRate) -> Pulse {
    Pulse {
        baud: symbol_rate.baud(),
        roll_off: ROLL_OFF,
        span: SPAN,
    }
}

/// The gain in dB of pre-emphasis filter `index`, 0 to 10, at f/S, as
/// figures 1 and 2 and tables 3 and 4 give it. Figure 2 leaves 0.4 to 0.8
/// open; this takes a straight line across it.
#[must_use]
pub fn pre_emphasis_db(index: u8, normalized: f64) -> f64 {
    let f = normalized.clamp(0.0, 1.2);
    match index {
        0..=5 => 2.0 * f64::from(index) * f,
        6..=10 => {
            let beta = 0.5 * f64::from(index - 5);
            let gamma = 2.0 * beta;
            if f <= 0.4 {
                0.0
            } else if f <= 0.8 {
                beta * (f - 0.4) / 0.4
            } else {
                beta + gamma * (f - 0.8) / 0.4
            }
        }
        _ => 0.0,
    }
}

#[expect(clippy::cast_precision_loss, reason = "a few dozen taps")]
fn response(taps: &[f64], hz: f64) -> f64 {
    let (re, im) = taps
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(re, im), (n, &h)| {
            let angle = TAU * hz * n as f64 / SAMPLE_RATE;
            (re + h * angle.cos(), im - h * angle.sin())
        });
    (re * re + im * im).sqrt()
}

#[expect(clippy::cast_precision_loss, reason = "a few dozen taps and points")]
fn design(symbol_rate: SymbolRate, carrier_hz: f64, index: u8) -> Vec<f64> {
    let n = TAPS as f64;
    let middle = (TAPS / 2) as f64;
    let gain = |k: usize| {
        let hz = k as f64 * SAMPLE_RATE / n;
        10f64.powf(pre_emphasis_db(index, hz / symbol_rate.baud()) / 20.0)
    };
    let taps: Vec<f64> = (0..TAPS)
        .map(|t| {
            let rest: f64 = (1..=TAPS / 2)
                .map(|k| 2.0 * gain(k) * (TAU * k as f64 * (t as f64 - middle) / n).cos())
                .sum();
            (gain(0) + rest) / n
        })
        .collect();
    let band = symbol_rate.baud() / 2.0;
    let power: f64 = (0..64)
        .map(|i| {
            response(
                &taps,
                carrier_hz - band + 2.0 * band * (f64::from(i) + 0.5) / 64.0,
            )
        })
        .map(|g| g * g)
        .sum::<f64>()
        / 64.0;
    taps.into_iter().map(|h| h / power.sqrt()).collect()
}

/// Sends points of unit mean power at one symbol rate and carrier, with a
/// mean power of a sine at the level it is given.
#[derive(Debug)]
pub struct Modulator {
    transmitter: Transmitter,
    taps: Vec<f64>,
    history: VecDeque<f64>,
    shaped: Vec<i16>,
}

impl Modulator {
    /// With pre-emphasis filter `pre_emphasis`, 0 to 10, as the far end
    /// asked in INFO1.
    #[must_use]
    pub fn new(
        symbol_rate: SymbolRate,
        high_carrier: bool,
        pre_emphasis: u8,
        level_dbm0: f64,
    ) -> Self {
        let carrier_hz = symbol_rate.carrier_hz(high_carrier);
        let taps = design(symbol_rate, carrier_hz, pre_emphasis);
        Self {
            transmitter: Transmitter::with_pulse(carrier_hz, level_dbm0, pulse(symbol_rate)),
            history: VecDeque::from(vec![0.0; taps.len()]),
            taps,
            shaped: Vec::new(),
        }
    }

    /// Fills `out`, taking a point from `next_point` each symbol period.
    pub fn render(&mut self, out: &mut [i16], next_point: impl FnMut() -> Complex) {
        self.shaped.resize(out.len(), 0);
        self.transmitter.render(&mut self.shaped, next_point);
        for (sample, &shaped) in out.iter_mut().zip(&self.shaped) {
            self.history.pop_front();
            self.history.push_back(f64::from(shaped));
            let filtered: f64 = self
                .history
                .iter()
                .rev()
                .zip(&self.taps)
                .map(|(x, h)| x * h)
                .sum();
            *sample = to_sample(filtered);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_0_is_flat() {
        let taps = design(SymbolRate::S3200, 1920.0, 0);
        for hz in [300.0, 1000.0, 1920.0, 3000.0, 3500.0] {
            assert!((20.0 * response(&taps, hz).log10()).abs() < 0.01);
        }
    }

    #[test]
    fn each_filter_follows_its_template_across_the_band() {
        for symbol_rate in [SymbolRate::S2400, SymbolRate::S3200, SymbolRate::S3429] {
            let carrier = symbol_rate.carrier_hz(false);
            let baud = symbol_rate.baud();
            for index in 0..=10 {
                let taps = design(symbol_rate, carrier, index);
                let at = |normalized: f64| 20.0 * response(&taps, normalized * baud).log10();
                let reference = carrier / baud;
                for offset in [-0.45, -0.2, 0.2, 0.45] {
                    let normalized = reference + offset;
                    let measured = at(normalized) - at(reference);
                    let wanted =
                        pre_emphasis_db(index, normalized) - pre_emphasis_db(index, reference);
                    assert!(
                        (measured - wanted).abs() < 1.0,
                        "filter {index} at {symbol_rate:?} would leave the ±1 dB of figures 1 and 2: \
                         {measured:.2} dB, not {wanted:.2} dB at f/S = {normalized:.2}"
                    );
                }
            }
        }
    }

    #[test]
    fn follows_tables_3_and_4() {
        assert!((pre_emphasis_db(5, 1.0) - 10.0).abs() < 1e-9);
        assert!((pre_emphasis_db(3, 0.5) - 3.0).abs() < 1e-9);
        assert!((pre_emphasis_db(8, 0.8) - 1.5).abs() < 1e-9);
        assert!((pre_emphasis_db(10, 1.2) - 7.5).abs() < 1e-9);
        assert!(pre_emphasis_db(7, 0.3).abs() < 1e-9);
    }
}
