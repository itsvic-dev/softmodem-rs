//! V.22bis: 2400 bit/s QAM, full duplex on the V.22 channels, with the
//! handshake of its § 6.3.1 that falls back to V.22, and the retrain of § 6.4.

use std::collections::VecDeque;

use crate::dpsk::{self, V22_HIGH_HZ, V22_LOW_HZ, quarter_turns};
use crate::pump::{DataPump, Role};
use crate::qam::{self, Modulator, Rate};
use crate::scrambler::{Descrambler, Scrambler};
use crate::tone::Tone;
use crate::uart::Decoder;
use crate::v22::{
    GUARD_TONE_DBM0, GUARD_TONE_HZ, HIGH_CHANNEL_DBM0, LOW_CHANNEL_DBM0, Run, SETTLE_SAMPLES,
    USB1_BITS, WAIT_SAMPLES,
};

const S1_SAMPLES: usize = 800;
const S1_SYMBOLS: u32 = 20;
const DECIDE_16_SAMPLES: usize = 3600;
const FAST_SAMPLES: usize = 4800;
const FAST_READY_SAMPLES: usize = 1600;
const FAST_ONES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transmit {
    Silence,
    UnscrambledOnes,
    S1 { until: usize },
    Slow,
    Fast { since: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speed {
    Slow { data_at: usize },
    Fast { s1_end: usize },
}

#[derive(Debug, Default)]
struct S1Detector {
    run: u32,
    last: Option<u8>,
    heard: bool,
}

impl S1Detector {
    // Takes one symbol's quadrant change and tells whether S1 just ended.
    fn push(&mut self, turns: u8) -> bool {
        let fits = matches!(turns, 1 | 3);
        self.run = match (fits, self.last) {
            (true, Some(last)) if last != turns => self.run + 1,
            (true, _) => 1,
            (false, _) => 0,
        };
        self.last = Some(turns);
        if self.run >= S1_SYMBOLS {
            self.heard = true;
        }
        let ended = self.heard && self.run < 2;
        if ended {
            self.heard = false;
        }
        ended
    }
}

/// The answering end sends unscrambled ones. The caller hears them for
/// 155 ms, waits 456 ms, and sends S1 then scrambled ones. Each end that hears
/// S1 end, sends S1 back if it has not yet, and goes to 2400 bit/s 600 ms
/// later. An end that hears scrambled ones without S1 finishes the V.22
/// handshake at 1200 bit/s instead.
///
/// The handshake signals, S1 during data, and the V.22 fallback are received
/// by a differential demodulator, which needs no training. A coherent one
/// beside it trains from the end of S1 and receives 2400 bit/s.
#[derive(Debug)]
pub(crate) struct V22bis {
    role: Role,
    transmit: Transmit,
    speed: Option<Speed>,
    modulator: Modulator,
    slow: dpsk::Demodulator,
    fast: qam::Demodulator,
    receive_rate: Rate,
    guard: Option<Tone>,
    scrambler: Scrambler,
    slow_descrambler: Descrambler,
    fast_descrambler: Descrambler,
    queue: VecDeque<bool>,
    sent: usize,
    send_s1_at: Option<usize>,
    sent_s1: bool,
    s1: S1Detector,
    s1_phase: usize,
    ones: usize,
    run: Run,
    fast_ones: usize,
    ready: bool,
    connected: bool,
}

impl V22bis {
    pub(crate) fn new(role: Role) -> Self {
        let (modulator, receive_hz, guard, transmit) = match role {
            Role::Originate => (
                Modulator::new(V22_LOW_HZ, LOW_CHANNEL_DBM0),
                V22_HIGH_HZ,
                None,
                Transmit::Silence,
            ),
            Role::Answer => (
                Modulator::new(V22_HIGH_HZ, HIGH_CHANNEL_DBM0),
                V22_LOW_HZ,
                Some(Tone::new(GUARD_TONE_HZ, GUARD_TONE_DBM0)),
                Transmit::UnscrambledOnes,
            ),
        };
        let mut scrambler = Scrambler::new();
        scrambler.guard_against_lockup();
        Self {
            role,
            transmit,
            speed: None,
            modulator,
            slow: dpsk::Demodulator::new(receive_hz),
            fast: qam::Demodulator::new(receive_hz),
            receive_rate: Rate::Bps1200,
            guard,
            scrambler,
            slow_descrambler: Descrambler::new(),
            fast_descrambler: Descrambler::new(),
            queue: VecDeque::new(),
            sent: 0,
            send_s1_at: None,
            sent_s1: false,
            s1: S1Detector::default(),
            s1_phase: 0,
            ones: 0,
            run: Run::default(),
            fast_ones: 0,
            ready: false,
            connected: false,
        }
    }

    fn send_s1(&mut self) {
        self.transmit = Transmit::S1 {
            until: self.sent + S1_SAMPLES,
        };
        self.s1_phase = 0;
        self.sent_s1 = true;
    }

    fn s1_ended(&mut self) {
        if !self.sent_s1 {
            self.send_s1();
        }
        self.speed = Some(Speed::Fast { s1_end: self.sent });
        self.receive_rate = Rate::Bps1200;
        self.fast.restart();
        self.fast_ones = 0;
        self.ready = false;
    }

    fn advance(&mut self) {
        if let Some(at) = self.send_s1_at
            && self.sent >= at
        {
            self.send_s1_at = None;
            self.send_s1();
        }
        if let Transmit::S1 { until } = self.transmit
            && self.sent >= until
        {
            self.transmit = Transmit::Slow;
        }
        match self.speed {
            Some(Speed::Fast { s1_end }) => {
                if self.receive_rate == Rate::Bps1200 && self.sent >= s1_end + DECIDE_16_SAMPLES {
                    self.receive_rate = Rate::Bps2400;
                    self.fast.set_rate(Rate::Bps2400);
                }
                if self.transmit == Transmit::Slow && self.sent >= s1_end + FAST_SAMPLES {
                    self.transmit = Transmit::Fast { since: self.sent };
                }
                if let Transmit::Fast { since } = self.transmit
                    && self.fast_heard()
                    && self.sent >= since + FAST_READY_SAMPLES
                {
                    self.ready = true;
                    self.sent_s1 = false;
                }
            }
            Some(Speed::Slow { data_at }) if self.sent >= data_at => self.ready = true,
            _ => {}
        }
        self.connected |= self.ready;
    }

    fn handshake_bit(&mut self, line: bool, descrambled: bool) {
        self.ones = if line { self.ones + 1 } else { 0 };
        self.run.push(line, descrambled);
        if self.s1.heard {
            return;
        }
        match (self.role, self.transmit) {
            (Role::Originate, Transmit::Silence)
                if self.send_s1_at.is_none() && self.ones >= USB1_BITS =>
            {
                self.send_s1_at = Some(self.sent + WAIT_SAMPLES);
            }
            (Role::Answer, Transmit::UnscrambledOnes) if self.run.scrambled() => {
                self.transmit = Transmit::Slow;
                self.fall_back();
            }
            (Role::Originate, Transmit::Slow) if self.run.scrambled() && self.run.value => {
                self.fall_back();
            }
            _ => {}
        }
    }

    fn fall_back(&mut self) {
        self.speed = Some(Speed::Slow {
            data_at: self.sent + SETTLE_SAMPLES,
        });
    }

    fn fast_heard(&self) -> bool {
        self.fast_ones >= FAST_ONES
    }

    fn fast_bit(&mut self, descrambled: bool, bits: &mut Vec<bool>) {
        if self.fast_heard() {
            bits.push(descrambled);
        } else if self.receive_rate == Rate::Bps2400 {
            self.fast_ones = if descrambled { self.fast_ones + 1 } else { 0 };
        }
    }
}

impl DataPump for V22bis {
    fn bit_rate(&self) -> u32 {
        match self.speed {
            Some(Speed::Fast { .. }) => 2400,
            _ => 1200,
        }
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
        self.advance();
        self.sent += out.len();
        let ready = self.ready;
        match self.transmit {
            Transmit::Silence => out.fill(0),
            Transmit::UnscrambledOnes => self.modulator.render(out, Rate::Bps1200, || true),
            Transmit::S1 { .. } => {
                let phase = &mut self.s1_phase;
                self.modulator.render(out, Rate::Bps1200, || {
                    let bit = *phase % 4 >= 2;
                    *phase += 1;
                    bit
                });
            }
            Transmit::Slow | Transmit::Fast { .. } => {
                let rate = if matches!(self.transmit, Transmit::Fast { .. }) {
                    Rate::Bps2400
                } else {
                    Rate::Bps1200
                };
                let (scrambler, queue) = (&mut self.scrambler, &mut self.queue);
                self.modulator.render(out, rate, || {
                    let bit = if ready { queue.pop_front() } else { None };
                    scrambler.scramble(bit.unwrap_or(true))
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
        self.slow.process(input, &mut line);
        for dibit in line.chunks(2) {
            let s1_ended = self.s1.push(quarter_turns(dibit[0], dibit[1]));
            for &bit in dibit {
                let descrambled = self.slow_descrambler.descramble(bit);
                match self.speed {
                    Some(Speed::Slow { .. }) if self.ready => bits.push(descrambled),
                    None => self.handshake_bit(bit, descrambled),
                    _ => {}
                }
            }
            if s1_ended {
                self.s1_ended();
            }
        }

        line.clear();
        self.fast.process(input, &mut line);
        if matches!(self.speed, Some(Speed::Fast { .. })) {
            for bit in line {
                let descrambled = self.fast_descrambler.descramble(bit);
                self.fast_bit(descrambled, bits);
            }
        }
    }

    fn carrier(&self) -> bool {
        self.connected && self.fast.carrier()
    }

    fn engaged(&self) -> bool {
        self.speed.is_some() || self.send_s1_at.is_some() || self.sent_s1
    }

    fn connected(&self) -> bool {
        self.connected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v22::V22;

    const FRAME: usize = 160;

    fn exchange(
        caller: &mut dyn DataPump,
        answerer: &mut dyn DataPump,
        frames: usize,
    ) -> (Vec<bool>, Vec<bool>) {
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

    fn connect(caller: &mut dyn DataPump, answerer: &mut dyn DataPump) {
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) {
            exchange(caller, answerer, 1);
            frames += 1;
            assert!(frames < 250, "no connection after {} ms", frames * 20);
        }
    }

    fn crosses(caller: &mut dyn DataPump, answerer: &mut dyn DataPump) {
        let message: Vec<bool> = (0..1200).map(|n| n % 7 < 3).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange(caller, answerer, 100);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(found(&at_caller), "the caller lost the answerer's data");
        assert!(found(&at_answerer), "the answerer lost the caller's data");
    }

    #[test]
    fn two_ends_train_at_2400_and_carry_data() {
        let mut caller = V22bis::new(Role::Originate);
        let mut answerer = V22bis::new(Role::Answer);
        connect(&mut caller, &mut answerer);
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
        assert!(caller.carrier() && answerer.carrier());
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn answers_a_retrain_and_carries_data_again() {
        let mut caller = V22bis::new(Role::Originate);
        let mut answerer = V22bis::new(Role::Answer);
        connect(&mut caller, &mut answerer);
        caller.send_s1();
        caller.ready = false;
        exchange(&mut caller, &mut answerer, 20);
        assert!(!answerer.ready, "the answerer did not hear the retrain");
        exchange(&mut caller, &mut answerer, 60);
        assert!(caller.ready && answerer.ready, "the retrain did not finish");
        assert!(answerer.carrier(), "carrier dropped during the retrain");
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn a_v22_answerer_brings_it_down_to_1200() {
        let mut caller = V22bis::new(Role::Originate);
        let mut answerer = V22::new(Role::Answer);
        connect(&mut caller, &mut answerer);
        assert_eq!(caller.bit_rate(), 1200);
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn a_v22_caller_brings_it_down_to_1200() {
        let mut caller = V22::new(Role::Originate);
        let mut answerer = V22bis::new(Role::Answer);
        connect(&mut caller, &mut answerer);
        assert_eq!(answerer.bit_rate(), 1200);
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn recognises_s1_and_its_end() {
        let mut detector = S1Detector::default();
        let ended: Vec<bool> = (0..30)
            .map(|n| if n % 2 == 0 { 1 } else { 3 })
            .chain([0, 0])
            .map(|turns| detector.push(turns))
            .collect();
        assert_eq!(ended.iter().filter(|&&e| e).count(), 1);
        assert!(ended[30]);
    }

    #[test]
    fn unscrambled_ones_are_not_s1() {
        let mut detector = S1Detector::default();
        assert!(!(0..100).any(|_| detector.push(3)));
        assert!(!detector.heard);
    }
}
