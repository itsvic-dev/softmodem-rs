// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The answer tones: ANS of V.25 and `ANSam` of V.8, each with or without the
//! phase reversals that disable echo cancellers, and a detector that tells
//! them apart.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::correlator::Correlator;
use crate::tone::ANSWER_TONE_HZ;
use crate::{SAMPLE_RATE, sine_peak, to_sample};

// V.8 § 7.2 and V.25 § 2.3.
const ENVELOPE_HZ: f64 = 15.0;
const DEPTH: f64 = 0.2;
const REVERSAL_SAMPLES: usize = 3600;

const STEP: usize = 40;
const ENVELOPES: usize = 80;
const PURITY: f64 = 0.7;
const DETECT_DBM0: f64 = -43.0;
const MODULATED: f64 = 0.1;
const GAP_STEPS: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerToneKind {
    /// 2100 Hz, as V.25 sends it.
    Ans,
    /// 2100 Hz amplitude modulated at 15 Hz, as V.8 sends it.
    Ansam,
}

/// Sends ANS or `ANSam`.
#[derive(Debug)]
pub struct AnswerTone {
    amplitude: f64,
    modulated: bool,
    reversals: bool,
    phase: f64,
    sent: usize,
}

impl AnswerTone {
    #[must_use]
    pub fn new(kind: AnswerToneKind, reversals: bool, level_dbm0: f64) -> Self {
        Self {
            amplitude: sine_peak(level_dbm0),
            modulated: kind == AnswerToneKind::Ansam,
            reversals,
            phase: 0.0,
            sent: 0,
        }
    }

    pub fn render(&mut self, out: &mut [i16]) {
        for sample in out {
            if self.reversals && self.sent > 0 && self.sent.is_multiple_of(REVERSAL_SAMPLES) {
                self.phase = (self.phase + 0.5).fract();
            }
            #[expect(clippy::cast_precision_loss, reason = "a few seconds of samples")]
            let t = self.sent as f64 / SAMPLE_RATE;
            let envelope = if self.modulated {
                1.0 + DEPTH * (TAU * ENVELOPE_HZ * t).sin()
            } else {
                1.0
            };
            *sample = to_sample(self.amplitude * envelope * (TAU * self.phase).sin());
            self.phase = (self.phase + ANSWER_TONE_HZ / SAMPLE_RATE).fract();
            self.sent += 1;
        }
    }
}

/// Hears an answer tone and tells whether it is ANS or `ANSam`, once it has
/// lasted 400 ms.
#[derive(Debug)]
pub struct AnswerToneDetector {
    tone: Correlator,
    squares: VecDeque<f64>,
    sum_of_squares: f64,
    threshold: f64,
    count: usize,
    envelopes: VecDeque<f64>,
    gap: u32,
    heard: Option<AnswerToneKind>,
}

impl Default for AnswerToneDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl AnswerToneDetector {
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "the step is 40")]
    pub fn new() -> Self {
        Self {
            tone: Correlator::new(ANSWER_TONE_HZ, STEP as f64),
            squares: VecDeque::from(vec![0.0; STEP]),
            sum_of_squares: 0.0,
            threshold: sine_peak(DETECT_DBM0),
            count: 0,
            envelopes: VecDeque::new(),
            gap: 0,
            heard: None,
        }
    }

    /// The tone on the line at the end of `input`, if one has lasted long
    /// enough to tell which it is.
    pub fn process(&mut self, input: &[i16]) -> Option<AnswerToneKind> {
        for &sample in input {
            let x = f64::from(sample);
            let power = self.tone.push(x);
            let square = x * x;
            self.sum_of_squares += square - self.squares.pop_front().unwrap_or_default();
            self.squares.push_back(square);
            self.count += 1;
            if self.count.is_multiple_of(STEP) {
                self.envelope(power);
            }
        }
        self.heard
    }

    #[expect(clippy::cast_precision_loss, reason = "short windows")]
    fn envelope(&mut self, power: f64) {
        let amplitude = 2.0 * power.sqrt();
        let mean_square = self.sum_of_squares.max(0.0) / STEP as f64;
        let present = amplitude >= self.threshold && 2.0 * power >= PURITY * mean_square;
        self.gap = if present { 0 } else { self.gap + 1 };
        if self.gap >= GAP_STEPS {
            self.envelopes.clear();
            self.heard = None;
            return;
        }
        self.envelopes.push_back(amplitude);
        if self.envelopes.len() > ENVELOPES {
            self.envelopes.pop_front();
        }
        if self.envelopes.len() < ENVELOPES {
            return;
        }
        let rate = SAMPLE_RATE / STEP as f64;
        let (re, im, sum) =
            self.envelopes
                .iter()
                .enumerate()
                .fold((0.0, 0.0, 0.0), |(re, im, sum), (k, &e)| {
                    let angle = TAU * ENVELOPE_HZ * k as f64 / rate;
                    (re + e * angle.cos(), im - e * angle.sin(), sum + e)
                });
        let depth = 2.0 * (re * re + im * im).sqrt() / sum;
        self.heard = Some(if depth > MODULATED {
            AnswerToneKind::Ansam
        } else {
            AnswerToneKind::Ans
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::Tone;

    const LEVEL: f64 = -13.0;

    fn tone(kind: AnswerToneKind, reversals: bool, samples: usize) -> Vec<i16> {
        let mut out = vec![0; samples];
        AnswerTone::new(kind, reversals, LEVEL).render(&mut out);
        out
    }

    fn heard(samples: &[i16]) -> Option<AnswerToneKind> {
        AnswerToneDetector::new().process(samples)
    }

    #[test]
    fn tells_ans_from_ansam_with_and_without_reversals() {
        for reversals in [false, true] {
            let ans = tone(AnswerToneKind::Ans, reversals, 16_000);
            assert_eq!(
                heard(&ans),
                Some(AnswerToneKind::Ans),
                "reversals {reversals}"
            );
            let ansam = tone(AnswerToneKind::Ansam, reversals, 16_000);
            assert_eq!(
                heard(&ansam),
                Some(AnswerToneKind::Ansam),
                "reversals {reversals}"
            );
        }
    }

    #[test]
    fn needs_400_ms_to_decide() {
        assert_eq!(heard(&tone(AnswerToneKind::Ansam, false, 2800)), None);
        assert!(heard(&tone(AnswerToneKind::Ansam, false, 3400)).is_some());
    }

    #[test]
    fn forgets_the_tone_within_30_ms_of_its_end() {
        let mut detector = AnswerToneDetector::new();
        detector.process(&tone(AnswerToneKind::Ansam, true, 16_000));
        assert_eq!(detector.process(&[0; 240]), None);
    }

    #[test]
    fn ignores_silence_and_other_tones() {
        assert_eq!(heard(&vec![0; 16_000]), None);
        let mut mark = vec![0; 16_000];
        Tone::new(1650.0, LEVEL).render(&mut mark);
        assert_eq!(heard(&mark), None);
    }
}
