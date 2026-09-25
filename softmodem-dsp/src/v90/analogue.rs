// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The analogue modem: PCM codewords down from the digital modem, V.34 up.

use std::collections::VecDeque;

use super::RETRAIN_TONE;
use super::cp::Cp;
use super::design::{self, Levels};
use super::downstream::{Downstream, Events};
use super::encoder::Mapping;
use super::jd::Jd;
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
use crate::v34::{NOMINAL_DBM0, rates, tones};

const S_SYMBOLS: usize = 128;
const S_BAR_SYMBOLS: usize = 16;
// § 9.3.2.3: TRN for at least 512T; this modem sends twice that.
const TRN_SYMBOLS: usize = 1024;
// § 9.3.2.1: 70 ± 5 ms of silence after INFO1a.
const SILENCE_MS: f64 = 70.0;
// CP′ until Ed, or this long: one CP′ and E fit in a lost RTP packet.
const CP_ACK_SECONDS: f64 = 0.2;
// Past a round trip, the time the digital modem takes from S̄ to enough Ri.
const RI_MARGIN_SECONDS: f64 = 0.5;

fn sixteen(sixteen: bool) -> Points {
    if sixteen {
        Points::Sixteen
    } else {
        Points::Four
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Send {
    /// Ja, until the Sd to S̄d transition.
    Ja,
    /// Silence, until Jd.
    Quiet,
    /// S, until Jd′.
    S,
    /// SCR through DIL, until it has shown the levels.
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
    jd: Option<Jd>,
    /// For CPt, CP and E, as Jd asks for phase 4 or a renegotiation (§ 8.5.2).
    points: Points,
    cpt: Option<Cp>,
    cp: Option<Cp>,
    /// Symbols of CP′ sent.
    acked: f64,
    /// Symbols of CPt sent since the last S̄.
    cpt_sent: f64,
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
            jd: None,
            points: Points::Four,
            cpt: None,
            cp: None,
            acked: 0.0,
            cpt_sent: 0.0,
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
        if self.jd.is_none() {
            self.jd = events.jd;
        }
        match self.send {
            Send::Ja if events.sd => self.send = Send::Quiet,
            Send::Ja => {
                let ja = self.training.sequence(&self.ja, Points::Four);
                self.queue.extend(ja);
            }
            Send::Quiet if events.jd.is_some() => self.send = Send::S,
            Send::Quiet if events.levels.is_none() => self.queue.push_back((0.0, 0.0)),
            // § 9.3.2.9: SCR, not silence, so that DIL meets the line as data mode will.
            Send::Dil if events.levels.is_none() => {
                let scr = self.training.sequence(&[true; 2], Points::Four);
                self.queue.extend(scr);
            }
            Send::Quiet => self.send = Send::S,
            Send::S if events.jd_prime => {
                self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
                self.send = Send::Dil;
            }
            Send::S => self.queue.extend((0..2).map(training::s)),
            Send::Dil => self.design(events.levels.as_ref()),
            Send::Cpt if events.r_bar => self.send = Send::Cp,
            Send::Cpt if !events.ri && self.cpt_sent >= self.ri_wait() => self.end_dil(),
            Send::Cpt => {
                let frame = self.cpt.as_ref().map(Cp::frame).unwrap_or_default();
                let symbols = self.training.sequence(&frame, self.points);
                self.cpt_sent += f64::from(u32::try_from(symbols.len()).unwrap_or(u32::MAX));
                self.queue.extend(symbols);
                if frame.is_empty() {
                    self.queue.push_back((0.0, 0.0));
                    self.cpt_sent += 1.0;
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
        self.points = sixteen(self.jd.is_some_and(|jd| jd.sixteen_points_renegotiating));
        self.acked = 0.0;
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
        self.points = sixteen(self.jd.is_some_and(|jd| jd.sixteen_points));
        self.send = Send::Cpt;
        self.end_dil();
    }

    // S and S̄, again until Ri comes, as a lost S̄ leaves the digital modem in DIL.
    fn end_dil(&mut self) {
        self.queue.extend((0..S_SYMBOLS).map(training::s));
        self.queue.extend((0..S_BAR_SYMBOLS).map(training::s_bar));
        self.training = training::Sender::new(Role::Answer);
        self.cpt_sent = 0.0;
    }

    // CPt symbols to wait for Ri after S̄: a round trip and some.
    #[expect(clippy::cast_precision_loss, reason = "a round trip in samples")]
    fn ri_wait(&self) -> f64 {
        let round_trip = self.outcome.round_trip as f64 / 8000.0;
        self.outcome.upstream.symbol_rate.baud() * (round_trip + RI_MARGIN_SECONDS)
    }

    // § 9.4.2.3 and § 9.4.2.4: CP until MP, CP′ until MP′ or Ed, then E.
    fn cp(&mut self, events: &Events) {
        let enough = self.acked >= self.outcome.upstream.symbol_rate.baud() * CP_ACK_SECONDS;
        if self.acked > 0.0 && events.mp_ack && (events.ed || enough) {
            let e = self.training.sequence(&[true; E_ONES], self.points);
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
        let mut cp = Cp {
            acknowledge,
            ..cp.clone()
        };
        if self.clearing {
            cp.rate = 0;
        }
        let symbols = self.training.sequence(&cp.frame(), self.points);
        if acknowledge {
            self.acked += f64::from(u32::try_from(symbols.len()).unwrap_or(u32::MAX));
        }
        self.queue.extend(symbols);
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
    outcome: Option<PcmOutcome>,
    /// Connected once, and still in the call through renegotiations and retrains.
    online: bool,
    retrain_tone: tones::Detector,
    tone_heard: usize,
    /// The bit rate before a retrain, until the retrain sets a new one.
    retrained_from: u32,
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
            outcome: None,
            online: false,
            retrain_tone: tones::Detector::new(Role::Originate),
            tone_heard: 0,
            retrained_from: 0,
        }
    }

    /// Whether it has ended DIL and not yet reached data mode.
    #[cfg(test)]
    pub(crate) fn in_phase_4(&self) -> bool {
        self.upstream
            .as_ref()
            .is_some_and(|up| matches!(up.send, Send::Cpt | Send::Cp))
    }

    // § 9.5.2: phase 2 again from the tones, with the far INFO0d kept.
    fn restart(&mut self) {
        let Some(outcome) = self.outcome.take() else {
            return;
        };
        self.retrained_from = self.bit_rate();
        self.phase2 = Phase2::retrain_analogue(outcome.digital);
        self.modulator = None;
        self.upstream = None;
        self.downstream = None;
        self.tone_heard = 0;
    }

    // § 9.3.2 and § 9.4.2: from phase 3 on, tone B starts a retrain.
    fn far_retrains(&mut self, input: &[i16]) -> bool {
        if self.upstream.is_none() {
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
        self.outcome = Some(outcome);
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
            .map_or(self.retrained_from, Cp::bit_rate)
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
        if self.far_retrains(input) {
            self.restart();
            return;
        }
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

    fn retrain(&mut self) {
        if self.connected() {
            self.restart();
        }
    }

    // § 9.6.2.1.
    fn renegotiate(&mut self) {
        self.start_renegotiation(false);
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
