// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The digital modem's receiver of the V.34 signal from the analogue
//! modem, from phase 3 on (§ 9.3.1, § 9.4.1).

use super::cp::Cp;
use super::dil::Descriptor;
use super::frames::Deframer;
use super::jd::Jd;
use crate::passband::Complex;
use crate::pump::Role;
use crate::scrambler::{Descrambler, Polynomial};
use crate::v34::decoder::Decoder;
use crate::v34::detect::{Heard, SDetector};
use crate::v34::encoder::{Encoder, Settings};
use crate::v34::framing::Framing;
use crate::v34::mp::{E_ONES, Trellis};
use crate::v34::phase2::Direction;
use crate::v34::receiver::{DELAY, Equalizer, FrontEnd, Symbol, Tracking};
use crate::v34::training::{self, PP_SYMBOLS, Points};
use crate::v34::{SymbolRate, constellation, rates};

const S_BAR_SYMBOLS: usize = 16;
const TRN_LEAST: usize = 512;
const HALF: f64 = std::f64::consts::FRAC_1_SQRT_2;
// The average energy of the 16 points on the grid of odd integers.
const SIXTEEN_ENERGY: f64 = 10.0;
// S and S̄ come before CP on 4 points, which lie between the 16, so the equaliser skips them.
const SIXTEEN_TRUST: f64 = 0.05;

/// What the digital modem asks of the analogue modem's transmitter, in MP.
pub const TRELLIS: Trellis = Trellis::States16;

fn nearest_odd(value: f64) -> f64 {
    2.0 * ((value - 1.0) / 2.0).round() + 1.0
}

/// What the receiver has heard that the transmitter acts on.
#[derive(Debug, Default)]
pub struct Events {
    /// The DIL descriptor from Ja.
    pub ja: Option<Descriptor>,
    /// S heard after Ja, which ends Jd.
    pub s: bool,
    /// The S̄ heard after Ja: the first ends Jd′, the second DIL.
    pub s_bars: u32,
    pub cpt: Option<Cp>,
    pub cp: Option<Cp>,
    /// CP′ or E, which ends MP.
    pub cp_ack: bool,
    /// The highest upstream rate the phase 3 training allows, in multiples of 2400 bit/s.
    pub trained: Option<u8>,
    /// Rate renegotiations the analogue modem has started, by the S̄ of each.
    pub renegotiations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listen {
    /// For S and the S̄ after it.
    S,
    /// On PP and TRN, known from the S̄ that began at `from`.
    Train { from: usize },
    /// Decisions on 4 points: Ja, CPt, CP and E.
    Sequences,
    /// B1 and data, from `from`.
    Data { from: usize },
    /// S in data mode, until the S̄ that starts a rate renegotiation.
    Hold,
}

/// Hears the analogue modem from its phase 3 on.
#[derive(Debug)]
pub struct Upstream {
    symbol_rate: SymbolRate,
    front_end: FrontEnd,
    equalizer: Equalizer,
    detector: SDetector,
    listen: Listen,
    count: usize,
    known: training::Sender,
    known_symbols: Vec<Complex>,
    descrambler: Descrambler,
    quadrant: u8,
    /// Ja is on 4 points, CPt, CP and E on those its Jd asks for (§ 8.5.2).
    points: Points,
    phase4_points: Points,
    renegotiation_points: Points,
    ja: Deframer<Descriptor>,
    cp: Deframer<Cp>,
    ones: usize,
    decoder: Option<Decoder>,
    scale: f64,
    frame: Vec<Complex>,
    data_descrambler: Descrambler,
    skip_bits: usize,
    events: Events,
}

impl Upstream {
    #[must_use]
    pub fn new(upstream: &Direction, jd: &Jd) -> Self {
        let points = |sixteen| {
            if sixteen {
                Points::Sixteen
            } else {
                Points::Four
            }
        };
        Self {
            symbol_rate: upstream.symbol_rate,
            front_end: FrontEnd::new(upstream.symbol_rate, upstream.high_carrier),
            equalizer: Equalizer::default(),
            detector: SDetector::default(),
            listen: Listen::S,
            count: 0,
            known: training::Sender::new(Role::Answer),
            known_symbols: Vec::new(),
            descrambler: Descrambler::with(Polynomial::V34_ANSWER),
            quadrant: 0,
            points: Points::Four,
            phase4_points: points(jd.sixteen_points),
            renegotiation_points: points(jd.sixteen_points_renegotiating),
            ja: Deframer::default(),
            cp: Deframer::default(),
            ones: 0,
            decoder: None,
            scale: 1.0,
            frame: Vec::new(),
            data_descrambler: Descrambler::with(Polynomial::V34_ANSWER),
            skip_bits: 0,
            events: Events::default(),
        }
    }

    #[must_use]
    pub fn events(&self) -> &Events {
        &self.events
    }

    /// Whether B1 has passed and data is coming.
    #[must_use]
    pub fn in_data(&self) -> bool {
        self.decoder.is_some() && self.skip_bits == 0 && matches!(self.listen, Listen::Data { .. })
    }

    #[must_use]
    pub fn symbol_rate(&self) -> SymbolRate {
        self.symbol_rate
    }

