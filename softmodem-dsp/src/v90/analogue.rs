// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The analogue modem: PCM codewords down from the digital modem, V.34 up.

use std::collections::VecDeque;

use super::cp::Cp;
use super::design::{self, Levels};
use super::downstream::{Downstream, Events};
use super::encoder::Mapping;
use crate::passband::Complex;
use crate::pump::{DataPump, Role};
use crate::scrambler::{Polynomial, Scrambler};
use crate::uart::Decoder as Characters;
use crate::v34::encoder::{Encoder, Settings};
use crate::v34::framing::Framing;
use crate::v34::modulator::Modulator;
use crate::v34::mp::{E_ONES, Mp};
use crate::v34::phase2::{PcmOutcome, Phase2};
use crate::v34::training::{self, PP_SYMBOLS, Points};
use crate::v34::{NOMINAL_DBM0, rates};

const S_SYMBOLS: usize = 128;
const S_BAR_SYMBOLS: usize = 16;
// § 9.3.2.3: TRN for at least 512T; this modem sends twice that.
const TRN_SYMBOLS: usize = 1024;
// § 9.3.2.1: 70 ± 5 ms of silence after INFO1a.
const SILENCE_MS: f64 = 70.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Send {
    /// Ja, until the Sd to S̄d transition.
    Ja,
    /// Silence, until Jd.
    Quiet,
    /// S, until Jd′.
    S,
    /// Silence through DIL, until it has shown the levels.
    Dil,
    /// CPt, until R̄i.
    Cpt,
    /// CP, then CP′ until MP′ or Ed.
    Cp,
    /// B1 and data.
    Data,
    /// Silence once a cleardown has ended the call.
    Cleared,
}

/// The upstream V.34 signal from phase 3 on.
#[derive(Debug)]
struct Upstream {
    outcome: PcmOutcome,
    send: Send,
    queue: VecDeque<Complex>,
    training: training::Sender,
    ja: Vec<bool>,
    cpt: Option<Cp>,
    cp: Option<Cp>,
    acks: usize,
    encoder: Option<Encoder>,
    scale: f64,
    scrambler: Scrambler,
    bits: VecDeque<bool>,
    b1_frames: usize,
    b1_sent: bool,
    rate: u8,
    /// Renegotiations the digital modem has started, as far as this end has answered them.
    renegotiations: u32,
    /// A renegotiation to start at the next symbol.
    renegotiate: bool,
    /// This end started a cleardown, so its CP asks for 0 bit/s.
    clearing: bool,
}

