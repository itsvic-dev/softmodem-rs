// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The V.34 data pump: phase 2, the training of phases 3 and 4 as the
//! error-free procedures of § 11.3 and § 11.4 give it, and data mode.

use std::collections::VecDeque;

use super::decoder::Decoder;
use super::detect::{Heard, SDetector};
use super::encoder::{Encoder, Settings};
use super::framing::Framing;
use super::modulator::Modulator;
use super::mp::{self, Asks, Mp};
use super::phase2::{Outcome, Phase2};
use super::receiver::{DELAY, Equalizer, FrontEnd, Symbol, Tracking};
use super::training::{self, J_4, J_16, J_PRIME, PP_SYMBOLS, Points};
use super::{NOMINAL_DBM0, SymbolRate, rates, tones};
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
// Of the far phase 4 TRN: the symbols the equaliser settles on, then the end of the measurement.
const TRN_SETTLE: usize = 512;
const TRN_HEARD: usize = 1536;
// § 11.5: the far tone for more than 50 ms starts a retrain.
const RETRAIN_TONE: usize = 400;
// § 11.3.1.2.1: the answer modem waits 70 ± 5 ms after INFO1a.
const SILENCE_MS: f64 = 70.0;
const HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
const J_BITS: u32 = 16;
const J_4_CODE: u32 = code(J_4);
const J_16_CODE: u32 = code(J_16);
const J_PRIME_CODE: u32 = code(J_PRIME);

