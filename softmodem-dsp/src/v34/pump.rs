//! The V.34 data pump: phase 2, the training of phases 3 and 4 as the
//! error-free procedures of § 11.3 and § 11.4 give it, and data mode.

use std::collections::VecDeque;

use super::decoder::Decoder;
use super::detect::{Heard, SDetector};
use super::encoder::{Encoder, Settings};
use super::framing::Framing;
use super::modulator::Modulator;
use super::mp::{self, Mp, Trellis};
use super::phase2::{Outcome, Phase2};
use super::receiver::{DELAY, Equalizer, FrontEnd, Symbol, Tracking};
use super::training::{self, J_4, J_16, J_PRIME, PP_SYMBOLS, Points};
use super::{NOMINAL_DBM0, rates};
use crate::passband::Complex;
use crate::pump::{DataPump, Role};
use crate::scrambler::{Descrambler, Polynomial, Scrambler};
use crate::sine_peak;
use crate::uart::Decoder as Characters;

const S_SYMBOLS: usize = 128;
const S_BAR_SYMBOLS: usize = 16;
// § 11.3.1: TRN for at least 512T; this modem sends twice that in phase 3.
const TRN_SYMBOLS: usize = 1024;
const TRN_LEAST: usize = 512;
// § 11.3.1.2.1: the answer modem waits 70 ± 5 ms after INFO1a.
const SILENCE_MS: f64 = 70.0;
const HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
const CARRIER_DBM0: f64 = -43.0;

fn polynomial(role: Role) -> Polynomial {
    match role {
        Role::Originate => Polynomial::V34_CALL,
        Role::Answer => Polynomial::V34_ANSWER,
    }
}

fn other(role: Role) -> Role {
    match role {
        Role::Originate => Role::Answer,
        Role::Answer => Role::Originate,
    }
}

fn nearest_odd(value: f64) -> f64 {
    2.0 * ((value - 1.0) / 2.0).round() + 1.0
}

fn phase_3_start() -> Vec<Complex> {
    (0..S_SYMBOLS)
        .map(training::s)
        .chain((0..S_BAR_SYMBOLS).map(training::s_bar))
        .chain((0..PP_SYMBOLS).map(training::pp))
        .collect()
}

/// What the receive side has heard that the transmit side acts on.
#[derive(Debug, Default)]
struct Far {
    /// S̄ heard from the far end: 1 in phase 3, 2 once its phase 4 begins.
    s_bar: u8,
    /// J, and the constellation it asks this end to train with.
    j: Option<Points>,
    /// At least 512T of the far phase 4 TRN.
    trn: bool,
    mp: Option<Mp>,
    /// MP′ or E from the far end.
    ack: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Send {
    /// Silence until the receive side hears what this end waits for.
    Wait,
    Trn {
        sent: usize,
    },
    J,
    Mp,
    Data,
}

/// The symbols this end sends from phase 3 on, of unit mean power.
#[derive(Debug)]
struct Source {
    role: Role,
    send: Send,
    phase: u8,
    queue: VecDeque<Complex>,
    training: training::Sender,
    points: Points,
    mp: Mp,
    acked: bool,
    encoder: Option<Encoder>,
    scale: f64,
    scrambler: Scrambler,
    bits: VecDeque<bool>,
    b1_frames: usize,
    b1_sent: bool,
}

impl Source {
    fn new(role: Role, silence: usize, mp: Mp) -> Self {
        let mut queue = VecDeque::from(vec![(0.0, 0.0); silence]);
        let send = if role == Role::Answer {
            queue.extend(phase_3_start());
            Send::Trn { sent: 0 }
        } else {
            Send::Wait
        };
        Self {
            role,
            send,
            phase: 3,
            queue,
            training: training::Sender::new(role),
            points: Points::Four,
            mp,
            acked: false,
            encoder: None,
            scale: 1.0,
            scrambler: Scrambler::with(polynomial(role)),
            bits: VecDeque::new(),
            b1_frames: 0,
            b1_sent: false,
        }
    }

    fn next(&mut self, far: &Far) -> Complex {
        if self.queue.is_empty() {
            self.refill(far);
        }
        self.queue.pop_front().unwrap_or((0.0, 0.0))
    }

    fn refill(&mut self, far: &Far) {
        match self.send {
            Send::Wait => self.wait(far),
            Send::Trn { sent } => self.trn(sent, far),
            Send::J => self.j(far),
            Send::Mp => self.mp(far),
            Send::Data => self.data(),
        }
    }

