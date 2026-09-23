//! V.22: 1200 bit/s DPSK, full duplex on two channels, with the constant
//! carrier handshake of its § 6.3.1.

use std::collections::VecDeque;

use crate::dpsk::{Demodulator, Modulator, V22_HIGH_HZ, V22_LOW_HZ};
use crate::pump::{DataPump, Role};
use crate::scrambler::{Descrambler, Scrambler};
use crate::tone::Tone;
use crate::uart::Decoder;

const BIT_RATE: u32 = 1200;
pub(crate) const GUARD_TONE_HZ: f64 = 1800.0;
// V.2 allows -13 dBm0 in all; the guard tone is 6 dB below the high channel data.
pub(crate) const LOW_CHANNEL_DBM0: f64 = -13.0;
pub(crate) const HIGH_CHANNEL_DBM0: f64 = -13.97;
pub(crate) const GUARD_TONE_DBM0: f64 = -19.97;

pub(crate) const USB1_BITS: usize = 186;
const SCRAMBLED_BITS: usize = 324;
pub(crate) const WAIT_SAMPLES: usize = 3648;
pub(crate) const SETTLE_SAMPLES: usize = 6120;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listening,
    Waiting { until: usize },
    UnscrambledOnes,
    ScrambledOnes,
    Settling { until: usize },
    Data,
}

// Unscrambled ones descramble to ones too, so scrambled ones need varied line bits.
#[derive(Debug, Default)]
pub(crate) struct Run {
    pub(crate) value: bool,
    length: usize,
    line_zeros: usize,
}

impl Run {
    pub(crate) fn push(&mut self, line: bool, descrambled: bool) {
        if descrambled != self.value || self.length == 0 {
            *self = Self {
                value: descrambled,
                ..Self::default()
            };
        }
        self.length += 1;
        self.line_zeros += usize::from(!line);
    }

    pub(crate) fn scrambled(&self) -> bool {
        self.length >= SCRAMBLED_BITS && self.line_zeros >= self.length / 4
    }
}

/// The answering end sends unscrambled ones until it hears the caller's
/// scrambled ones, then scrambled ones for 765 ms before data. The caller
/// stays silent until it has heard unscrambled ones for 155 ms, waits
/// 456 ms, sends scrambled ones, and goes to data 765 ms after it hears them
/// back.
#[derive(Debug)]
pub(crate) struct V22 {
    phase: Phase,
    modulator: Modulator,
    demodulator: Demodulator,
    guard: Option<Tone>,
    scrambler: Scrambler,
    descrambler: Descrambler,
    queue: VecDeque<bool>,
    sent: usize,
    ones: usize,
    run: Run,
}

impl V22 {
    pub(crate) fn new(role: Role) -> Self {
        let (modulator, demodulator, guard, phase) = match role {
            Role::Originate => (
                Modulator::new(V22_LOW_HZ, LOW_CHANNEL_DBM0),
                Demodulator::new(V22_HIGH_HZ),
                None,
                Phase::Listening,
            ),
            Role::Answer => (
                Modulator::new(V22_HIGH_HZ, HIGH_CHANNEL_DBM0),
                Demodulator::new(V22_LOW_HZ),
                Some(Tone::new(GUARD_TONE_HZ, GUARD_TONE_DBM0)),
                Phase::UnscrambledOnes,
            ),
        };
        Self {
            phase,
            modulator,
            demodulator,
            guard,
            scrambler: Scrambler::new(),
            descrambler: Descrambler::new(),
            queue: VecDeque::new(),
            sent: 0,
            ones: 0,
            run: Run::default(),
        }
    }

    fn settle(&mut self) {
        self.phase = Phase::Settling {
            until: self.sent + SETTLE_SAMPLES,
        };
    }

    fn heard(&mut self, line: bool) {
        let descrambled = self.descrambler.descramble(line);
        self.ones = if line { self.ones + 1 } else { 0 };
        self.run.push(line, descrambled);
        match self.phase {
            Phase::Listening if self.ones >= USB1_BITS => {
                self.phase = Phase::Waiting {
                    until: self.sent + WAIT_SAMPLES,
                };
            }
            Phase::UnscrambledOnes if self.run.scrambled() => self.settle(),
            Phase::ScrambledOnes if self.run.scrambled() && self.run.value => self.settle(),
            _ => {}
        }
    }
}

