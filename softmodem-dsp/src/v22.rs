// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.22: 1200 bit/s DPSK, full duplex on two channels, with the constant
//! carrier handshake of its § 6.3.1.

use std::collections::VecDeque;
use std::time::Duration;

use crate::SAMPLE_RATE;
use crate::dpsk::{Demodulator, Modulator, V22_HIGH_HZ, V22_LOW_HZ};
use crate::pump::{DataPump, Role};
use crate::qam;
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
const SCRAMBLED_SAMPLES: usize = SCRAMBLED_BITS * 8000 / BIT_RATE as usize;
pub(crate) const WAIT_SAMPLES: usize = 3648;
pub(crate) const SETTLE_SAMPLES: usize = 6120;
// Random decisions leave about 0.3; a trained equaliser on a poor line 0.04.
pub(crate) const LOCKED_ERROR: f64 = 0.1;
pub(crate) const UNLOCKED_ERROR: f64 = 0.2;

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
///
/// Over a long round trip, the caller shortens its 765 ms to what the
/// answerer can still need, as the answerer's own 765 ms started a round
/// trip earlier. Its data, the V.42 ODP first, then reaches an answerer
/// whose detection phase is still open.
#[derive(Debug)]
pub(crate) struct V22 {
    role: Role,
    phase: Phase,
    scrambled_from: usize,
    round_trip: Option<usize>,
    modulator: Modulator,
    demodulator: Demodulator,
    // Hears the data with an equaliser, as a mobile codec smears the signal.
    coherent: qam::Demodulator,
    // Its own, as the equaliser can settle a symbol later than the other.
    coherent_descrambler: Descrambler,
    locked: bool,
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
        let (modulator, receive_hz, guard, phase) = match role {
            Role::Originate => (
                Modulator::new(V22_LOW_HZ, LOW_CHANNEL_DBM0),
                V22_HIGH_HZ,
                None,
                Phase::Listening,
            ),
            Role::Answer => (
                Modulator::new(V22_HIGH_HZ, HIGH_CHANNEL_DBM0),
                V22_LOW_HZ,
                Some(Tone::new(GUARD_TONE_HZ, GUARD_TONE_DBM0)),
                Phase::UnscrambledOnes,
            ),
        };
        Self {
            role,
            phase,
            scrambled_from: 0,
            round_trip: None,
            modulator,
            demodulator: Demodulator::new(receive_hz),
            coherent: qam::Demodulator::new(receive_hz),
            coherent_descrambler: Descrambler::new(),
            locked: false,
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
        self.coherent.restart();
    }

    // § 6.3.1.1: the caller's 765 ms wait holds back only what it sends.
    fn receiving(&self) -> bool {
        match self.phase {
            Phase::Data => true,
            Phase::Settling { .. } => self.role == Role::Originate,
            _ => false,
        }
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
            Phase::ScrambledOnes if self.run.scrambled() && self.run.value => self.answered(),
            _ => {}
        }
    }

    // The answerer went to data 765 ms after it heard our scrambled ones.
    fn answered(&mut self) {
        let heard_after = self.sent - self.scrambled_from;
        self.round_trip = Some(heard_after.saturating_sub(2 * SCRAMBLED_SAMPLES));
        self.phase = Phase::Settling {
            until: self
                .sent
                .max(self.scrambled_from + SCRAMBLED_SAMPLES + SETTLE_SAMPLES),
        };
        self.coherent.restart();
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
            self.scrambled_from = self.sent;
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
        let mut coherent = Vec::new();
        self.coherent.process(input, &mut coherent);
        let descrambler = &mut self.coherent_descrambler;
        let coherent: Vec<bool> = coherent
            .into_iter()
            .map(|bit| descrambler.descramble(bit))
            .collect();
        let limit = if self.locked {
            UNLOCKED_ERROR
        } else {
            LOCKED_ERROR
        };
        self.locked = self.coherent.error() < limit;
        if self.locked && self.receiving() {
            bits.extend(coherent);
            for bit in line {
                self.descrambler.descramble(bit);
            }
            return;
        }
        for bit in line {
            if self.receiving() {
                bits.push(self.descrambler.descramble(bit));
            } else {
                self.heard(bit);
            }
        }
    }

    fn carrier(&self) -> bool {
        self.receiving() && self.demodulator.carrier()
    }

    fn engaged(&self) -> bool {
        !matches!(self.phase, Phase::Listening | Phase::UnscrambledOnes)
    }

    fn connected(&self) -> bool {
        self.phase == Phase::Data
    }

    #[expect(clippy::cast_precision_loss, reason = "a few seconds of samples")]
    fn round_trip(&self) -> Option<Duration> {
        self.round_trip
            .map(|samples| Duration::from_secs_f64(samples as f64 / SAMPLE_RATE))
    }

    fn stage(&self) -> String {
        let stage = match self.phase {
            Phase::Listening => "listening for unscrambled ones",
            Phase::Waiting { .. } => "waiting to send scrambled ones",
            Phase::UnscrambledOnes => "unscrambled ones",
            Phase::ScrambledOnes => "scrambled ones",
            Phase::Settling { .. } => "scrambled ones before data",
            Phase::Data => "data",
        };
        format!("V.22 {stage}")
    }
}

