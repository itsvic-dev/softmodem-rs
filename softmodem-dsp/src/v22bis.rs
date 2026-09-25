// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.22bis: 2400 bit/s QAM, full duplex on the V.22 channels, with the
//! handshake of its § 6.3.1 that falls back to V.22, the retrain of § 6.4 and
//! the rate change of § 6.6.

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
const DECIDE_SAMPLES: usize = 3600;
const CHANGE_SAMPLES: usize = 4800;
const READY_SAMPLES: usize = 1600;
const READY_ONES: usize = 32;
const RATE_DIBITS: u32 = 32;
// § 6.4: repeat a retrain unanswered within the longest two-way delay.
const REPEAT_SAMPLES: usize = 9600;
const REPLY_SAMPLES: usize = 8000;
// Random decisions leave (0.632)²/6 ≈ 0.067, the most this error can read.
const LOST_ERROR: f64 = 0.045;
const LOST_SAMPLES: usize = 2400;
const STEP_DOWN_SAMPLES: usize = 80_000;

type Dibit = (bool, bool);

// Table 4: the dibit after S1 that names the rate.
const FAST_DIBIT: Dibit = (true, true);
const SLOW_DIBIT: Dibit = (false, true);

fn rate_dibit(rate: Rate) -> Dibit {
    match rate {
        Rate::Bps2400 => FAST_DIBIT,
        Rate::Bps1200 => SLOW_DIBIT,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transmit {
    Silence,
    UnscrambledOnes,
    S1 { until: usize },
    Slow,
    Fast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Speed {
    Slow { data_at: usize },
    Trained { t0: usize, rate: Option<Rate> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exchange {
    Idle,
    Initiated { repeat_at: usize },
    Answering,
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

#[derive(Debug, Default)]
struct RateDibits {
    last: Option<Dibit>,
    run: u32,
}

impl RateDibits {
    fn push(&mut self, dibit: Dibit) {
        if self.last == Some(dibit) {
            self.run += 1;
        } else {
            self.last = Some(dibit);
            self.run = 1;
        }
    }

    fn rate(&self) -> Rate {
        if self.last == Some(FAST_DIBIT) {
            Rate::Bps2400
        } else {
            Rate::Bps1200
        }
    }
}

/// The answering end sends unscrambled ones. The caller hears them for
/// 155 ms, waits 456 ms, and sends S1 then scrambled ones. Each end that hears
/// S1 end, sends S1 back if it has not yet, and goes to 2400 bit/s 600 ms
/// later. An end that hears scrambled ones without S1 finishes the V.22
/// handshake at 1200 bit/s instead.
///
/// A retrain and a rate change are the same exchange during data: S1, then
/// the dibit that names the rate, answered by S1 and the same dibit. This end
/// starts one when it loses equalisation, and asks for 1200 bit/s when it
/// loses it again soon after.
///
/// The handshake signals, S1 during data, and 1200 bit/s are received by a
/// differential demodulator, which needs no training. A coherent one beside
/// it trains from the end of S1 and receives 2400 bit/s.
#[derive(Debug)]
pub(crate) struct V22bis {
    role: Role,
    transmit: Transmit,
    speed: Option<Speed>,
    exchange: Exchange,
    modulator: Modulator,
    slow: dpsk::Demodulator,
    fast: qam::Demodulator,
    guard: Option<Tone>,
    scrambler: Scrambler,
    slow_descrambler: Descrambler,
    fast_descrambler: Descrambler,
    queue: VecDeque<bool>,
    sent: usize,
    send_s1_at: Option<usize>,
    s1_sent_until: Option<usize>,
    s1: S1Detector,
    s1_phase: usize,
    dibit: Dibit,
    heard_dibits: RateDibits,
    changed_at: Option<usize>,
    ones: usize,
    run: Run,
    ready_ones: usize,
    rate: Rate,
    lost_since: Option<usize>,
    retrained_at: Option<usize>,
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
            exchange: Exchange::Idle,
            modulator,
            slow: dpsk::Demodulator::new(receive_hz),
            fast: qam::Demodulator::new(receive_hz),
            guard,
            scrambler,
            slow_descrambler: Descrambler::new(),
            fast_descrambler: Descrambler::new(),
            queue: VecDeque::new(),
            sent: 0,
            send_s1_at: None,
            s1_sent_until: None,
            s1: S1Detector::default(),
            s1_phase: 0,
            dibit: FAST_DIBIT,
            heard_dibits: RateDibits::default(),
            changed_at: None,
            ones: 0,
            run: Run::default(),
            ready_ones: 0,
            rate: Rate::Bps1200,
            lost_since: None,
            retrained_at: None,
            ready: false,
            connected: false,
        }
    }

    fn send_s1(&mut self) {
        let until = self.sent + S1_SAMPLES;
        self.transmit = Transmit::S1 { until };
        self.s1_phase = 0;
        self.s1_sent_until = Some(until);
    }

    // § 6.6.1 f): an S1 heard soon after ours is the reply, not a new request.
    fn replies_to_ours(&self) -> bool {
        self.s1_sent_until
            .is_some_and(|until| self.sent < until + REPLY_SAMPLES)
    }

    /// Starts a retrain, or a rate change to `rate`, as §§ 6.4 and 6.6 have it.
    fn initiate(&mut self, rate: Rate) {
        self.dibit = rate_dibit(rate);
        self.send_s1();
        self.exchange = Exchange::Initiated {
            repeat_at: self.sent + S1_SAMPLES + REPEAT_SAMPLES,
        };
        self.ready = false;
        self.ready_ones = 0;
        self.lost_since = None;
    }

    fn train_from(&mut self, t0: usize) {
        self.speed = Some(Speed::Trained { t0, rate: None });
        self.exchange = Exchange::Idle;
        self.changed_at = None;
        self.ready_ones = 0;
        self.fast.restart();
        self.ready = false;
    }

    fn s1_ended(&mut self) {
        self.heard_dibits = RateDibits::default();
        match self.exchange {
            _ if !self.connected => {
                if self.s1_sent_until.is_none() {
                    self.send_s1();
                }
                self.train_from(self.sent);
            }
            Exchange::Initiated { .. } => self.train_from(self.sent),
            Exchange::Idle if self.replies_to_ours() => self.train_from(self.sent),
            Exchange::Idle | Exchange::Answering => {
                self.exchange = Exchange::Answering;
                self.ready = false;
                self.ready_ones = 0;
            }
        }
    }

    fn advance(&mut self) {
        if let Some(at) = self.send_s1_at
            && self.sent >= at
        {
            self.send_s1_at = None;
            self.send_s1();
        }
        if let Exchange::Initiated { repeat_at } = self.exchange
            && self.sent >= repeat_at
        {
            let rate = if self.dibit == FAST_DIBIT {
                Rate::Bps2400
            } else {
                Rate::Bps1200
            };
            self.initiate(rate);
        }
        if let Transmit::S1 { until } = self.transmit
            && self.sent >= until
        {
            self.transmit = Transmit::Slow;
        }
        match self.speed {
            Some(Speed::Trained { t0, rate }) if self.exchange == Exchange::Idle => {
                self.advance_trained(t0, rate);
            }
            Some(Speed::Slow { data_at }) if self.sent >= data_at => self.ready = true,
            _ => {}
        }
        self.watch_equalisation();
        self.connected |= self.ready;
    }

    fn advance_trained(&mut self, t0: usize, rate: Option<Rate>) {
        let rate = match rate {
            None if self.sent >= t0 + DECIDE_SAMPLES => {
                let rate = self.heard_dibits.rate();
                self.fast.set_rate(rate);
                self.speed = Some(Speed::Trained {
                    t0,
                    rate: Some(rate),
                });
                rate
            }
            None => return,
            Some(rate) => rate,
        };
        if self.changed_at.is_none() && self.sent >= t0 + CHANGE_SAMPLES {
            self.changed_at = Some(self.sent);
            self.rate = rate;
            self.dibit = FAST_DIBIT;
            self.transmit = match rate {
                Rate::Bps2400 => Transmit::Fast,
                Rate::Bps1200 => Transmit::Slow,
            };
        }
        if let Some(at) = self.changed_at
            && !self.ready
            && self.ready_ones >= READY_ONES
            && self.sent >= at + READY_SAMPLES
        {
            self.ready = true;
            self.s1_sent_until = None;
            if self.connected {
                self.retrained_at = Some(self.sent);
            }
        }
    }

    fn watch_equalisation(&mut self) {
        let fast_data = self.ready
            && matches!(
                self.speed,
                Some(Speed::Trained {
                    rate: Some(Rate::Bps2400),
                    ..
                })
            );
        if !fast_data || self.fast.error() < LOST_ERROR {
            self.lost_since = None;
            return;
        }
        let since = *self.lost_since.get_or_insert(self.sent);
        if self.sent - since < LOST_SAMPLES {
            return;
        }
        let soon = self
            .retrained_at
            .is_some_and(|at| self.sent - at < STEP_DOWN_SAMPLES);
        self.initiate(if soon { Rate::Bps1200 } else { Rate::Bps2400 });
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

    fn trained_rate(&self) -> Option<Rate> {
        match self.speed {
            Some(Speed::Trained { rate, .. }) if self.exchange == Exchange::Idle => rate,
            _ => None,
        }
    }

    // Received data flows before this end may send, as in V.22.
    fn receiving(&self) -> bool {
        match self.speed {
            Some(Speed::Slow { .. }) => self.role == Role::Originate || self.ready,
            Some(Speed::Trained { .. }) => {
                self.trained_rate().is_some() && self.ready_ones >= READY_ONES
            }
            None => false,
        }
    }

    fn slow_dibit(&mut self, dibit: Dibit, bits: &mut Vec<bool>) {
        let descrambled = (
            self.slow_descrambler.descramble(dibit.0),
            self.slow_descrambler.descramble(dibit.1),
        );
        match self.speed {
            None => {
                self.handshake_bit(dibit.0, descrambled.0);
                self.handshake_bit(dibit.1, descrambled.1);
            }
            Some(Speed::Slow { .. }) if self.receiving() => {
                bits.extend([descrambled.0, descrambled.1]);
            }
            Some(Speed::Slow { .. }) => {}
            Some(Speed::Trained { .. }) => {
                self.heard_dibits.push(descrambled);
                if self.exchange == Exchange::Answering && self.heard_dibits.run >= RATE_DIBITS {
                    let asked = self.heard_dibits.rate();
                    self.dibit = rate_dibit(asked);
                    self.send_s1();
                    self.train_from(self.sent);
                } else if self.trained_rate() == Some(Rate::Bps1200) {
                    self.ready_bit(descrambled.0, bits);
                    self.ready_bit(descrambled.1, bits);
                }
            }
        }
    }

    // §§ 6.3.1.1 f) and 6.6.1 e): data after 32 ones in a row at the new rate.
    fn ready_bit(&mut self, bit: bool, bits: &mut Vec<bool>) {
        if self.ready_ones >= READY_ONES {
            bits.push(bit);
        } else {
            self.ready_ones = if bit { self.ready_ones + 1 } else { 0 };
        }
    }
}

impl DataPump for V22bis {
    fn bit_rate(&self) -> u32 {
        match (self.speed, self.rate) {
            (Some(Speed::Trained { .. }), Rate::Bps2400) => 2400,
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

    fn retrain(&mut self) {
        if self.connected && matches!(self.speed, Some(Speed::Trained { .. })) {
            self.initiate(Rate::Bps2400);
        }
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
            Transmit::Slow => {
                let (scrambler, queue) = (&mut self.scrambler, &mut self.queue);
                let (dibit, phase) = (self.dibit, &mut self.s1_phase);
                self.modulator.render(out, Rate::Bps1200, || {
                    let idle = if *phase % 2 == 1 { dibit.1 } else { dibit.0 };
                    *phase += 1;
                    let bit = if ready { queue.pop_front() } else { None };
                    scrambler.scramble(bit.unwrap_or(idle))
                });
            }
            Transmit::Fast => {
                let (scrambler, queue) = (&mut self.scrambler, &mut self.queue);
                self.modulator.render(out, Rate::Bps2400, || {
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
            self.slow_dibit((dibit[0], dibit[1]), bits);
            if s1_ended {
                self.s1_ended();
            }
        }

        line.clear();
        self.fast.process(input, &mut line);
        let fast = matches!(
            self.speed,
            Some(Speed::Trained {
                rate: Some(Rate::Bps2400),
                ..
            })
        );
        if fast && self.exchange == Exchange::Idle {
            for bit in line {
                let descrambled = self.fast_descrambler.descramble(bit);
                self.ready_bit(descrambled, bits);
            }
        }
    }

    fn carrier(&self) -> bool {
        (self.connected || self.receiving()) && self.fast.carrier()
    }

    fn engaged(&self) -> bool {
        self.speed.is_some() || self.send_s1_at.is_some() || self.s1_sent_until.is_some()
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

    struct Noise(u64);

    impl Noise {
        #[expect(clippy::cast_precision_loss, reason = "a test signal")]
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        }
    }

    fn exchange_with(
        caller: &mut dyn DataPump,
        answerer: &mut dyn DataPump,
        frames: usize,
        mut line: impl FnMut(&mut [i16]),
    ) -> (Vec<bool>, Vec<bool>) {
        let mut up = [0; FRAME];
        let mut down = [0; FRAME];
        let mut at_caller = Vec::new();
        let mut at_answerer = Vec::new();
        for _ in 0..frames {
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            line(&mut up);
            line(&mut down);
            answerer.receive(&up, &mut at_answerer);
            caller.receive(&down, &mut at_caller);
        }
        (at_caller, at_answerer)
    }

    fn exchange(
        caller: &mut dyn DataPump,
        answerer: &mut dyn DataPump,
        frames: usize,
    ) -> (Vec<bool>, Vec<bool>) {
        exchange_with(caller, answerer, frames, |_| {})
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

    fn settle(caller: &mut V22bis, answerer: &mut V22bis) {
        let mut frames = 0;
        while !(caller.ready && answerer.ready) {
            exchange(caller, answerer, 1);
            frames += 1;
            assert!(
                frames < 150,
                "the exchange did not finish in {} ms",
                frames * 20
            );
        }
    }

    fn trained() -> (V22bis, V22bis) {
        let mut caller = V22bis::new(Role::Originate);
        let mut answerer = V22bis::new(Role::Answer);
        connect(&mut caller, &mut answerer);
        (caller, answerer)
    }

    #[test]
    fn two_ends_train_at_2400_and_carry_data() {
        let (mut caller, mut answerer) = trained();
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
        assert!(caller.carrier() && answerer.carrier());
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn answers_a_retrain_and_carries_data_again() {
        let (mut caller, mut answerer) = trained();
        caller.retrain();
        exchange(&mut caller, &mut answerer, 20);
        assert!(!answerer.ready, "the answerer did not hear the retrain");
        assert!(answerer.carrier(), "carrier dropped during the retrain");
        settle(&mut caller, &mut answerer);
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn changes_rate_down_to_1200_and_back_up() {
        let (mut caller, mut answerer) = trained();
        answerer.initiate(Rate::Bps1200);
        settle(&mut caller, &mut answerer);
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (1200, 1200));
        crosses(&mut caller, &mut answerer);

        caller.retrain();
        exchange(&mut caller, &mut answerer, 5);
        settle(&mut caller, &mut answerer);
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
        crosses(&mut caller, &mut answerer);
    }

    #[test]
    fn repeats_s1_until_the_far_end_answers() {
        let (mut caller, _answerer) = trained();
        caller.retrain();
        let mut out = [0; FRAME];
        let mut s1_starts = 0;
        for _ in 0..150 {
            let was_s1 = matches!(caller.transmit, Transmit::S1 { .. });
            caller.transmit(&mut out);
            caller.receive(&[0; FRAME], &mut Vec::new());
            if !was_s1 && matches!(caller.transmit, Transmit::S1 { .. }) {
                s1_starts += 1;
            }
        }
        assert!(s1_starts >= 2, "S1 was sent again {s1_starts} times in 3 s");
    }

    #[test]
    fn retrains_when_noise_ruins_equalisation_then_steps_down() {
        let (mut caller, mut answerer) = trained();
        let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
        let mut burst = |frames: usize, caller: &mut V22bis, answerer: &mut V22bis| {
            exchange_with(caller, answerer, frames, |samples| {
                for sample in samples.iter_mut() {
                    *sample = sample.saturating_add(crate::to_sample(9000.0 * noise.next()));
                }
            });
        };

        let retrained_at = (caller.retrained_at, answerer.retrained_at);
        burst(40, &mut caller, &mut answerer);
        settle(&mut caller, &mut answerer);
        assert_ne!(
            (caller.retrained_at, answerer.retrained_at),
            retrained_at,
            "nobody retrained"
        );
        assert_eq!(caller.bit_rate(), 2400, "the first retrain stepped down");

        burst(40, &mut caller, &mut answerer);
        settle(&mut caller, &mut answerer);
        assert_eq!(
            (caller.bit_rate(), answerer.bit_rate()),
            (1200, 1200),
            "a second loss soon after did not step down"
        );
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