impl Upstream {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "70 ms of symbols"
    )]
    fn new(outcome: PcmOutcome) -> Self {
        let baud = outcome.upstream.symbol_rate.baud();
        let silence = (SILENCE_MS / 1000.0 * baud) as usize;
        let mut training = training::Sender::new(Role::Answer);
        let mut queue = VecDeque::from(vec![(0.0, 0.0); silence]);
        queue.extend((0..S_SYMBOLS).map(training::s));
        queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
        queue.extend((0..PP_SYMBOLS).map(training::pp));
        queue.extend((0..TRN_SYMBOLS).map(|_| training.trn(Points::Four)));
        Self {
            outcome,
            send: Send::Ja,
            queue,
            training,
            ja: design::descriptor(outcome.uinfo).frame(),
            cpt: None,
            cp: None,
            acks: 0,
            encoder: None,
            scale: 1.0,
            scrambler: Scrambler::with(Polynomial::V34_ANSWER),
            bits: VecDeque::new(),
            b1_frames: 0,
            b1_sent: false,
            rate: 0,
            renegotiations: 0,
            renegotiate: false,
            clearing: false,
        }
    }

    // The rates its transmitter has at its symbol rate, bit n for (n + 1) · 2400 bit/s.
    fn own_rates(&self) -> u16 {
        rates::mask(self.outcome.upstream.symbol_rate, 14)
    }

    fn next(&mut self, events: &Events) -> Complex {
        while self.queue.is_empty() {
            self.refill(events);
        }
        self.queue.pop_front().unwrap_or((0.0, 0.0))
    }

    fn refill(&mut self, events: &Events) {
        // § 9.6.2.2: the responding modem answers the far R̄d; an initiating one has already begun.
        if events.renegotiations != self.renegotiations {
            self.renegotiations = events.renegotiations;
            self.renegotiate |= self.send == Send::Data;
        }
        match self.send {
            Send::Ja if events.sd => self.send = Send::Quiet,
            Send::Ja => {
                let ja = self.training.sequence(&self.ja, Points::Four);
                self.queue.extend(ja);
            }
            Send::Quiet if events.jd.is_some() => self.send = Send::S,
            Send::Quiet | Send::Dil if events.levels.is_none() => self.queue.push_back((0.0, 0.0)),
            Send::Quiet => self.send = Send::S,
            Send::S if events.jd_prime => {
                self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
                self.send = Send::Dil;
            }
            Send::S => self.queue.extend((0..2).map(training::s)),
            Send::Dil => self.design(events.levels.as_ref()),
            Send::Cpt if events.r_bar => self.send = Send::Cp,
            Send::Cpt => {
                let frame = self.cpt.as_ref().map(Cp::frame).unwrap_or_default();
                self.queue
                    .extend(self.training.sequence(&frame, Points::Four));
                if frame.is_empty() {
                    self.queue.push_back((0.0, 0.0));
                }
            }
            Send::Cp => self.cp(events),
            Send::Data if self.renegotiate => self.start_renegotiation(),
            Send::Data => self.data(),
            Send::Cleared => self.queue.push_back((0.0, 0.0)),
        }
    }

    // § 9.6.2.1: S and S̄, then CP.
    fn start_renegotiation(&mut self) {
        self.renegotiate = false;
        self.queue.extend((0..S_SYMBOLS).map(training::s));
        self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
        self.training = training::Sender::new(Role::Answer);
        self.acks = 0;
        self.b1_sent = false;
        self.send = Send::Cp;
    }

    // § 9.3.2.10: CPt and CP from what DIL showed, then S and S̄ to end DIL.
    fn design(&mut self, levels: Option<&Levels>) {
        let Some(levels) = levels else {
            self.queue.push_back((0.0, 0.0));
            return;
        };
        let (law, max_power) = (self.outcome.digital.law, self.outcome.digital.max_power);
        let rates = self.own_rates() >> 1;
        self.cpt = design::training(levels, law, max_power, rates);
        self.cp = design::data(levels, law, max_power, rates);
        self.queue.extend((0..S_SYMBOLS).map(training::s));
        self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
        self.training = training::Sender::new(Role::Answer);
        self.send = Send::Cpt;
    }

    // § 9.4.2.3 and § 9.4.2.4: CP until MP, CP′ until MP′ or Ed, then E.
    fn cp(&mut self, events: &Events) {
        if self.acks > 0 && events.mp_ack {
            let e = self.training.sequence(&[true; E_ONES], Points::Four);
            self.queue.extend(e);
            // § 9.7: after a rate sequence of 0 bit/s from either end, the call is over.
            let far_clears = events.mp.is_some_and(|mp| mp.max_answer_to_call == 0);
            if self.clearing || far_clears {
                self.send = Send::Cleared;
            } else {
                self.start_data(events.mp);
            }
            return;
        }
        let Some(cp) = &self.cp else {
            self.queue.push_back((0.0, 0.0));
            return;
        };
        let acknowledge = events.mp.is_some();
        self.acks += usize::from(acknowledge);
        let mut cp = Cp {
            acknowledge,
            ..cp.clone()
        };
        if self.clearing {
            cp.rate = 0;
        }
        let frame = cp.frame();
        self.queue
            .extend(self.training.sequence(&frame, Points::Four));
    }

    // § 9.4.2.4: the highest rate both enable, up to the maximum in MP.
    fn start_data(&mut self, mp: Option<Mp>) {
        self.send = Send::Data;
        let Some(mp) = mp else {
            return;
        };
        let both = self.own_rates() & mp.rates;
        let rate = (1..=mp.max_answer_to_call)
            .rev()
            .find(|&n| both >> (n - 1) & 1 == 1)
            .unwrap_or(0);
        let symbol_rate = self.outcome.upstream.symbol_rate;
        let Some(framing) = Framing::new(symbol_rate, u32::from(rate) * 2400, mp.expanded_shaping)
        else {
            return;
        };
        let settings = Settings {
            trellis: mp.trellis,
            nonlinear: mp.nonlinear,
            precoding: mp.precoding.unwrap_or_default(),
        };
        let encoder = Encoder::new(framing, settings);
        self.scale = encoder.energy().sqrt();
        self.encoder = Some(encoder);
        self.scrambler = Scrambler::with(Polynomial::V34_ANSWER);
        self.b1_frames = framing.p;
        self.rate = rate;
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

/// The analogue modem, from phase 2 on.
#[derive(Debug)]
pub struct Analogue {
    phase2: Phase2,
    modulator: Option<Modulator>,
    upstream: Option<Upstream>,
    downstream: Option<Downstream>,
    /// Connected once, and still in the call through renegotiations.
    online: bool,
}

impl Default for Analogue {
    fn default() -> Self {
        Self::new()
    }
}

impl Analogue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase2: Phase2::analogue(),
            modulator: None,
            upstream: None,
            downstream: None,
            online: false,
        }
    }

    /// Starts a rate renegotiation from data mode, as § 9.6.2.1 has the
    /// analogue modem do.
    pub fn renegotiate(&mut self) {
        self.start_renegotiation(false);
    }

    fn start_renegotiation(&mut self, clearing: bool) {
        let (Some(upstream), Some(downstream)) = (&mut self.upstream, &mut self.downstream) else {
            return;
        };
        if upstream.send != Send::Data {
            return;
        }
        upstream.renegotiate = true;
        upstream.clearing = clearing;
        upstream.b1_sent = false;
        downstream.expect_renegotiation();
    }

    fn start(&mut self, outcome: PcmOutcome) {
        let up = outcome.upstream;
        self.modulator = Some(Modulator::new(
            up.symbol_rate,
            up.high_carrier,
            up.pre_emphasis,
            NOMINAL_DBM0,
        ));
        let descriptor = design::descriptor(outcome.uinfo);
        self.downstream = Some(Downstream::new(
            outcome.uinfo,
            outcome.digital.law,
            &descriptor,
        ));
        self.upstream = Some(Upstream::new(outcome));
    }
}