#[cfg(test)]
pub(crate) mod tests {
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

    const ECHO_SAMPLES: usize = 7;
    const ECHO_PERCENT: i32 = 70;
    const NOISE: i32 = 4000;

    /// A line with an echo half a symbol late, and noise.
    pub(crate) struct Smear {
        history: VecDeque<i16>,
        seed: u32,
    }

    impl Smear {
        pub(crate) fn new() -> Self {
            Self {
                history: VecDeque::new(),
                seed: 1,
            }
        }

        pub(crate) fn apply(&mut self, samples: &mut [i16]) {
            for sample in samples {
                self.history.push_back(*sample);
                let echo = if self.history.len() > ECHO_SAMPLES {
                    self.history.pop_front().unwrap_or_default()
                } else {
                    0
                };
                self.seed = self
                    .seed
                    .wrapping_mul(1_664_525)
                    .wrapping_add(1_013_904_223);
                let noise = i32::from((self.seed >> 16) as u16) - 32_768;
                let smeared = i32::from(*sample)
                    + i32::from(echo) * ECHO_PERCENT / 100
                    + noise * NOISE / 32_768;
                *sample = i16::try_from(smeared.clamp(-32_768, 32_767)).unwrap_or_default();
            }
        }
    }

    pub(crate) fn random_bits(count: usize) -> Vec<bool> {
        let mut seed = 7u32;
        (0..count)
            .map(|_| {
                seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                seed >> 16 & 1 == 1
            })
            .collect()
    }

    /// The fewest bits that differ between `message` and any stretch of `received`.
    pub(crate) fn errors(received: &[bool], message: &[bool]) -> usize {
        (0..received.len().saturating_sub(message.len()))
            .map(|at| {
                received[at..at + message.len()]
                    .iter()
                    .zip(message)
                    .filter(|(a, b)| a != b)
                    .count()
            })
            .min()
            .unwrap_or(message.len())
    }

    #[test]
    fn equalises_a_line_that_smears_the_symbols() {
        let mut caller = V22::new(Role::Originate);
        let mut answerer = V22::new(Role::Answer);
        let mut smear = Smear::new();
        let mut up = [0; FRAME];
        let mut down = [0; FRAME];
        let mut at_caller = Vec::new();
        let mut line = Vec::new();
        let message = random_bits(6000);
        for frame in 0..450 {
            if frame == 150 {
                answerer.push_bits(&message);
                at_caller.clear();
            }
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            smear.apply(&mut down);
            answerer.receive(&up, &mut Vec::new());
            caller.receive(&down, &mut at_caller);
            if frame >= 120 {
                line.extend_from_slice(&down);
            }
        }
        let mut differential = Demodulator::new(V22_HIGH_HZ);
        let mut descrambler = Descrambler::new();
        let mut bits = Vec::new();
        differential.process(&line, &mut bits);
        let bits: Vec<bool> = bits
            .into_iter()
            .map(|bit| descrambler.descramble(bit))
            .collect();
        assert_eq!(errors(&at_caller, &message), 0);
        assert!(
            errors(&bits, &message) > 10,
            "the line is too clean to tell"
        );
    }
}
