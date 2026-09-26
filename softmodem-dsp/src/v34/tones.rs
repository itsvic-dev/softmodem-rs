// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Tones A and B of § 10.1.2.1 and § 10.1.2.2, whose phase reversals time
//! the round trip in phase 2.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use super::{GUARD_HZ, carrier_hz, info_level_dbm0};
use crate::correlator::Correlator;
use crate::passband::Complex;
use crate::pump::Role;
use crate::{SAMPLE_RATE, sine_peak, to_sample};

// 5 ms, with nulls every 200 Hz: on the guard tone and on the other tone.
const WINDOW: usize = 40;
const PURITY: f64 = 0.6;
const DETECT_DBM0: f64 = -43.0;
// How far below the tone the average must swing for a reversal to count.
const SWING: f64 = 0.25;

/// Sends tone A from the answer modem or tone B from the call modem, and
/// reverses its phase when told to.
#[derive(Debug)]
pub struct Sender {
    step: f64,
    phase: f64,
    amplitude: f64,
    reverse_in: Option<usize>,
}

impl Sender {
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            step: carrier_hz(role) / SAMPLE_RATE,
            phase: 0.0,
            amplitude: sine_peak(info_level_dbm0(role)),
            reverse_in: None,
        }
    }

    /// Reverses the phase at the sample that many samples into what the
    /// next calls to `render` fill.
    pub fn reverse_after(&mut self, samples: usize) {
        self.reverse_in = Some(samples);
    }

    pub fn render(&mut self, out: &mut [i16]) {
        for sample in out {
            match self.reverse_in {
                Some(0) => {
                    self.phase = (self.phase + 0.5).fract();
                    self.reverse_in = None;
                }
                Some(n) => self.reverse_in = Some(n - 1),
                None => {}
            }
            *sample = to_sample(self.amplitude * (TAU * self.phase).cos());
            self.phase = (self.phase + self.step).fract();
        }
    }
}

/// Hears the far end's tone, and times each of its phase reversals.
#[derive(Debug)]
pub struct Detector {
    step: f64,
    phase: f64,
    mixed: VecDeque<Complex>,
    sum: Complex,
    averages: VecDeque<Complex>,
    squares: VecDeque<f64>,
    sum_of_squares: f64,
    // Some answer modems send it louder than tone A itself, so it does not count against the purity.
    guard: Option<Correlator>,
    threshold: f64,
    samples: u64,
    last_projection: f64,
    candidate: Option<(f64, u64)>,
    absent_for: usize,
    present_from: u64,
}

fn power(a: Complex) -> f64 {
    a.0 * a.0 + a.1 * a.1
}

impl Detector {
    /// For the tone that the end in `role` sends.
    #[expect(clippy::cast_precision_loss, reason = "the window is 40")]
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            step: carrier_hz(role) / SAMPLE_RATE,
            phase: 0.0,
            mixed: VecDeque::from(vec![(0.0, 0.0); WINDOW]),
            sum: (0.0, 0.0),
            averages: VecDeque::from(vec![(0.0, 0.0); WINDOW + 1]),
            squares: VecDeque::from(vec![0.0; WINDOW]),
            sum_of_squares: 0.0,
            guard: (role == Role::Answer).then(|| Correlator::new(GUARD_HZ, WINDOW as f64)),
            threshold: sine_peak(DETECT_DBM0),
            samples: 0,
            last_projection: f64::NAN,
            candidate: None,
            absent_for: 2 * WINDOW,
            present_from: 0,
        }
    }

    /// Whether the tone is on the line, through its reversals.
    #[must_use]
    pub fn present(&self) -> bool {
        self.absent_for < 2 * WINDOW
    }

    /// The reversals heard in `input`, as the position of the first
    /// reversed sample, counted in samples since the detector started.
    pub fn process(&mut self, input: &[i16]) -> Vec<f64> {
        let mut reversals = Vec::new();
        for &sample in input {
            if let Some(at) = self.push(sample) {
                reversals.push(at);
            }
        }
        reversals
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts stay far below 2⁵²"
    )]
    fn push(&mut self, sample: i16) -> Option<f64> {
        let x = f64::from(sample);
        let angle = TAU * self.phase;
        self.phase = (self.phase + self.step).fract();
        let product = (x * angle.cos(), -x * angle.sin());
        let (old_re, old_im) = self.mixed.pop_front().unwrap_or_default();
        self.mixed.push_back(product);
        self.sum = (
            self.sum.0 + product.0 - old_re,
            self.sum.1 + product.1 - old_im,
        );
        let window = WINDOW as f64;
        let average = (self.sum.0 / window, self.sum.1 / window);
        self.averages.pop_front();
        self.averages.push_back(average);
        let square = x * x;
        self.sum_of_squares += square - self.squares.pop_front().unwrap_or_default();
        self.squares.push_back(square);

        let n = self.samples;
        self.samples += 1;
        let tone = 2.0 * power(average);
        let guard = self.guard.as_mut().map_or(0.0, |guard| 2.0 * guard.push(x));
        let heard = 2.0 * tone.sqrt() >= self.threshold
            && tone >= PURITY * (self.sum_of_squares.max(0.0) / window - guard);
        let was_present = self.present();
        self.absent_for = if heard { 0 } else { self.absent_for + 1 };
        if self.present() && !was_present {
            self.present_from = n;
        }

        let before = self.averages[0];
        // A pure tone only: L1 and L2 beat within the window, and at onset `before` still holds them.
        if !self.present()
            || n < self.present_from + 2 * WINDOW as u64
            || 2.0 * 2.0 * power(before) < self.threshold * self.threshold
        {
            self.last_projection = f64::NAN;
            self.candidate = None;
            return None;
        }
        let projection = average.0 * before.0 + average.1 * before.1;
        if self.last_projection >= 0.0 && projection < 0.0 {
            let fraction = self.last_projection / (self.last_projection - projection);
            let crossing = (n - 1) as f64 + fraction;
            self.candidate = Some((crossing + 1.0 - window / 2.0, n + WINDOW as u64 / 2));
        }
        self.last_projection = projection;
        let (at, deadline) = self.candidate?;
        if projection < -SWING * power(before) {
            self.candidate = None;
            return Some(at);
        }
        if n > deadline {
            self.candidate = None;
        }
        None
    }
}
