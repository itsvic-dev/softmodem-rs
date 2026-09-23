//! Binary FSK: one tone for mark, another for space.

use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::{SAMPLE_RATE, to_sample};

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
    pub fn new(channel: Channel, amplitude: i16) -> Self {
        Self {
            channel,
            amplitude: f64::from(amplitude),
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

// Tone peak on the i16 scale: about -43 dBm0 on and -48 dBm0 off, per V.21.
const CARRIER_ON: f64 = 160.0;
const CARRIER_OFF: f64 = 90.0;
const CARRIER_ON_SAMPLES: u32 = 80;
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

            let decision = if level < CARRIER_OFF {
                1.0
            } else {
                (mark - space) / (mark + space)
            };
            if (decision > 0.0) != (self.last_decision > 0.0) && level >= CARRIER_OFF {
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
            level < CARRIER_OFF
        } else {
            level > CARRIER_ON
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

#[derive(Debug)]
struct Correlator {
    step: f64,
    phase: f64,
    history: VecDeque<(f64, f64)>,
    sum: (f64, f64),
    window: f64,
}

impl Correlator {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the window is a small positive whole number"
    )]
    fn new(hz: f64, window: f64) -> Self {
        Self {
            step: hz / SAMPLE_RATE,
            phase: 0.0,
            history: VecDeque::from(vec![(0.0, 0.0); window as usize]),
            sum: (0.0, 0.0),
            window,
        }
    }

    fn push(&mut self, x: f64) -> f64 {
        let angle = TAU * self.phase;
        let product = (x * angle.cos(), -x * angle.sin());
        self.phase = (self.phase + self.step).fract();

        let (old_re, old_im) = self.history.pop_front().unwrap_or_default();
        self.history.push_back(product);
        self.sum.0 += product.0 - old_re;
        self.sum.1 += product.1 - old_im;

        let re = self.sum.0 / self.window;
        let im = self.sum.1 / self.window;
        re * re + im * im
    }
}
