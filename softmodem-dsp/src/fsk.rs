//! Binary FSK: one tone for mark, another for space.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::correlator::Correlator;
use crate::{SAMPLE_RATE, sine_peak, to_sample};

/// One direction of an FSK link.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Channel {
    pub mark_hz: f64,
    pub space_hz: f64,
    pub baud: f64,
}

/// V.21 channel 1, sent by the originating modem.
pub const V21_ORIGINATE: Channel = Channel {
    mark_hz: 980.0,
    space_hz: 1180.0,
    baud: 300.0,
};

/// V.21 channel 2, sent by the answering modem.
pub const V21_ANSWER: Channel = Channel {
    mark_hz: 1650.0,
    space_hz: 1850.0,
    baud: 300.0,
};

/// The highest transmit level V.21 allows, from its § 6.
pub const V21_MAX_LEVEL_DBM0: f64 = -13.0;

impl Channel {
    fn samples_per_bit(&self) -> f64 {
        SAMPLE_RATE / self.baud
    }
}

/// Turns bits into phase-continuous FSK. The line idles on mark when no bits
/// are queued.
#[derive(Debug)]
pub struct Modulator {
    channel: Channel,
    amplitude: f64,
    phase: f64,
    bit_elapsed: f64,
    bit: bool,
    queue: VecDeque<bool>,
}

impl Modulator {
    #[must_use]
    pub fn new(channel: Channel, level_dbm0: f64) -> Self {
        Self {
            channel,
            amplitude: sine_peak(level_dbm0),
            phase: 0.0,
            bit_elapsed: 1.0,
            bit: true,
            queue: VecDeque::new(),
        }
    }

    pub fn push_bits(&mut self, bits: impl IntoIterator<Item = bool>) {
        self.queue.extend(bits);
    }

    /// Bits queued but not yet started.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    pub fn render(&mut self, out: &mut [i16]) {
        let bit_step = 1.0 / self.channel.samples_per_bit();
        for sample in out {
            if self.bit_elapsed >= 1.0 {
                self.bit_elapsed -= 1.0;
                self.bit = self.queue.pop_front().unwrap_or(true);
            }
            let hz = if self.bit {
                self.channel.mark_hz
            } else {
                self.channel.space_hz
            };
            *sample = to_sample(self.amplitude * (TAU * self.phase).sin());
            self.phase = (self.phase + hz / SAMPLE_RATE).fract();
            self.bit_elapsed += bit_step;
        }
    }
}

// V.21 § 8.3 and table 2, switched network: on in 300 to 700 ms, off in 20 to 80 ms.
const CARRIER_ON_DBM0: f64 = -43.0;
const CARRIER_OFF_DBM0: f64 = -48.0;
const CARRIER_ON_SAMPLES: u32 = 3200;
const CARRIER_OFF_SAMPLES: u32 = 400;
const TIMING_GAIN: f64 = 0.2;

/// Turns FSK back into bits. It recovers bit timing from transitions, so the
/// sender's clock may differ from ours, and it only yields bits while carrier
/// is present.
#[derive(Debug)]
pub struct Demodulator {
    mark: Correlator,
    space: Correlator,
    bit_step: f64,
    bit_phase: f64,
    last_decision: f64,
    carrier: bool,
    carrier_count: u32,
    carrier_on: f64,
    carrier_off: f64,
}

impl Demodulator {
    #[must_use]
    pub fn new(channel: Channel) -> Self {
        let window = channel.samples_per_bit().round();
        Self {
            mark: Correlator::new(channel.mark_hz, window),
            space: Correlator::new(channel.space_hz, window),
            bit_step: 1.0 / channel.samples_per_bit(),
            bit_phase: 0.0,
            last_decision: 0.0,
            carrier: false,
            carrier_count: 0,
            carrier_on: sine_peak(CARRIER_ON_DBM0),
            carrier_off: sine_peak(CARRIER_OFF_DBM0),
        }
    }

    #[must_use]
    pub fn carrier(&self) -> bool {
        self.carrier
    }

    /// Appends the bits found in `input` to `bits`.
    pub fn process(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        for &sample in input {
            let x = f64::from(sample);
            let mark = self.mark.push(x);
            let space = self.space.push(x);
            let level = 2.0 * (mark + space).sqrt();
            self.track_carrier(level);

            let present = level >= self.carrier_off;
            let decision = if present {
                (mark - space) / (mark + space)
            } else {
                1.0
            };
            if present && (decision > 0.0) != (self.last_decision > 0.0) {
                let error = if self.bit_phase < 0.5 {
                    self.bit_phase
                } else {
                    self.bit_phase - 1.0
                };
                self.bit_phase -= TIMING_GAIN * error;
            }
            self.last_decision = decision;

            let before = self.bit_phase.rem_euclid(1.0);
            self.bit_phase = (before + self.bit_step).rem_euclid(1.0);
            if before < 0.5 && self.bit_phase >= 0.5 && self.carrier {
                bits.push(decision > 0.0);
            }
        }
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
            CARRIER_OFF_SAMPLES
        } else {
            CARRIER_ON_SAMPLES
        };
        if self.carrier_count >= needed {
            self.carrier = !self.carrier;
            self.carrier_count = 0;
        }
    }
}
