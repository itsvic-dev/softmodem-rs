// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! DTMF digits, as Q.23 has them, for a menu at the far end that answers
//! before its modem does.

use std::collections::VecDeque;

use crate::SAMPLE_RATE;
use crate::tone::Tone;

const LOW_HZ: [f64; 4] = [697.0, 770.0, 852.0, 941.0];
const HIGH_HZ: [f64; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
const KEYS: [[char; 4]; 4] = [
    ['1', '2', '3', 'A'],
    ['4', '5', '6', 'B'],
    ['7', '8', '9', 'C'],
    ['*', '0', '#', 'D'],
];
// Well above the 40 ms that detectors need, so a digit survives a codec.
const TONE_SAMPLES: usize = 800;
const GAP_SAMPLES: usize = 800;
const LOW_DBM0: f64 = -10.0;
// The high group 2 dB above the low one, to make up for the line's loss.
const HIGH_DBM0: f64 = -8.0;

#[derive(Debug)]
enum Step {
    Digit(Tone, Tone, usize),
    Silence(usize),
}

/// Plays a sequence of DTMF digits, with `,` as a pause.
#[derive(Debug)]
pub struct DtmfSender {
    steps: VecDeque<Step>,
}

impl DtmfSender {
    /// `sequence` holds `0` to `9`, `*`, `#`, `A` to `D` and `,`, which is
    /// silence for `pause_seconds`. Other characters are skipped.
    #[must_use]
    pub fn new(sequence: &str, pause_seconds: f64) -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a pause of a few seconds"
        )]
        let pause = (pause_seconds.max(0.0) * SAMPLE_RATE) as usize;
        let mut steps = VecDeque::new();
        for key in sequence.chars() {
            if key == ',' {
                steps.push_back(Step::Silence(pause));
                continue;
            }
            let Some((row, column)) = KEYS.iter().enumerate().find_map(|(row, keys)| {
                keys.iter()
                    .position(|&k| k == key)
                    .map(|column| (row, column))
            }) else {
                continue;
            };
            steps.push_back(Step::Digit(
                Tone::new(LOW_HZ[row], LOW_DBM0),
                Tone::new(HIGH_HZ[column], HIGH_DBM0),
                TONE_SAMPLES,
            ));
            steps.push_back(Step::Silence(GAP_SAMPLES));
        }
        Self { steps }
    }

    /// Whether the whole sequence has been played.
    #[must_use]
    pub fn done(&self) -> bool {
        self.steps.is_empty()
    }

    /// Fills `out`, with silence once the sequence is done.
    pub fn render(&mut self, out: &mut [i16]) {
        let mut at = 0;
        while at < out.len() {
            let Some(step) = self.steps.front_mut() else {
                out[at..].fill(0);
                return;
            };
            let (left, chunk) = match step {
                Step::Digit(low, high, left) => {
                    let n = (*left).min(out.len() - at);
                    let chunk = &mut out[at..at + n];
                    low.render(chunk);
                    let mut other = vec![0; n];
                    high.render(&mut other);
                    for (sample, high) in chunk.iter_mut().zip(other) {
                        *sample = sample.saturating_add(high);
                    }
                    *left -= n;
                    (*left, n)
                }
                Step::Silence(left) => {
                    let n = (*left).min(out.len() - at);
                    out[at..at + n].fill(0);
                    *left -= n;
                    (*left, n)
                }
            };
            at += chunk;
            if left == 0 {
                self.steps.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlator::Correlator;
    use crate::sine_peak;

    fn play(sequence: &str, pause_seconds: f64) -> Vec<i16> {
        let mut sender = DtmfSender::new(sequence, pause_seconds);
        let mut out = Vec::new();
        let mut frame = [0; 160];
        while !sender.done() {
            sender.render(&mut frame);
            out.extend_from_slice(&frame);
        }
        out
    }

    #[expect(clippy::cast_precision_loss, reason = "a few hundred samples")]
    fn amplitude(samples: &[i16], hz: f64) -> f64 {
        let mut correlator = Correlator::new(hz, samples.len() as f64);
        let mut power = 0.0;
        for &sample in samples {
            power = correlator.push(f64::from(sample));
        }
        2.0 * power.sqrt()
    }

    #[test]
    fn each_key_sends_its_row_and_column() {
        for (row, keys) in KEYS.iter().enumerate() {
            for (column, &key) in keys.iter().enumerate() {
                let samples = play(&key.to_string(), 0.0);
                let tone = &samples[..TONE_SAMPLES];
                for (i, &hz) in LOW_HZ.iter().enumerate() {
                    let heard = amplitude(tone, hz) > sine_peak(LOW_DBM0 - 3.0);
                    assert_eq!(heard, i == row, "{key} at {hz} Hz");
                }
                for (i, &hz) in HIGH_HZ.iter().enumerate() {
                    let heard = amplitude(tone, hz) > sine_peak(HIGH_DBM0 - 3.0);
                    assert_eq!(heard, i == column, "{key} at {hz} Hz");
                }
            }
        }
    }

    #[test]
    fn a_digit_is_a_tone_then_a_gap() {
        let samples = play("5", 0.0);
        assert!(samples[..TONE_SAMPLES].iter().any(|&s| s != 0));
        assert!(
            samples[TONE_SAMPLES..TONE_SAMPLES + GAP_SAMPLES]
                .iter()
                .all(|&s| s == 0)
        );
    }

    #[test]
    fn a_comma_is_a_silent_pause() {
        let samples = play(",1", 0.5);
        assert!(samples[..4000].iter().all(|&s| s == 0));
        assert!(samples[4000..4000 + TONE_SAMPLES].iter().any(|&s| s != 0));
    }

    #[test]
    fn skips_what_is_not_a_key() {
        assert!(DtmfSender::new("xyz", 1.0).done());
        assert_eq!(play("1x2", 0.0), play("12", 0.0));
    }

    #[test]
    fn is_silent_once_done() {
        let mut sender = DtmfSender::new("", 0.0);
        let mut out = [1; 160];
        sender.render(&mut out);
        assert!(out.iter().all(|&s| s == 0));
    }
}