    fn wait(&mut self, far: &Far) {
        if self.phase != 3 || far.j.is_none() {
            self.queue.push_back((0.0, 0.0));
            return;
        }
        self.training = training::Sender::new(self.role);
        match self.role {
            // § 11.3.1.1.3: the call modem trains the far end once it has J.
            Role::Originate => self.queue.extend(phase_3_start()),
            // § 11.4.1.2.1: phase 4 starts with S and S̄, then TRN.
            Role::Answer => {
                self.phase = 4;
                self.points = far.j.unwrap_or(Points::Four);
                self.queue.extend((0..S_SYMBOLS).map(training::s));
                self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
            }
        }
        self.send = Send::Trn { sent: 0 };
    }

    fn trn(&mut self, sent: usize, far: &Far) {
        let enough = if self.phase == 3 {
            sent >= TRN_SYMBOLS
        } else {
            sent >= TRN_LEAST && far.trn
        };
        if enough {
            self.send = if self.phase == 3 { Send::J } else { Send::Mp };
            self.refill(far);
            return;
        }
        let points = if self.phase == 3 {
            Points::Four
        } else {
            self.points
        };
        self.queue.push_back(self.training.trn(points));
        self.send = Send::Trn { sent: sent + 1 };
    }

    // Phase 3 J, until the far S̄ that ends it.
    fn j(&mut self, far: &Far) {
        let ended = match self.role {
            Role::Answer => far.s_bar >= 1,
            Role::Originate => far.s_bar >= 2,
        };
        if !ended {
            let request = training::pattern(J_4);
            self.queue
                .extend(self.training.sequence(&request, Points::Four));
            return;
        }
        match self.role {
            Role::Answer => {
                self.send = Send::Wait;
                self.queue.push_back((0.0, 0.0));
            }
            // § 11.4.1.1.1: J′, then TRN.
            Role::Originate => {
                self.phase = 4;
                self.points = far.j.unwrap_or(Points::Four);
                let end = training::pattern(J_PRIME);
                self.queue
                    .extend(self.training.sequence(&end, Points::Four));
                self.training = training::Sender::new(self.role);
                self.send = Send::Trn { sent: 0 };
            }
        }
    }

    fn mp(&mut self, far: &Far) {
        if self.acked && far.ack {
            let e = self.training.sequence(&[true; mp::E_ONES], self.points);
            self.queue.extend(e);
            self.send = Send::Data;
            return;
        }
        let mut mp = self.mp;
        mp.acknowledge = far.mp.is_some();
        self.acked |= mp.acknowledge;
        let frame = mp.frame();
        self.queue
            .extend(self.training.sequence(&frame, self.points));
    }