impl DataPump for Analogue {
    fn engaged(&self) -> bool {
        self.phase2.engaged()
    }

    fn bit_rate(&self) -> u32 {
        self.upstream
            .as_ref()
            .and_then(|up| up.cp.as_ref())
            .map_or(0, Cp::bit_rate)
    }

    fn decoder(&self) -> Characters {
        Characters::v14()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        if let Some(upstream) = &mut self.upstream {
            upstream.bits.extend(bits);
        }
    }

    fn pending(&self) -> usize {
        self.upstream.as_ref().map_or(0, |up| up.bits.len())
    }

    fn transmit(&mut self, out: &mut [i16]) {
        let (Some(modulator), Some(upstream), Some(downstream)) = (
            &mut self.modulator,
            &mut self.upstream,
            &mut self.downstream,
        ) else {
            self.phase2.transmit(out);
            return;
        };
        modulator.render(out, || upstream.next(downstream.events()));
        if !downstream.has_mappings()
            && let (Some(cpt), Some(cp)) = (&upstream.cpt, &upstream.cp)
        {
            downstream.set_mappings(Mapping::from_cp(cpt), Mapping::from_cp(cp));
        }
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        let Some(downstream) = &mut self.downstream else {
            self.phase2.receive(input);
            if let Some(outcome) = self.phase2.pcm_outcome().filter(|_| self.phase2.done()) {
                self.start(outcome);
            }
            return;
        };
        downstream.receive(input, bits);
        self.online |= self.connected();
    }

    fn clear_down(&mut self) {
        self.start_renegotiation(true);
    }

    fn cleared(&self) -> bool {
        self.upstream
            .as_ref()
            .is_some_and(|up| up.send == Send::Cleared)
    }

    fn carrier(&self) -> bool {
        self.online && !self.cleared()
    }

    fn connected(&self) -> bool {
        self.upstream.as_ref().is_some_and(|up| up.b1_sent)
            && self.downstream.as_ref().is_some_and(Downstream::in_data)
    }
}
