// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Single tones, such as the V.25 answer tone.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::correlator::Correlator;
use crate::{SAMPLE_RATE, sine_peak, to_sample};

/// The V.25 answer tone.
pub const ANSWER_TONE_HZ: f64 = 2100.0;

const WINDOW: usize = 160;
// Share of all power that must be in the tone, so a tone under data is not one.
const PURITY: f64 = 0.7;
const DETECT_DBM0: f64 = -43.0;

/// A phase-continuous sine.
#[derive(Debug)]
pub struct Tone {
    step: f64,
    phase: f64,
    amplitude: f64,
}

impl Tone {
    #[must_use]
    pub fn new(hz: f64, level_dbm0: f64) -> Self {
        Self {
            step: hz / SAMPLE_RATE,
            phase: 0.0,
            amplitude: sine_peak(level_dbm0),
        }
    }

    pub fn render(&mut self, out: &mut [i16]) {
        for sample in out {
            *sample = to_sample(self.amplitude * (TAU * self.phase).sin());
            self.phase = (self.phase + self.step).fract();
        }
    }
}

/// Tells whether one tone, and little else, is on the line.
#[derive(Debug)]
pub struct ToneDetector {
    tone: Correlator,
    squares: VecDeque<f64>,
    sum_of_squares: f64,
    threshold: f64,
    present: bool,
}

impl ToneDetector {
    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    #[must_use]
    pub fn new(hz: f64) -> Self {
        Self {
            tone: Correlator::new(hz, WINDOW as f64),
            squares: VecDeque::from(vec![0.0; WINDOW]),
            sum_of_squares: 0.0,
            threshold: sine_peak(DETECT_DBM0),
            present: false,
        }
    }

    /// Whether the tone was present at the end of `input`.
    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    pub fn process(&mut self, input: &[i16]) -> bool {
        for &sample in input {
            let x = f64::from(sample);
            let power = self.tone.push(x);
            let square = x * x;
            self.sum_of_squares += square - self.squares.pop_front().unwrap_or_default();
            self.squares.push_back(square);

            let amplitude = 2.0 * power.sqrt();
            let mean_square = self.sum_of_squares / WINDOW as f64;
            self.present = amplitude >= self.threshold && 2.0 * power >= PURITY * mean_square;
        }
        self.present
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsk::{Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0};

    fn tone(hz: f64, dbm0: f64, samples: usize) -> Vec<i16> {
        let mut out = vec![0; samples];
        Tone::new(hz, dbm0).render(&mut out);
        out
    }

    #[test]
    fn hears_the_answer_tone() {
        let mut detector = ToneDetector::new(ANSWER_TONE_HZ);
        assert!(detector.process(&tone(ANSWER_TONE_HZ, V21_MAX_LEVEL_DBM0, 800)));
    }

    #[test]
    fn stops_hearing_it_within_a_window_of_silence() {
        let mut detector = ToneDetector::new(ANSWER_TONE_HZ);
        detector.process(&tone(ANSWER_TONE_HZ, V21_MAX_LEVEL_DBM0, 800));
        assert!(!detector.process(&[0; WINDOW]));
    }

    #[test]
    fn ignores_the_answering_carrier() {
        let mut modulator = Modulator::new(V21_ANSWER, V21_MAX_LEVEL_DBM0);
        modulator.push_bits((0..200).map(|n| n % 3 == 0));
        let mut carrier = vec![0; 8000];
        modulator.render(&mut carrier);
        let mut detector = ToneDetector::new(ANSWER_TONE_HZ);
        assert!(!detector.process(&carrier));
    }

    #[test]
    fn ignores_a_tone_below_the_threshold() {
        let mut detector = ToneDetector::new(ANSWER_TONE_HZ);
        assert!(!detector.process(&tone(ANSWER_TONE_HZ, -46.0, 800)));
    }

    #[test]
    fn ignores_other_frequencies() {
        let mut detector = ToneDetector::new(ANSWER_TONE_HZ);
        assert!(!detector.process(&tone(1800.0, V21_MAX_LEVEL_DBM0, 800)));
    }
}
