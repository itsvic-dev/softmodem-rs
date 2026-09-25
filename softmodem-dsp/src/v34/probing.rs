// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The line probing signals L1 and L2 of § 10.1.2.4, and what a receiver
//! measures from them.

use std::f64::consts::TAU;

use super::NOMINAL_DBM0;
use crate::passband::Complex;
use crate::{SAMPLE_RATE, sine_peak, to_sample};

/// Table 17: each tone, and whether it starts at 180°.
pub const TONES: [(f64, bool); 21] = [
    (150.0, false),
    (300.0, true),
    (450.0, false),
    (600.0, false),
    (750.0, false),
    (1050.0, false),
    (1350.0, false),
    (1500.0, false),
    (1650.0, true),
    (1950.0, false),
    (2100.0, false),
    (2250.0, true),
    (2550.0, false),
    (2700.0, true),
    (2850.0, false),
    (3000.0, true),
    (3150.0, true),
    (3300.0, true),
    (3450.0, true),
    (3600.0, false),
    (3750.0, false),
];

/// L1 goes 6 dB above nominal, L2 at nominal.
pub const L1_DBM0: f64 = NOMINAL_DBM0 + 6.0;
pub const L2_DBM0: f64 = NOMINAL_DBM0;
/// L1 lasts 24 repetitions, 160 ms.
pub const L1_SAMPLES: usize = 1280;

// Three periods of 150 Hz, so each tone falls on a bin 50 Hz wide.
const BLOCK: usize = 160;
const BIN_HZ: f64 = SAMPLE_RATE / 160.0;
const OFFSET_TONE: usize = 5;
const OFFSET_MIN_SNR_DB: f64 = 20.0;

#[expect(clippy::cast_precision_loss, reason = "positions within a block")]
fn cycle() -> Vec<f64> {
    (0..BLOCK)
        .map(|n| {
            TONES
                .iter()
                .map(|&(hz, inverted)| {
                    let phase = if inverted { 0.5 } else { 0.0 };
                    (TAU * (hz * n as f64 / SAMPLE_RATE + phase)).cos()
                })
                .sum()
        })
        .collect()
}

#[expect(clippy::cast_precision_loss, reason = "21 tones")]
fn tone_count() -> f64 {
    TONES.len() as f64
}

/// Sends L1 or L2, whose mean power is that of a sine at the level given.
#[derive(Debug)]
pub struct Sender {
    cycle: Vec<f64>,
    position: usize,
    amplitude: f64,
}

impl Sender {
    #[must_use]
    pub fn new(level_dbm0: f64) -> Self {
        Self {
            cycle: cycle(),
            position: 0,
            amplitude: sine_peak(level_dbm0) / tone_count().sqrt(),
        }
    }

    pub fn render(&mut self, out: &mut [i16]) {
        for sample in out {
            *sample = to_sample(self.amplitude * self.cycle[self.position]);
            self.position = (self.position + 1) % BLOCK;
        }
    }
}

/// What one probing tone showed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneResult {
    pub hz: f64,
    /// Received against sent, in dB.
    pub gain_db: f64,
    /// Against the noise and distortion in the 150 Hz around the tone, as a
    /// data signal of the same power would meet them.
    pub snr_db: f64,
}

/// What line probing found, tone by tone in the order of `TONES`.
#[derive(Debug, Clone, PartialEq)]
pub struct Probing {
    pub tones: Vec<ToneResult>,
    /// Of the 1050 Hz tone, received against sent, if it could be measured.
    pub frequency_offset_hz: Option<f64>,
}

/// Measures L1 or L2 from the far end, block by block.
#[derive(Debug)]
pub struct Analyser {
    sent_tone_power: f64,
    block: Vec<f64>,
    powers: Vec<f64>,
    blocks: usize,
    last_offset_bin: Option<Complex>,
    offset_turns: Complex,
}

#[expect(
    clippy::cast_precision_loss,
    reason = "bins and positions within a block"
)]
fn dft(block: &[f64], bin: usize) -> Complex {
    block
        .iter()
        .enumerate()
        .fold((0.0, 0.0), |(re, im), (n, &x)| {
            let angle = TAU * (bin * n) as f64 / BLOCK as f64;
            (re + x * angle.cos(), im - x * angle.sin())
        })
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the tones are whole multiples of 50 Hz below 4 kHz"
)]
fn bin_of(hz: f64) -> usize {
    (hz / BIN_HZ).round() as usize
}

impl Analyser {
    /// For a signal sent at `sent_dbm0`.
    #[must_use]
    pub fn new(sent_dbm0: f64) -> Self {
        Self {
            sent_tone_power: sine_peak(sent_dbm0).powi(2) / 2.0 / tone_count(),
            block: Vec::with_capacity(BLOCK),
            powers: vec![0.0; BLOCK / 2],
            blocks: 0,
            last_offset_bin: None,
            offset_turns: (0.0, 0.0),
        }
    }

    pub fn push(&mut self, input: &[i16]) {
        for &sample in input {
            self.block.push(f64::from(sample));
            if self.block.len() == BLOCK {
                self.measure();
                self.block.clear();
            }
        }
    }

    fn measure(&mut self) {
        for (bin, power) in self.powers.iter_mut().enumerate().skip(1) {
            let (re, im) = dft(&self.block, bin);
            *power += re * re + im * im;
        }
        let offset_bin = dft(&self.block, bin_of(TONES[OFFSET_TONE].0));
        if let Some(last) = self.last_offset_bin {
            self.offset_turns.0 += offset_bin.0 * last.0 + offset_bin.1 * last.1;
            self.offset_turns.1 += offset_bin.1 * last.0 - offset_bin.0 * last.1;
        }
        self.last_offset_bin = Some(offset_bin);
        self.blocks += 1;
    }

    /// What the blocks so far showed, once there is one.
    #[must_use]
    #[expect(clippy::cast_precision_loss, reason = "a few hundred blocks at most")]
    pub fn result(&self) -> Option<Probing> {
        if self.blocks == 0 {
            return None;
        }
        let per_block = |bin: usize| self.powers[bin] / self.blocks as f64;
        let scale = (BLOCK as f64 / 2.0).powi(2);
        let tones: Vec<ToneResult> = TONES
            .iter()
            .map(|&(hz, _)| {
                let bin = bin_of(hz);
                let noise = f64::midpoint(per_block(bin - 1), per_block(bin + 1));
                let tone = (per_block(bin) - noise).max(f64::MIN_POSITIVE);
                ToneResult {
                    hz,
                    gain_db: 10.0 * (tone / scale / 2.0 / self.sent_tone_power).log10(),
                    snr_db: 10.0 * (tone / (3.0 * noise.max(f64::MIN_POSITIVE))).log10(),
                }
            })
            .collect();
        let frequency_offset_hz =
            (tones[OFFSET_TONE].snr_db >= OFFSET_MIN_SNR_DB && self.blocks > 1).then(|| {
                let turn = self.offset_turns.1.atan2(self.offset_turns.0) / TAU;
                turn * SAMPLE_RATE / BLOCK as f64
            });
        Some(Probing {
            tones,
            frequency_offset_hz,
        })
    }
}