    fn data(&mut self) {
        let Some(encoder) = &mut self.encoder else {
            self.queue.push_back((0.0, 0.0));
            return;
        };
        let b1 = self.b1_frames > 0;
        let bits: Vec<bool> = (0..encoder.bits())
            .map(|_| {
                let bit = if b1 {
                    true
                } else {
                    self.bits.pop_front().unwrap_or(true)
                };
                self.scrambler.scramble(bit)
            })
            .collect();
        let scale = self.scale;
        self.queue.extend(
            encoder
                .encode(&bits)
                .map(|(re, im)| (re / scale, im / scale)),
        );
        if b1 {
            self.b1_frames -= 1;
            self.b1_sent |= self.b1_frames == 0;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listen {
    /// For S and the S̄ after it.
    S,
    /// On PP and TRN, known from the S̄ that began at `from`.
    Train { from: usize },
    /// Decisions on 4 points, reading J, J′, MP and E.
    Sequences,
    /// Phase 4 TRN, from `from`.
    Trn { from: usize },
    /// B1 and data, from `from`.
    Data { from: usize },
}

/// The receive side from phase 3 on.
#[derive(Debug)]
struct Sink {
    front_end: FrontEnd,
    equalizer: Equalizer,
    detector: SDetector,
    listen: Listen,
    phase: u8,
    count: usize,
    known: training::Sender,
    known_symbols: Vec<Complex>,
    descrambler: Descrambler,
    quadrant: u8,
    window: VecDeque<bool>,
    deframer: mp::Deframer,
    decoder: Option<Decoder>,
    scale: f64,
    frame: Vec<Complex>,
    data_descrambler: Descrambler,
    skip_bits: usize,
}

impl Sink {
    fn new(role: Role, outcome: &Outcome) -> Self {
        let far = other(role);
        Self {
            front_end: FrontEnd::new(outcome.receive.symbol_rate, outcome.receive.high_carrier),
            equalizer: Equalizer::default(),
            detector: SDetector::default(),
            listen: Listen::S,
            phase: 3,
            count: 0,
            known: training::Sender::new(far),
            known_symbols: Vec::new(),
            descrambler: Descrambler::with(polynomial(far)),
            quadrant: 0,
            window: VecDeque::new(),
            deframer: mp::Deframer::default(),
            decoder: None,
            scale: 1.0,
            frame: Vec::new(),
            data_descrambler: Descrambler::with(polynomial(far)),
            skip_bits: 0,
        }
    }

    fn quarter(z: Complex) -> u8 {
        match (z.0 >= 0.0, z.1 >= 0.0) {
            (true, true) => 0,
            (true, false) => 1,
            (false, false) => 2,
            (false, true) => 3,
        }
    }

    fn corner(quadrant: u8) -> Complex {
        match quadrant % 4 {
            0 => (HALF, HALF),
            1 => (HALF, -HALF),
            2 => (-HALF, -HALF),
            _ => (-HALF, HALF),
        }
    }

    // One 4-point symbol of J, J′, MP or E, descrambled.
    fn sequence_bits(&mut self, z: Complex) -> [bool; 2] {
        let quadrant = Self::quarter(z);
        let turns = (quadrant + 4 - self.quadrant) % 4;
        self.quadrant = quadrant;
        [turns & 1 == 1, turns & 2 == 2].map(|bit| self.descrambler.descramble(bit))
    }

    fn take(&mut self, symbol: &Symbol, far: &mut Far, data: &mut Vec<bool>) {
        let z = self.equalizer.output(symbol);
        let index = self.count.checked_sub(DELAY);
        self.count += 1;
        if let Some(Heard::SBar(at)) = self.detector.push(symbol) {
            match self.listen {
                Listen::S => {
                    self.listen = Listen::Train { from: at };
                    self.front_end.track(Tracking::Train);
                    far.s_bar = 1;
                }
                Listen::Sequences if self.phase == 4 && far.s_bar == 1 => {
                    self.listen = Listen::Trn {
                        from: at + S_BAR_SYMBOLS,
                    };
                    far.s_bar = 2;
                }
                _ => {}
            }
        }
        let Some(index) = index else {
            return;
        };
        match self.listen {
            Listen::Train { from } => self.train(index, from),
            Listen::Sequences => self.sequences(z, index, far),
            Listen::Trn { from } => {
                self.equalizer.adapt(Self::corner(Self::quarter(z)));
                self.sequence_bits(z);
                if index >= from + TRN_LEAST {
                    far.trn = true;
                    self.listen = Listen::Sequences;
                }
            }
            Listen::Data { from } if index >= from => self.data(z, data),
            Listen::S | Listen::Data { .. } => {}
        }
    }

    fn train(&mut self, index: usize, from: usize) {
        let Some(offset) = index.checked_sub(from + S_BAR_SYMBOLS) else {
            return;
        };
        if offset < PP_SYMBOLS {
            self.equalizer.adapt(training::pp(offset));
            return;
        }
        let trn = offset - PP_SYMBOLS;
        while self.known_symbols.len() <= trn {
            let next = self.known.trn(Points::Four);
            self.known_symbols.push(next);
        }
        let known = self.known_symbols[trn];
        self.equalizer.adapt(known);
        self.quadrant = Self::quarter(known);
        if trn + 1 >= TRN_LEAST {
            self.listen = Listen::Sequences;
        }
    }

    fn sequences(&mut self, z: Complex, index: usize, far: &mut Far) {
        self.equalizer.adapt(Self::corner(Self::quarter(z)));
        for bit in self.sequence_bits(z) {
            self.window.push_back(bit);
            if self.window.len() > 16 {
                self.window.pop_front();
            }
            if self.phase == 3 {
                if self.window.iter().eq(&training::pattern(J_4)) {
                    far.j = Some(Points::Four);
                } else if self.window.iter().eq(&training::pattern(J_16)) {
                    far.j = Some(Points::Sixteen);
                }
                if far.j.is_some() {
                    self.phase = 4;
                }
            } else if !far.trn && self.window.iter().eq(&training::pattern(J_PRIME)) {
                self.listen = Listen::Trn { from: index + 1 };
            }
            if let Some(mp) = self.deframer.push(bit) {
                far.ack |= mp.acknowledge;
                far.mp = Some(mp);
            }
            if far.mp.is_some() && self.deframer.ones() == mp::E_ONES {
                far.ack = true;
                self.listen = Listen::Data { from: index + 1 };
            }
        }
    }

    fn start_data(&mut self, framing: Framing) {
        self.scale = Encoder::new(framing, Settings::default()).energy().sqrt();
        self.decoder = Some(Decoder::new(framing, Trellis::States16));
        self.skip_bits = framing.p * framing.b - (framing.p - framing.r);
    }

    fn data(&mut self, z: Complex, data: &mut Vec<bool>) {
        if self.frame.is_empty() && self.skip_bits > 0 {
            self.front_end.track(Tracking::Data);
        }
        let scale = self.scale;
        let grid = (z.0 * scale, z.1 * scale);
        self.equalizer
            .adapt((nearest_odd(grid.0) / scale, nearest_odd(grid.1) / scale));
        self.frame.push(grid);
        if self.frame.len() < 8 {
            return;
        }
        let points: [Complex; 8] = std::mem::take(&mut self.frame)
            .try_into()
            .unwrap_or_default();
        let Some(decoder) = &mut self.decoder else {
            return;
        };
        for bit in decoder.decode(points) {
            let bit = self.data_descrambler.descramble(bit);
            if self.skip_bits > 0 {
                self.skip_bits -= 1;
            } else {
                data.push(bit);
            }
        }
    }
}

/// V.34 duplex, from phase 2 to data mode.
#[derive(Debug)]
pub struct V34 {
    role: Role,
    phase2: Phase2,
    outcome: Option<Outcome>,
    modulator: Option<Modulator>,
    source: Option<Source>,
    sink: Option<Sink>,
    far: Far,
    rate: u8,
    level: f64,
    carrier: bool,
}

impl V34 {
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            role,
            phase2: Phase2::new(role),
            outcome: None,
            modulator: None,
            source: None,
            sink: None,
            far: Far::default(),
            rate: 0,
            level: sine_peak(CARRIER_DBM0) / std::f64::consts::SQRT_2,
            carrier: false,
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "70 ms of symbols"
    )]
    fn start_training(&mut self, outcome: Outcome) {
        let transmit = outcome.transmit;
        self.modulator = Some(Modulator::new(
            transmit.symbol_rate,
            transmit.high_carrier,
            transmit.pre_emphasis,
            NOMINAL_DBM0,
        ));
        let silence = if self.role == Role::Answer {
            (SILENCE_MS / 1000.0 * transmit.symbol_rate.baud()) as usize
        } else {
            0
        };
        let receive_max = outcome.receive.max_rate;
        let (call_to_answer, answer_to_call) = match self.role {
            Role::Originate => (transmit.max_rate, receive_max),
            Role::Answer => (receive_max, transmit.max_rate),
        };
        let mp = Mp {
            max_call_to_answer: call_to_answer,
            max_answer_to_call: answer_to_call,
            rates: rates::mask(transmit.symbol_rate, 14),
            ..Mp::default()
        };
        self.source = Some(Source::new(self.role, silence, mp));
        self.sink = Some(Sink::new(self.role, &outcome));
        self.outcome = Some(outcome);
    }

    // § 11.4.1.1.3: once the far MP has come, both ends know the data rate.
    fn agree(&mut self) {
        let (Some(outcome), Some(source), Some(sink), Some(far)) =
            (self.outcome, &mut self.source, &mut self.sink, self.far.mp)
        else {
            return;
        };
        if source.encoder.is_some() {
            return;
        }
        let ours = source.mp;
        let rate = rates::agree(
            [
                ours.max_call_to_answer,
                ours.max_answer_to_call,
                far.max_call_to_answer,
                far.max_answer_to_call,
            ],
            [ours.rates, far.rates],
        );
        let bit_rate = u32::from(rate) * 2400;
        let (Some(transmit), Some(receive)) = (
            Framing::new(outcome.transmit.symbol_rate, bit_rate, far.expanded_shaping),
            Framing::new(outcome.receive.symbol_rate, bit_rate, false),
        ) else {
            return;
        };
        let settings = Settings {
            trellis: far.trellis,
            nonlinear: far.nonlinear,
            precoding: far.precoding.unwrap_or_default(),
        };
        let encoder = Encoder::new(transmit, settings);
        source.scale = encoder.energy().sqrt();
        source.encoder = Some(encoder);
        source.b1_frames = transmit.p;
        sink.start_data(receive);
        self.rate = rate;
    }
}

impl DataPump for V34 {
    fn engaged(&self) -> bool {
        self.phase2.engaged()
    }