    /// The data bits in `input`.
    pub fn receive(&mut self, input: &[i16], data: &mut Vec<bool>) {
        for symbol in self.front_end.process(input) {
            self.take(&symbol, data);
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

    fn take(&mut self, symbol: &Symbol, data: &mut Vec<bool>) {
        let z = self.equalizer.output(symbol);
        let index = self.count.checked_sub(DELAY);
        self.count += 1;
        match (self.detector.push(symbol), self.listen) {
            (Some(Heard::SBar(at)), Listen::S) => {
                self.listen = Listen::Train { from: at };
                self.front_end.track(Tracking::Train);
            }
            (Some(Heard::S), Listen::Sequences) if self.events.ja.is_some() => {
                self.events.s = true;
            }
            (Some(Heard::SBar(_)), Listen::Sequences) if self.events.ja.is_some() => {
                self.events.s_bars += 1;
            }
            (Some(Heard::S), Listen::Data { .. }) => self.listen = Listen::Hold,
            (Some(Heard::SBar(_)), Listen::Data { .. } | Listen::Hold) => {
                self.expect_renegotiation();
                self.listen = Listen::Sequences;
                self.events.renegotiations += 1;
            }
            _ => {}
        }
        let Some(index) = index else {
            return;
        };
        match self.listen {
            Listen::Train { from } => self.train(index, from),
            Listen::Sequences => self.sequences(z, index),
            Listen::Data { from } if index >= from => self.data(z, data),
            Listen::S | Listen::Data { .. } | Listen::Hold => {}
        }
    }

    /// Forgets the CP of data mode, for the new one that § 9.6 has the
    /// analogue modem send after S and S̄.
    pub fn expect_renegotiation(&mut self) {
        self.events.cp = None;
        self.events.cp_ack = false;
        self.cp = Deframer::default();
        self.ones = 0;
        self.decoder = None;
        self.frame.clear();
        self.points = self.renegotiation_points;
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

    // The nearest 16-point symbol, as its label in the quarter and its quarter turns.
    fn sixteen_point(z: Complex) -> (Complex, u8, u8) {
        let scale = SIXTEEN_ENERGY.sqrt().recip();
        let mut best = ((0.0, 0.0), 0, 0, f64::INFINITY);
        for turns in 0..4 {
            for label in 0..4 {
                let (x, y) = constellation::rotate(constellation::point(usize::from(label)), turns);
                let point = (f64::from(x) * scale, f64::from(y) * scale);
                let distance = (z.0 - point.0).powi(2) + (z.1 - point.1).powi(2);
                if distance < best.3 {
                    best = (point, label, turns, distance);
                }
            }
        }
        (best.0, best.1, best.2)
    }

    // One symbol of Ja, CP or E, descrambled, after the equaliser adapts to its decision.
    fn sequence_bits(&mut self, z: Complex) -> Vec<bool> {
        let (decided, quadrant, label) = match self.points {
            Points::Four => {
                let quadrant = Self::quarter(z);
                (Some(Self::corner(quadrant)), quadrant, None)
            }
            Points::Sixteen => {
                let (point, label, turns) = Self::sixteen_point(z);
                let distance = (z.0 - point.0).powi(2) + (z.1 - point.1).powi(2);
                let trusted = (distance < SIXTEEN_TRUST).then_some(point);
                (trusted, turns, Some(label))
            }
        };
        if let Some(decided) = decided {
            self.equalizer.adapt(decided);
        }
        let turns = (quadrant + 4 - self.quadrant) % 4;
        self.quadrant = quadrant;
        let mut bits = vec![turns & 1 == 1, turns & 2 == 2];
        if let Some(label) = label {
            bits.extend([label & 1 == 1, label & 2 == 2]);
        }
        bits.into_iter()
            .map(|bit| self.descrambler.descramble(bit))
            .collect()
    }

    fn sequences(&mut self, z: Complex, index: usize) {
        for bit in self.sequence_bits(z) {
            self.ones = if bit { self.ones + 1 } else { 0 };
            if self.events.ja.is_none() {
                self.ja_bit(bit);
            } else {
                self.cp_bit(bit, index);
            }
        }
    }

    fn ja_bit(&mut self, bit: bool) {
        if let Some(ja) = self.ja.push(bit) {
            self.events.trained = Some(rates::trained_rate(
                self.symbol_rate,
                self.equalizer.error(),
            ));
            self.events.ja = Some(ja);
            self.points = self.phase4_points;
        }
    }

    fn cp_bit(&mut self, bit: bool, index: usize) {
        if let Some(cp) = self.cp.push(bit) {
            self.events.cp_ack |= cp.acknowledge && !cp.training && !cp.silence;
            if cp.training {
                self.events.cpt = Some(cp);
            } else {
                self.events.cp = Some(cp);
            }
        }
        // § 9.6.2.1.6: after CPs′ comes SCR, whose ones are not E.
        let after_cp = self.events.cp.as_ref().is_some_and(|cp| !cp.silence);
        if after_cp && self.ones == E_ONES {
            self.events.cp_ack = true;
            self.listen = Listen::Data { from: index + 1 };
        }
    }

    /// Whether data mode waits for its rate, from phase 4 or a renegotiation.
    #[must_use]
    pub fn awaits_rate(&self) -> bool {
        self.decoder.is_none()
    }

    /// Starts decoding data mode at `bit_rate`, from after B1.
    pub fn start_data(&mut self, bit_rate: u32) {
        let Some(framing) = Framing::new(self.symbol_rate, bit_rate, false) else {
            return;
        };
        self.scale = Encoder::new(framing, Settings::default()).energy().sqrt();
        self.decoder = Some(Decoder::new(framing, TRELLIS));
        self.data_descrambler = Descrambler::with(Polynomial::V34_ANSWER);
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