// A 16-bit sequence of table 19, its first bit highest.
const fn code(pattern: &str) -> u32 {
    let bits = pattern.as_bytes();
    let mut value = 0;
    let mut n = 0;
    while n < bits.len() {
        value = value << 1 | (bits[n] == b'1') as u32;
        n += 1;
    }
    value
}
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
    /// The far phase 4 TRN measured, or ended by its MP.
    trn: bool,
    /// The highest rate that the far phase 4 TRN was heard well enough for.
    trained: Option<u8>,
    mp: Option<Mp>,
    /// MP′ or E from the far end.
    ack: bool,
    /// Rate renegotiations heard from the far end, by the S̄ that starts each.
    renegotiations: u32,
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
    /// Silence once a cleardown has ended the call.
    Cleared,
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
    acks_sent: u8,
    encoder: Option<Encoder>,
    scale: f64,
    scrambler: Scrambler,
    bits: VecDeque<bool>,
    b1_frames: usize,
    b1_sent: bool,
    renegotiations: u32,
    /// This end started a cleardown, so its MP asks for 0 bit/s both ways.
    clearing: bool,
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
            acks_sent: 0,
            encoder: None,
            scale: 1.0,
            scrambler: Scrambler::with(polynomial(role)),
            bits: VecDeque::new(),
            b1_frames: 0,
            b1_sent: false,
            renegotiations: 0,
            clearing: false,
        }
    }

    fn next(&mut self, far: &Far) -> Complex {
        if self.queue.is_empty() {
            self.refill(far);
        }
        self.queue.pop_front().unwrap_or((0.0, 0.0))
    }

    fn refill(&mut self, far: &Far) {
        if far.renegotiations != self.renegotiations {
            self.renegotiations = far.renegotiations;
            // § 11.6.1.2.2: the responding modem answers the far S̄; an initiating one has already begun.
            if self.send == Send::Data {
                self.renegotiate();
            }
        }
        match self.send {
            Send::Wait => self.wait(far),
            Send::Trn { sent } => self.trn(sent, far),
            Send::J => self.j(far),
            Send::Mp => self.mp(far),
            Send::Data => self.data(),
            Send::Cleared => self.queue.push_back((0.0, 0.0)),
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

    // Ours, with the rate the far TRN allows in the direction this end hears.
    fn own_mp(&self, far: &Far) -> Mp {
        let mut mp = self.mp;
        if let Some(trained) = far.trained {
            match self.role {
                Role::Originate => mp.max_answer_to_call = trained,
                Role::Answer => mp.max_call_to_answer = trained,
            }
        }
        mp
    }

    fn mp(&mut self, far: &Far) {
        let far_clears = far
            .mp
            .is_some_and(|mp| mp.max_call_to_answer == 0 && mp.max_answer_to_call == 0);
        // Two MP′ at least, as slmodemd may miss the first while it restarts its own MP.
        if self.acks_sent >= 2 && far.ack {
            // § 11.7: MP′ both ways ends a cleardown, with no E.
            if self.clearing || far_clears {
                self.send = Send::Cleared;
                self.queue.push_back((0.0, 0.0));
                return;
            }
            let e = self.training.sequence(&[true; mp::E_ONES], self.points);
            self.queue.extend(e);
            self.send = Send::Data;
            return;
        }
        let mut mp = self.own_mp(far);
        if self.clearing {
            mp.max_call_to_answer = 0;
            mp.max_answer_to_call = 0;
        }
        mp.acknowledge = far.mp.is_some();
        self.acks_sent += u8::from(mp.acknowledge);
        let frame = mp.frame();
        self.queue
            .extend(self.training.sequence(&frame, self.points));
    }

    // § 11.7.1.1: S and S̄, then MP asking for 0 bit/s, with no TRN.
    fn clear_down(&mut self) {
        self.renegotiate();
        self.clearing = true;
        self.send = Send::Mp;
    }

    // § 11.6: S, S̄ and TRN, then MP, E and B1 as in phase 4, all on 4 points.
    fn renegotiate(&mut self) {
        self.queue.extend((0..S_SYMBOLS).map(training::s));
        self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
        self.training = training::Sender::new(self.role);
        self.points = Points::Four;
        self.acks_sent = 0;
        self.encoder = None;
        self.b1_sent = false;
        self.send = Send::Trn { sent: 0 };
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
    /// S in data mode, until the S̄ that starts a rate renegotiation.
    Hold,
}

/// The receive side from phase 3 on.
#[derive(Debug)]
struct Sink {
    far_role: Role,
    symbol_rate: SymbolRate,
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
    window: u32,
    deframer: mp::Deframer,
    trn_heard: usize,
    trn_error: f64,
    decoder: Option<Decoder>,
    scale: f64,
    symbols: usize,
    data_descrambler: Descrambler,
    skip_bits: usize,
}

impl Sink {
    fn new(role: Role, outcome: &Outcome) -> Self {
        let far = other(role);
        Self {
            far_role: far,
            symbol_rate: outcome.receive.symbol_rate,
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
            window: 0,
            deframer: mp::Deframer::default(),
            trn_heard: TRN_HEARD,
            trn_error: 0.0,
            decoder: None,
            scale: 1.0,
            symbols: 0,
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
        match (self.detector.push(symbol), self.listen) {
            (Some(Heard::SBar(at)), Listen::S) => {
                self.listen = Listen::Train { from: at };
                self.front_end.track(Tracking::Train);
                far.s_bar = 1;
            }
            (Some(Heard::SBar(at)), Listen::Sequences) if self.phase == 4 && far.s_bar == 1 => {
                self.listen = Listen::Trn {
                    from: at + S_BAR_SYMBOLS,
                };
                far.s_bar = 2;
            }
            (Some(Heard::S), Listen::Data { .. }) => self.listen = Listen::Hold,
            (Some(Heard::SBar(at)), Listen::Data { .. } | Listen::Hold) => {
                self.renegotiate(at, far);
            }
            _ => {}
        }
        let Some(index) = index else {
            return;
        };
        match self.listen {
            Listen::Train { from } => self.train(index, from),
            Listen::Sequences => self.sequences(z, index, far),
            Listen::Trn { from } => {
                if let Some(offset) = index.checked_sub(from) {
                    self.trn(z, offset, far);
                } else {
                    self.equalizer.adapt(Self::corner(Self::quarter(z)));
                    self.sequence_bits(z);
                }
            }
            Listen::Data { from } if index >= from => self.data(z, data),
            Listen::S | Listen::Data { .. } | Listen::Hold => {}
        }
    }

    // § 11.6.1.1.2 and § 11.6.1.2.1: after the far S̄, TRN, MP and E again, then B1 at the new rate.
    fn renegotiate(&mut self, at: usize, far: &mut Far) {
        self.listen = Listen::Trn {
            from: at + S_BAR_SYMBOLS,
        };
        self.trn_error = 0.0;
        self.deframer = mp::Deframer::default();
        self.decoder = None;
        self.symbols = 0;
        far.trn = false;
        far.trained = None;
        far.mp = None;
        far.ack = false;
        far.renegotiations += 1;
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

    // The far phase 4 TRN, until `trn_heard` symbols of it or the far MP after it.
    fn trn(&mut self, z: Complex, offset: usize, far: &mut Far) {
        self.front_end.track(Tracking::Data);
        let target = Self::corner(Self::quarter(z));
        if offset >= TRN_SETTLE {
            self.trn_error += (z.0 - target.0).powi(2) + (z.1 - target.1).powi(2);
        }
        self.equalizer.adapt(target);
        for bit in self.sequence_bits(z) {
            if let Some(mp) = self.deframer.push(bit) {
                far.ack |= mp.acknowledge;
                far.mp = Some(mp);
            }
        }
        if offset + 1 < self.trn_heard && far.mp.is_none() {
            return;
        }
        #[expect(clippy::cast_precision_loss, reason = "a few thousand symbols")]
        let mse = match (offset + 1).checked_sub(TRN_SETTLE) {
            Some(measured) if measured > 0 => self.trn_error / measured as f64,
            _ => self.equalizer.error(),
        };
        far.trn = true;
        far.trained = Some(rates::trained_rate(self.symbol_rate, mse));
        self.listen = Listen::Sequences;
    }

    fn sequences(&mut self, z: Complex, index: usize, far: &mut Far) {
        self.equalizer.adapt(Self::corner(Self::quarter(z)));
        for bit in self.sequence_bits(z) {
            self.window = self.window << 1 | u32::from(bit);
            self.j(index, far);
            self.mp_bit(bit, index, far);
        }
    }

    // J repeats, and J′ follows one: a single 16-bit match can come from errors in a long TRN.
    fn j(&mut self, index: usize, far: &mut Far) {
        let (earlier, last) = (self.window >> J_BITS, self.window & 0xFFFF);
        if self.phase == 3 {
            for (j, points) in [(J_4_CODE, Points::Four), (J_16_CODE, Points::Sixteen)] {
                if earlier == j && last == j {
                    far.j = Some(points);
                }
            }
            if far.j.is_some() {
                self.phase = 4;
            }
        } else if self.far_role == Role::Originate
            && !far.trn
            && last == J_PRIME_CODE
            && (earlier == J_4_CODE || earlier == J_16_CODE)
        {
            self.listen = Listen::Trn { from: index + 1 };
        }
    }

    fn mp_bit(&mut self, bit: bool, index: usize, far: &mut Far) {
        if let Some(mp) = self.deframer.push(bit) {
            far.ack |= mp.acknowledge;
            far.mp = Some(mp);
        }
        if far.mp.is_some() && self.deframer.ones() == mp::E_ONES {
            far.ack = true;
            self.listen = Listen::Data { from: index + 1 };
        }
    }

    fn start_data(&mut self, framing: Framing, asked: Settings) {
        self.scale = Encoder::new(framing, Settings::default()).energy().sqrt();
        self.decoder = Some(Decoder::new(framing, asked));
        self.data_descrambler = Descrambler::with(polynomial(self.far_role));
        self.skip_bits = framing.p * framing.b - (framing.p - framing.r);
    }

    fn data(&mut self, z: Complex, data: &mut Vec<bool>) {
        if self.symbols.is_multiple_of(8) && self.skip_bits > 0 {
            self.front_end.track(Tracking::Data);
        }
        self.symbols += 1;
        let scale = self.scale;
        let grid = (z.0 * scale, z.1 * scale);
        let mut bits = Vec::new();
        let target = match &mut self.decoder {
            Some(decoder) => decoder.push(grid, &mut bits),
            None => (nearest_odd(grid.0), nearest_odd(grid.1)),
        };
        self.equalizer.adapt((target.0 / scale, target.1 / scale));
        for bit in bits {
            let bit = self.data_descrambler.descramble(bit);
            if self.skip_bits > 0 {
                self.skip_bits -= 1;
            } else {
                data.push(bit);
            }
        }
    }
}

// Samples at 8000/s, and dB of margin over what the rate needs.
const MONITOR_SETTLE: usize = 16_000;
const DOWN_MARGIN_DB: f64 = 0.5;
const DOWN_AFTER: usize = 8_000;
const RETRAIN_MARGIN_DB: f64 = -3.0;
const RETRAIN_AFTER: usize = 4_000;
const UP_MARGIN_DB: f64 = 4.5;
const UP_AFTER: usize = 80_000;
const UP_MOST: usize = 2_400_000;
const COOLDOWN: usize = 160_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Renegotiate,
    Retrain,
}

// When this end should renegotiate or retrain, from the margin in data mode.
#[derive(Debug)]
struct Monitor {
    in_data: usize,
    low: usize,
    lost: usize,
    high: usize,
    since_action: usize,
    up_after: usize,
    up_from: Option<u8>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self {
            in_data: 0,
            low: 0,
            lost: 0,
            high: 0,
            since_action: COOLDOWN,
            up_after: UP_AFTER,
            up_from: None,
        }
    }
}

impl Monitor {
    fn leave(&mut self, samples: usize) {
        self.since_action = self.since_action.saturating_add(samples);
        self.in_data = 0;
        self.low = 0;
        self.lost = 0;
        self.high = 0;
    }

    fn watch(
        &mut self,
        margin_db: f64,
        rate: u8,
        can_rise: bool,
        samples: usize,
    ) -> Option<Action> {
        if self.in_data == 0
            && let Some(from) = self.up_from.take()
        {
            // A try at a higher rate that failed waits twice as long for the next.
            self.up_after = if rate > from {
                UP_AFTER
            } else {
                (2 * self.up_after).min(UP_MOST)
            };
        }
        self.since_action = self.since_action.saturating_add(samples);
        self.in_data += samples;
        if self.in_data < MONITOR_SETTLE {
            return None;
        }
        let count = |run: usize, holds: bool| if holds { run + samples } else { 0 };
        self.low = count(self.low, margin_db < DOWN_MARGIN_DB);
        self.lost = count(self.lost, margin_db < RETRAIN_MARGIN_DB);
        self.high = count(self.high, margin_db > UP_MARGIN_DB && can_rise);
        if self.since_action < COOLDOWN {
            return None;
        }
        let action = if self.lost >= RETRAIN_AFTER {
            Action::Retrain
        } else if self.low >= DOWN_AFTER {
            Action::Renegotiate
        } else if self.high >= self.up_after {
            self.up_from = Some(rate);
            Action::Renegotiate
        } else {
            return None;
        };
        self.since_action = 0;
        Some(action)
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
    /// Connected once, and still in the call through renegotiations and retrains.
    online: bool,
    /// The far end's tone A or B in data mode, which starts a retrain.
    retrain_tone: tones::Detector,
    tone_heard: usize,
    monitor: Monitor,
    asks: Asks,
    retrains: Retrains,
}

/// Whose phase 2 a retrain runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Retrains {
    Here,
    /// Phase 2 was that of V.90, which a retrain must use again, once `asked`.
    InV90 {
        asked: bool,
    },
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
            online: false,
            retrain_tone: tones::Detector::new(other(role)),
            tone_heard: 0,
            monitor: Monitor::default(),
            asks: Asks::default(),
            retrains: Retrains::Here,
        }
    }

    /// V.34 from phase 3, where phase 2 of V.90 settled on V.34
    /// (§ 9.2.1.1.8/V.90 and § 9.2.2.1.9/V.90). A retrain goes back to
    /// phase 2 of V.90, which [`V34::retrain_asked`] leaves to the V.90 modem.
    #[must_use]
    pub fn after_v90(role: Role, outcome: Outcome) -> Self {
        let mut v34 = Self::new(role);
        v34.retrains = Retrains::InV90 { asked: false };
        v34.start_training(outcome);
        v34
    }

    /// Whether either end has started a retrain that phase 2 of V.90 must carry.
    #[must_use]
    pub fn retrain_asked(&self) -> bool {
        self.retrains == Retrains::InV90 { asked: true }
    }

    /// Asking the far transmitter for `asks` in MP.
    #[must_use]
    pub fn asking(mut self, asks: Asks) -> Self {
        self.asks = asks;
        self
    }

    // § 11.5 and § 11.6 leave it to each end when to retrain or renegotiate.
    fn watch(&mut self, samples: usize) {
        let in_data = self.connected()
            && self
                .sink
                .as_ref()
                .is_some_and(|sink| matches!(sink.listen, Listen::Data { .. }));
        let Some(sink) = self.sink.as_ref().filter(|_| in_data) else {
            self.monitor.leave(samples);
            return;
        };
        let bit_rate = u32::from(self.rate) * 2400;
        let margin = rates::margin_db(sink.symbol_rate, bit_rate, sink.equalizer.error());
        let can_rise = self.rate < rates::max_rate(sink.symbol_rate, f64::INFINITY);
        match self.monitor.watch(margin, self.rate, can_rise, samples) {
            Some(Action::Renegotiate) => self.renegotiate(),
            Some(Action::Retrain) => self.restart(),
            None => {}
        }
    }

    // § 11.5: phase 2 again from the tones, then phases 3 and 4, with the far INFO0 kept.
    fn restart(&mut self) {
        let Some(outcome) = self.outcome else {
            return;
        };
        if let Retrains::InV90 { asked } = &mut self.retrains {
            *asked = true;
            return;
        }
        self.phase2 = Phase2::retrain(self.role, outcome.far);
        self.outcome = None;
        self.modulator = None;
        self.source = None;
        self.sink = None;
        self.far = Far::default();
        self.tone_heard = 0;
    }

    // § 11.4.2 and § 11.5: the far tone for more than 50 ms from phase 4 on.
    fn far_retrains(&mut self, input: &[i16]) -> bool {
        if self.source.as_ref().is_none_or(|source| source.phase != 4) {
            self.tone_heard = 0;
            return false;
        }
        self.retrain_tone.process(input);
        self.tone_heard = if self.retrain_tone.present() {
            self.tone_heard + input.len()
        } else {
            0
        };
        self.tone_heard > RETRAIN_TONE
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
        // The far end judges the direction it hears, in its own MP.
        let receive_max = outcome.receive.max_rate;
        let (call_to_answer, answer_to_call) = match self.role {
            Role::Originate => (14, receive_max),
            Role::Answer => (receive_max, 14),
        };
        let mp = self.asks.ask(Mp {
            max_call_to_answer: call_to_answer,
            max_answer_to_call: answer_to_call,
            rates: rates::mask(transmit.symbol_rate, 14),
            ..Mp::default()
        });
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
        let ours = source.own_mp(&self.far);
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
            Framing::new(
                outcome.receive.symbol_rate,
                bit_rate,
                self.asks.expanded_shaping,
            ),
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
        source.scrambler = Scrambler::with(polynomial(self.role));
        source.b1_frames = transmit.p;
        sink.start_data(receive, self.asks.settings());
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
        if self.sink.is_none() {
            self.phase2.receive(input);
            if let Some(outcome) = self.phase2.outcome().filter(|_| self.phase2.done()) {
                self.start_training(outcome);
            }
            return;
        }
        let power = input.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>()
            / f64::from(u32::try_from(input.len().max(1)).unwrap_or(1));
        self.carrier = power.sqrt() > self.level;
        if self.far_retrains(input) {
            self.restart();
            return;
        }
        let Some(sink) = &mut self.sink else {
            return;
        };
        for symbol in sink.front_end.process(input) {
            sink.take(&symbol, &mut self.far, bits);
        }
        self.agree();
        self.online |= self.connected();
        self.watch(input.len());
    }

    fn retrain(&mut self) {
        if self.connected() {
            self.restart();
        }
    }

    // § 11.6.1.1: data stops until both ends are in data mode again, at the rate the two MPs agree.
    fn renegotiate(&mut self) {
        let Some(source) = &mut self.source else {
            return;
        };
        if source.send != Send::Data {
            return;
        }
        source.renegotiate();
        self.far.trn = false;
        self.far.trained = None;
        self.far.mp = None;
        self.far.ack = false;
    }

    fn clear_down(&mut self) {
        let Some(source) = &mut self.source else {
            return;
        };
        if source.send != Send::Data {
            return;
        }
        source.clear_down();
        self.far.trn = false;
        self.far.trained = None;
        self.far.mp = None;
        self.far.ack = false;
    }

    fn cleared(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| source.send == Send::Cleared)
    }

    fn carrier(&self) -> bool {
        self.online && self.carrier && !self.cleared()
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
        assert_eq!(
            (caller.bit_rate(), answerer.bit_rate()),
            (33_600, 33_600),
            "A-law leaves room for 33 600 bit/s at 3429 baud"
        );
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
    fn two_ends_connect_whatever_the_delay_of_the_line() {
        for delay in [1, 37, 80, 160, 241, 320, 480, 800, 1600] {
            let mut caller = V34::new(Role::Originate);
            let mut answerer = V34::new(Role::Answer);
            let mut lines = [
                VecDeque::from(vec![0; delay]),
                VecDeque::from(vec![0; delay]),
            ];
            let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
            let mut frames = 0;
            while !(caller.connected() && answerer.connected()) && frames < 500 {
                caller.transmit(&mut up);
                answerer.transmit(&mut down);
                lines[0].extend(up);
                lines[1].extend(down);
                let up: Vec<i16> = lines[0].drain(..FRAME).collect();
                let down: Vec<i16> = lines[1].drain(..FRAME).collect();
                answerer.receive(&up, &mut Vec::new());
                caller.receive(&down, &mut Vec::new());
                frames += 1;
            }
            assert!(
                caller.connected() && answerer.connected(),
                "no V.34 connection over a line of {delay} samples each way: caller {:?}/{:?} {:?}, answerer {:?}/{:?} {:?}",
                caller.source.as_ref().map(|s| s.send),
                caller.sink.as_ref().map(|s| s.listen),
                caller.far,
                answerer.source.as_ref().map(|s| s.send),
                answerer.sink.as_ref().map(|s| s.listen),
                answerer.far,
            );
        }
    }

    #[test]
    fn connects_to_an_end_that_sends_the_shortest_phase_4_trn() {
        let mut caller = V34::new(Role::Originate);
        let mut answerer = V34::new(Role::Answer);
        let mut frames = 0;
        let (mut mp_from, mut trn_until) = (None, None);
        while !(caller.connected() && answerer.connected()) && frames < 400 {
            if let Some(sink) = &mut answerer.sink {
                sink.trn_heard = 1;
            }
            exchange(&mut caller, &mut answerer, 1);
            frames += 1;
            if answerer.source.as_ref().is_some_and(|s| s.send == Send::Mp) {
                mp_from.get_or_insert(frames);
            }
            if caller.far.trn {
                trn_until.get_or_insert(frames);
            }
        }
        let late = trn_until.zip(mp_from).map(|(trn, mp)| (trn - mp) * 20);
        assert!(
            late.is_some_and(|ms| ms < 150),
            "the caller heard the far MP as TRN for {late:?} ms, and would answer it that much late"
        );
        assert!(
            caller.connected() && answerer.connected(),
            "a far end that ends its TRN after 512T would never reach data: caller {:?}/{:?}",
            caller.source.as_ref().map(|s| s.send),
            caller.sink.as_ref().map(|s| s.listen),
        );
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange(&mut caller, &mut answerer, 60);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer) && found(&at_caller),
            "data lost after a short TRN"
        );
    }

    thread_local! {
        static NOISE: std::cell::Cell<u64> = const { std::cell::Cell::new(0x2545_F491_4F6C_DD1D) };
    }

    // White noise about 26 dB below the signal.
    fn noisy(sample: i16) -> i16 {
        let uniform = || {
            NOISE.with(|state| {
                let mut x = state.get();
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                state.set(x);
                f64::from(u32::try_from(x >> 32).unwrap()) / f64::from(u32::MAX) - 0.5
            })
        };
        let noise: f64 = (0..4).map(|_| uniform()).sum::<f64>() * 300.0;
        #[expect(clippy::cast_possible_truncation, reason = "clamped to i16")]
        let out = (f64::from(sample) + noise).clamp(-32_768.0, 32_767.0) as i16;
        out
    }

    // Noise about 32 dB below the signal: less room than 33 600 bit/s needs, but not lost.
    fn mild(sample: i16) -> i16 {
        sample.saturating_add(noisy(0) / 2)
    }

    // Frames until both ends are connected again at a rate `holds` accepts, if within `frames`.
    fn until_rate(
        caller: &mut V34,
        answerer: &mut V34,
        line: fn(i16) -> i16,
        frames: usize,
        holds: impl Fn(u32) -> bool,
    ) -> Option<usize> {
        (1..=frames).find(|_| {
            exchange_over(caller, answerer, 1, line);
            caller.connected() && answerer.connected() && holds(caller.bit_rate())
        })
    }

    #[test]
    fn clears_down_from_either_end() {
        for initiator in [Role::Originate, Role::Answer] {
            let (mut caller, mut answerer) = connected_pair();
            match initiator {
                Role::Originate => caller.clear_down(),
                Role::Answer => answerer.clear_down(),
            }
            let cleared = (0..200).find(|_| {
                exchange(&mut caller, &mut answerer, 1);
                caller.cleared() && answerer.cleared()
            });
            assert!(
                cleared.is_some(),
                "a cleardown started by the {initiator:?} end would leave the call up: caller {:?}, answerer {:?}",
                caller.source.as_ref().map(|s| s.send),
                answerer.source.as_ref().map(|s| s.send),
            );
            assert!(
                !caller.carrier() && !answerer.carrier(),
                "DCD would stay on after a cleardown"
            );
        }
    }

    #[test]
    fn stays_in_data_mode_on_a_clean_line() {
        let (mut caller, mut answerer) = connected_pair();
        for _ in 0..1250 {
            exchange(&mut caller, &mut answerer, 1);
            assert!(
                caller.connected() && answerer.connected(),
                "a clean line would be renegotiated or retrained for nothing"
            );
        }
        assert_eq!(caller.bit_rate(), 33_600);
    }

    #[test]
    fn steps_down_on_its_own_when_the_line_gets_worse() {
        let (mut caller, mut answerer) = connected_pair();
        let stepped = until_rate(&mut caller, &mut answerer, mild, 750, |rate| rate < 33_600);
        assert!(
            stepped.is_some(),
            "33 600 bit/s would stay on a line that no longer carries it"
        );
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange_over(&mut caller, &mut answerer, 80, mild);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer) && found(&at_caller),
            "data would be lost at {} bit/s after stepping down",
            caller.bit_rate()
        );
    }

    #[test]
    fn steps_up_again_when_the_line_gets_better() {
        let (mut caller, mut answerer) = connected_pair();
        until_rate(&mut caller, &mut answerer, mild, 750, |rate| rate < 33_600)
            .expect("no step down to come back up from");
        let low = caller.bit_rate();
        let rose = until_rate(
            &mut caller,
            &mut answerer,
            |sample| sample,
            1500,
            |rate| rate > low,
        );
        assert!(
            rose.is_some(),
            "{low} bit/s would stay after the line got clean again"
        );
    }

    fn connected_pair() -> (V34, V34) {
        let mut caller = V34::new(Role::Originate);
        let mut answerer = V34::new(Role::Answer);
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) && frames < 400 {
            exchange(&mut caller, &mut answerer, 1);
            frames += 1;
        }
        assert!(caller.connected(), "no V.34 connection to renegotiate");
        (caller, answerer)
    }

    // Runs until both ends are in data mode again, and says whether the carrier held throughout.
    fn back_in_data(caller: &mut V34, answerer: &mut V34, line: fn(i16) -> i16) -> bool {
        let mut left = false;
        let mut carrier = true;
        for _ in 0..500 {
            exchange_over(caller, answerer, 1, line);
            carrier &= caller.carrier() && answerer.carrier();
            left |= !(caller.connected() && answerer.connected());
            if left && caller.connected() && answerer.connected() {
                return carrier;
            }
        }
        panic!(
            "a rate renegotiation would never return to data: caller {:?}/{:?}, answerer {:?}/{:?}",
            caller.source.as_ref().map(|s| s.send),
            caller.sink.as_ref().map(|s| s.listen),
            answerer.source.as_ref().map(|s| s.send),
            answerer.sink.as_ref().map(|s| s.listen),
        );
    }

    #[test]
    fn renegotiates_from_either_end_and_carries_data_after() {
        for initiator in [Role::Originate, Role::Answer] {
            let (mut caller, mut answerer) = connected_pair();
            match initiator {
                Role::Originate => caller.renegotiate(),
                Role::Answer => answerer.renegotiate(),
            }
            assert!(
                back_in_data(&mut caller, &mut answerer, |sample| sample),
                "DCD would drop during a renegotiation started by the {initiator:?} end"
            );
            assert_eq!((caller.bit_rate(), answerer.bit_rate()), (33_600, 33_600));
            let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
            caller.push_bits(&message);
            answerer.push_bits(&message);
            let (at_caller, at_answerer) = exchange(&mut caller, &mut answerer, 60);
            let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
            assert!(
                found(&at_answerer) && found(&at_caller),
                "data would be lost after a renegotiation started by the {initiator:?} end"
            );
        }
    }

    #[test]
    fn retrains_from_either_end_and_carries_data_after() {
        for initiator in [Role::Originate, Role::Answer] {
            let (mut caller, mut answerer) = connected_pair();
            match initiator {
                Role::Originate => caller.retrain(),
                Role::Answer => answerer.retrain(),
            }
            back_in_data(&mut caller, &mut answerer, |sample| sample);
            assert_eq!((caller.bit_rate(), answerer.bit_rate()), (33_600, 33_600));
            let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
            caller.push_bits(&message);
            answerer.push_bits(&message);
            let (at_caller, at_answerer) = exchange(&mut caller, &mut answerer, 60);
            let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
            assert!(
                found(&at_answerer) && found(&at_caller),
                "data would be lost after a retrain started by the {initiator:?} end"
            );
        }
    }

    #[test]
    fn answers_a_retrain_that_the_far_end_starts_in_phase_4() {
        for initiator in [Role::Originate, Role::Answer] {
            let mut caller = V34::new(Role::Originate);
            let mut answerer = V34::new(Role::Answer);
            let mut frames = 0;
            while !(in_phase_4(&caller) && in_phase_4(&answerer)) && frames < 400 {
                exchange(&mut caller, &mut answerer, 1);
                frames += 1;
            }
            assert!(
                in_phase_4(&caller) && in_phase_4(&answerer),
                "two ends would never train each other"
            );
            let (starts, answers) = match initiator {
                Role::Originate => (&mut caller, &mut answerer),
                Role::Answer => (&mut answerer, &mut caller),
            };
            starts.restart();
            let joined = (1..=25).any(|_| {
                exchange(starts, answers, 1);
                answers.sink.is_none()
            });
            assert!(
                joined,
                "a retrain from the {initiator:?} end in phase 4 would go unanswered until the far end gives up"
            );
            let back = (1..=500).any(|_| {
                exchange(&mut caller, &mut answerer, 1);
                caller.connected() && answerer.connected()
            });
            assert!(
                back,
                "a retrain in phase 4 from the {initiator:?} end would never reach data"
            );
        }
    }

    fn in_phase_4(pump: &V34) -> bool {
        pump.source
            .as_ref()
            .is_some_and(|source| source.phase == 4 && source.send != Send::Data)
    }

    #[test]
    fn retrains_down_when_the_line_gets_worse() {
        let (mut caller, mut answerer) = connected_pair();
        exchange_over(&mut caller, &mut answerer, 50, noisy);
        caller.retrain();
        back_in_data(&mut caller, &mut answerer, noisy);
        let rate = caller.bit_rate();
        assert!(
            rate < 33_600 && rate == answerer.bit_rate(),
            "a retrain over a noisier line would keep {rate} bit/s"
        );
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange_over(&mut caller, &mut answerer, 80, noisy);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer) && found(&at_caller),
            "data would be lost at {rate} bit/s after a retrain"
        );
    }

    #[test]
    fn renegotiates_down_when_the_line_gets_worse() {
        let (mut caller, mut answerer) = connected_pair();
        exchange_over(&mut caller, &mut answerer, 50, noisy);
        caller.renegotiate();
        back_in_data(&mut caller, &mut answerer, noisy);
        let rate = caller.bit_rate();
        assert!(
            rate < 33_600 && rate == answerer.bit_rate(),
            "a noisier line would keep {rate} bit/s and lose data"
        );
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange_over(&mut caller, &mut answerer, 80, noisy);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer) && found(&at_caller),
            "data would be lost at {rate} bit/s after stepping down"
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

    #[test]
    fn two_ends_that_ask_for_all_of_mp_carry_data_both_ways() {
        let asks = Asks {
            trellis: mp::Trellis::States64,
            nonlinear: true,
            expanded_shaping: true,
            precoding: [(6000, -1500), (-2500, 800), (700, 300)],
        };
        let mut caller = V34::new(Role::Originate).asking(asks);
        let mut answerer = V34::new(Role::Answer).asking(Asks {
            trellis: mp::Trellis::States32,
            ..asks
        });
        let up = (0..400).find(|_| {
            exchange_over(&mut caller, &mut answerer, 1, alaw);
            caller.connected() && answerer.connected()
        });
        assert!(up.is_some(), "no V.34 connection asking for all of MP");
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        caller.push_bits(&message);
        answerer.push_bits(&message);
        let (at_caller, at_answerer) = exchange_over(&mut caller, &mut answerer, 60, alaw);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        assert!(
            found(&at_answerer),
            "the answerer lost the caller's data at {}",
            caller.bit_rate()
        );
        assert!(found(&at_caller), "the caller lost the answerer's data");
    }
}