    fn bit_rate(&self) -> u32 {
        u32::from(self.rate) * 2400
    }

    fn decoder(&self) -> Characters {
        Characters::v14()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        if let Some(source) = &mut self.source {
            source.bits.extend(bits);
        }
    }

    fn pending(&self) -> usize {
        self.source.as_ref().map_or(0, |source| source.bits.len())
    }

    fn transmit(&mut self, out: &mut [i16]) {
        let (Some(modulator), Some(source)) = (&mut self.modulator, &mut self.source) else {
            self.phase2.transmit(out);
            return;
        };
        let far = &self.far;
        modulator.render(out, || source.next(far));
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        let Some(sink) = &mut self.sink else {
            self.phase2.receive(input);
            if let Some(outcome) = self.phase2.outcome().filter(|_| self.phase2.done()) {
                self.start_training(outcome);
            }
            return;
        };
        let power = input.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>()
            / f64::from(u32::try_from(input.len().max(1)).unwrap_or(1));
        self.carrier = power.sqrt() > self.level;
        for symbol in sink.front_end.process(input) {
            sink.take(&symbol, &mut self.far, bits);
        }
        self.agree();
    }

    fn carrier(&self) -> bool {
        self.connected() && self.carrier
    }

    fn connected(&self) -> bool {
        let sent = self.source.as_ref().is_some_and(|source| source.b1_sent);
        let heard = self.sink.as_ref().is_some_and(|sink| {
            sink.decoder.is_some()
                && sink.skip_bits == 0
                && matches!(sink.listen, Listen::Data { .. })
        });
        sent && heard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 160;

    // G.711 A-law and back, as the transport carries every call.
    fn alaw(sample: i16) -> i16 {
        const ENDS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
        let value = i32::from(sample) >> 3;
        let (negative, magnitude) = if value >= 0 {
            (false, value)
        } else {
            (true, -value - 1)
        };
        let segment = ENDS.iter().position(|&end| magnitude <= end).unwrap_or(7);
        let shift = if segment < 2 { 1 } else { segment };
        let step = ((magnitude.min(0xFFF) >> shift) & 0x0F) << 4;
        let restored = if segment == 0 {
            step + 8
        } else {
            (step + 0x108) << (segment - 1)
        };
        let restored = i16::try_from(restored).unwrap_or(i16::MAX);
        if negative { -restored } else { restored }
    }

    fn exchange_over(
        caller: &mut V34,
        answerer: &mut V34,
        frames: usize,
        line: fn(i16) -> i16,
    ) -> (Vec<bool>, Vec<bool>) {
        let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
        let (mut at_caller, mut at_answerer) = (Vec::new(), Vec::new());
        for _ in 0..frames {
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            let (up, down) = (up.map(line), down.map(line));
            answerer.receive(&up, &mut at_answerer);
            caller.receive(&down, &mut at_caller);
        }
        (at_caller, at_answerer)
    }

    fn exchange(caller: &mut V34, answerer: &mut V34, frames: usize) -> (Vec<bool>, Vec<bool>) {
        exchange_over(caller, answerer, frames, |sample| sample)
    }

    #[test]
    fn two_ends_carry_data_over_alaw() {
        let mut caller = V34::new(Role::Originate);
        let mut answerer = V34::new(Role::Answer);
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) && frames < 400 {
            exchange_over(&mut caller, &mut answerer, 1, alaw);
            frames += 1;
        }
        assert!(caller.connected(), "no V.34 connection over A-law");
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange_over(&mut caller, &mut answerer, 80, alaw);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer) && found(&at_caller),
            "data at {} bit/s would not survive A-law: mse {:?} {:?}",
            caller.bit_rate(),
            caller
                .sink
                .as_ref()
                .map(|s| 10.0 * s.equalizer.error().log10()),
            answerer
                .sink
                .as_ref()
                .map(|s| 10.0 * s.equalizer.error().log10()),
        );
    }

    #[test]
    fn two_ends_connect_at_33600_and_carry_data_both_ways() {
        let mut caller = V34::new(Role::Originate);
        let mut answerer = V34::new(Role::Answer);
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) {
            exchange(&mut caller, &mut answerer, 1);
            frames += 1;
            assert!(
                frames < 400,
                "no V.34 connection after {} ms: caller {:?}/{:?} {:?}, answerer {:?}/{:?} {:?}",
                frames * 20,
                caller.source.as_ref().map(|s| s.send),
                caller.sink.as_ref().map(|s| s.listen),
                caller.far,
                answerer.source.as_ref().map(|s| s.send),
                answerer.sink.as_ref().map(|s| s.listen),
                answerer.far,
            );
        }
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (33_600, 33_600));
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange(&mut caller, &mut answerer, 60);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(found(&at_answerer), "the answerer lost the caller's data");
        assert!(found(&at_caller), "the caller lost the answerer's data");
    }
}