impl DataPump for V22 {
    fn bit_rate(&self) -> u32 {
        BIT_RATE
    }

    fn decoder(&self) -> Decoder {
        Decoder::v14()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        self.queue.extend(bits);
    }

    fn pending(&self) -> usize {
        self.queue.len()
    }

    fn transmit(&mut self, out: &mut [i16]) {
        if let Phase::Waiting { until } = self.phase
            && self.sent >= until
        {
            self.phase = Phase::ScrambledOnes;
        }
        if let Phase::Settling { until } = self.phase
            && self.sent >= until
        {
            self.phase = Phase::Data;
            self.scrambler.guard_against_lockup();
        }
        self.sent += out.len();

        match self.phase {
            Phase::Listening | Phase::Waiting { .. } => out.fill(0),
            Phase::UnscrambledOnes => self.modulator.render(out, || true),
            Phase::ScrambledOnes | Phase::Settling { .. } => {
                let scrambler = &mut self.scrambler;
                self.modulator.render(out, || scrambler.scramble(true));
            }
            Phase::Data => {
                let (scrambler, queue) = (&mut self.scrambler, &mut self.queue);
                self.modulator.render(out, || {
                    scrambler.scramble(queue.pop_front().unwrap_or(true))
                });
            }
        }
        if let Some(guard) = &mut self.guard {
            let mut tone = vec![0; out.len()];
            guard.render(&mut tone);
            for (sample, tone) in out.iter_mut().zip(tone) {
                *sample = sample.saturating_add(tone);
            }
        }
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        let mut line = Vec::new();
        self.demodulator.process(input, &mut line);
        for bit in line {
            if self.phase == Phase::Data {
                bits.push(self.descrambler.descramble(bit));
            } else {
                self.heard(bit);
            }
        }
    }

    fn carrier(&self) -> bool {
        self.phase == Phase::Data && self.demodulator.carrier()
    }

    fn engaged(&self) -> bool {
        !matches!(self.phase, Phase::Listening | Phase::UnscrambledOnes)
    }

    fn connected(&self) -> bool {
        self.phase == Phase::Data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 160;

    fn exchange(caller: &mut V22, answerer: &mut V22, frames: usize) -> (Vec<bool>, Vec<bool>) {
        let mut up = [0; FRAME];
        let mut down = [0; FRAME];
        let mut at_caller = Vec::new();
        let mut at_answerer = Vec::new();
        for _ in 0..frames {
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            answerer.receive(&up, &mut at_answerer);
            caller.receive(&down, &mut at_caller);
        }
        (at_caller, at_answerer)
    }

    #[test]
    fn two_ends_train_within_the_handshake_times() {
        let mut caller = V22::new(Role::Originate);
        let mut answerer = V22::new(Role::Answer);
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) {
            exchange(&mut caller, &mut answerer, 1);
            frames += 1;
            assert!(frames < 150, "no connection after {} ms", frames * 20);
        }
        assert!(caller.carrier() && answerer.carrier());
    }

    #[test]
    fn the_caller_does_not_take_unscrambled_ones_for_scrambled() {
        let mut caller = V22::new(Role::Originate);
        let mut answerer = V22::new(Role::Answer);
        let mut up = [0; FRAME];
        let mut down = [0; FRAME];
        for _ in 0..300 {
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            caller.receive(&down, &mut Vec::new());
        }
        assert_eq!(caller.phase, Phase::ScrambledOnes);
    }

    #[test]
    fn data_crosses_both_ways_once_trained() {
        let mut caller = V22::new(Role::Originate);
        let mut answerer = V22::new(Role::Answer);
        exchange(&mut caller, &mut answerer, 150);
        let message: Vec<bool> = (0..600).map(|n| n % 7 < 3).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange(&mut caller, &mut answerer, 50);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(found(&at_caller), "the caller lost the answerer's data");
        assert!(found(&at_answerer), "the answerer lost the caller's data");
    }
}
