// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The digital modem: V.34 up from the analogue modem, PCM codewords down.

use std::collections::VecDeque;

use super::cp::Cp;
use super::dil::Dil;
use super::encoder::{Encoder, FRAME, Mapping};
use super::jd::{ALL_RATES, Jd};
use super::training::{self, JD_PRIME_BITS, R_BAR_SYMBOLS, RI_SYMBOLS, Signs, TRN1D_SYMBOLS};
use super::upstream::{self, Events, Upstream};
use super::{Codeword, RETRAIN_TONE};
use crate::pump::{DataPump, Role};
use crate::scrambler::{Polynomial, Scrambler};
use crate::uart::Decoder as Characters;
use crate::v34::mp::Mp;
use crate::v34::phase2::{PcmOutcome, Phase2};
use crate::v34::{rates, tones};

// § 9.4.1.2: TRN2d for at least 2040T.
const TRN2D_FRAMES: usize = 340;
// § 8.6.2 and § 8.6.1.
const ED_FRAMES: usize = 2;
const B1D_FRAMES: usize = 48;
// § 9.6.1.1.1: Rd for 384T.
const RD_SYMBOLS: usize = 384;
// The most look-ahead this modem's spectral shaper takes.
const LOOKAHEAD: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Send {
    /// Silence until Ja.
    Quiet,
    /// Jd, until the far S.
    Jd,
    /// DIL, until the second far S̄.
    Dil,
    /// Ri, until CPt.
    Ri { sent: usize },
    /// TRN2d, MP, MP′ and Ed, with the constellations of CPt.
    Training,
    /// B1d and data, with the constellations of CP.
    Data,
    /// Silence once a cleardown has ended the call.
    Cleared,
    /// Silence that the analogue modem asked for with CPs, until its next CP.
    Silent,
}

/// The downstream PCM signal from phase 3 on.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag for each turn that § 9.6 and § 9.7 can take"
)]
struct Downstream {
    outcome: PcmOutcome,
    jd: Jd,
    send: Send,
    queue: VecDeque<Codeword>,
    signs: Signs,
    dil: Option<Dil>,
    encoder: Option<Encoder>,
    scrambler: Scrambler,
    /// Bits queued for the encoder, and the data frames they fill.
    pending: VecDeque<bool>,
    queued_frames: usize,
    sent_frames: usize,
    /// The data frame the current encoder stops at.
    last_frame: Option<usize>,
    acks: usize,
    data: VecDeque<bool>,
    b1_sent: bool,
    rate: u32,
    /// The mapping of CPt, and the CP of data mode.
    training: Option<Mapping>,
    cp: Option<Cp>,
    /// Renegotiations the analogue modem has started, as far as this end has answered them.
    renegotiations: u32,
    /// A renegotiation to start at the next data frame.
    renegotiate: bool,
    /// This end started a cleardown, so its MP asks for 0 bit/s.
    clearing: bool,
    /// The analogue modem has acknowledged its request for silence.
    silence: bool,
}

impl Downstream {
    fn new(outcome: PcmOutcome, jd: Jd) -> Self {
        Self {
            outcome,
            jd,
            send: Send::Quiet,
            queue: VecDeque::new(),
            signs: Signs::new(outcome.uinfo),
            dil: None,
            encoder: None,
            scrambler: Scrambler::with(Polynomial::V34_CALL),
            pending: VecDeque::new(),
            queued_frames: 0,
            sent_frames: 0,
            last_frame: None,
            acks: 0,
            data: VecDeque::new(),
            b1_sent: false,
            rate: 0,
            training: None,
            cp: None,
            renegotiations: 0,
            renegotiate: false,
            clearing: false,
            silence: false,
        }
    }

    // A refill may only change what to send, and a stray symbol would move every data frame after it.
    fn next(&mut self, events: &Events, mp: Mp) -> Codeword {
        while self.queue.is_empty() {
            self.refill(events, mp);
        }
        self.queue.pop_front().unwrap_or(Codeword::SILENCE)
    }

    fn refill(&mut self, events: &Events, mp: Mp) {
        let uinfo = self.outcome.uinfo;
        // § 9.6.1.2.2: the responding modem answers the far S̄; an initiating one has already begun.
        if events.renegotiations != self.renegotiations {
            self.renegotiations = events.renegotiations;
            self.renegotiate |= self.send == Send::Data;
        }
        match self.send {
            Send::Quiet => {
                let Some(ja) = &events.ja else {
                    self.queue.push_back(Codeword::SILENCE);
                    return;
                };
                self.dil = Some(Dil::new(ja.clone()));
                self.queue.extend(training::sd(uinfo, false));
                self.queue.extend(training::sd(uinfo, true));
                for _ in 0..TRN1D_SYMBOLS {
                    let symbol = self.signs.trn();
                    self.queue.push_back(symbol);
                }
                self.send = Send::Jd;
            }
            Send::Jd if events.s => {
                let zeros = [false; JD_PRIME_BITS];
                self.queue.extend(self.signs.sequence(&zeros));
                self.send = if self.dil.as_ref().is_some_and(|dil| !dil.is_empty()) {
                    Send::Dil
                } else {
                    Send::Ri { sent: 0 }
                };
            }
            Send::Jd => {
                let frame = self.jd.frame();
                self.queue.extend(self.signs.sequence(&frame));
            }
            Send::Dil => match &mut self.dil {
                Some(dil) if !(events.s_bars >= 2 && dil.at_boundary()) => {
                    self.queue.extend(dil.next());
                }
                _ => self.send = Send::Ri { sent: 0 },
            },
            Send::Ri { sent } => {
                if let Some(cpt) = events.cpt.as_ref().filter(|_| sent >= RI_SYMBOLS) {
                    self.queue
                        .extend(training::r([uinfo; FRAME], true, R_BAR_SYMBOLS));
                    self.start_training(cpt);
                    return;
                }
                self.queue.extend(training::r([uinfo; FRAME], false, FRAME));
                self.send = Send::Ri { sent: sent + FRAME };
            }
            Send::Training => self.training(events, mp),
            Send::Data if self.renegotiate => self.start_renegotiation(),
            Send::Data => self.data(),
            Send::Cleared => self.queue.push_back(Codeword::SILENCE),
            Send::Silent => self.silent(events),
        }
    }

    // § 9.6.1.1.1: Rd and R̄d on the largest codeword of each interval, then MP.
    fn start_renegotiation(&mut self) {
        self.renegotiate = false;
        let (Some(training), Some(cp)) = (&self.training, &self.cp) else {
            self.send = Send::Data;
            return;
        };
        let Some(data) = Mapping::from_cp(cp) else {
            return;
        };
        let largest: [u8; FRAME] =
            std::array::from_fn(|i| data.sets[i].first().copied().unwrap_or(0));
        self.queue.extend(training::r(largest, false, RD_SYMBOLS));
        self.queue.extend(training::r(largest, true, R_BAR_SYMBOLS));
        let mapping = Mapping::renegotiating(training, &data);
        self.restart_encoder(mapping);
        self.acks = 0;
        self.b1_sent = false;
        self.send = Send::Training;
    }

    fn restart_encoder(&mut self, mapping: Mapping) {
        self.encoder = Some(Encoder::new(mapping));
        self.scrambler = Scrambler::with(Polynomial::V34_CALL);
        self.pending.clear();
        self.queued_frames = 0;
        self.sent_frames = 0;
        self.last_frame = None;
    }

    fn start_training(&mut self, cpt: &Cp) {
        let Some(mapping) = Mapping::from_cp(cpt) else {
            self.queue.push_back(Codeword::SILENCE);
            return;
        };
        self.training = Some(mapping.clone());
        self.restart_encoder(mapping);
        self.queue_bits(&vec![true; self.frame_bits() * TRN2D_FRAMES]);
        self.send = Send::Training;
    }

    fn frame_bits(&self) -> usize {
        self.encoder.as_ref().map_or(1, |e| e.mapping().bits())
    }

    // Bits of whole data frames, queued for the encoder.
    fn queue_bits(&mut self, bits: &[bool]) {
        let per_frame = self.frame_bits();
        let frames = bits.len().div_ceil(per_frame);
        self.pending.extend(bits);
        self.pending
            .extend(std::iter::repeat_n(false, frames * per_frame - bits.len()));
        self.queued_frames += frames;
    }

    // One data frame from the encoder, which may take bits frames ahead.
    fn emit(&mut self, idle: bool) {
        let Some(encoder) = &mut self.encoder else {
            self.queue.push_back(Codeword::SILENCE);
            return;
        };
        let (pending, scrambler) = (&mut self.pending, &mut self.scrambler);
        let frame = encoder.frame(|| scrambler.scramble(pending.pop_front().unwrap_or(idle)));
        self.queue.extend(frame);
        self.sent_frames += 1;
    }

    // Whether the encoder would take bits beyond those queued for the next frame.
    fn wants_bits(&self) -> bool {
        let lookahead = self
            .encoder
            .as_ref()
            .map_or(0, |e| usize::from(e.mapping().lookahead));
        self.queued_frames <= self.sent_frames + lookahead
    }

    // § 9.4.1.3 and § 9.4.1.4: MP until CP, MP′ until CP′ or E, then Ed.
    fn training(&mut self, events: &Events, mp: Mp) {
        if self.last_frame.is_none() && self.wants_bits() {
            // § 9.6.1.2.4: after CPs, MP′ until CPs′, then Ed and silence.
            let silence = events.cp.as_ref().filter(|cp| cp.silence);
            let silence_acknowledged = silence.is_some_and(|cp| cp.acknowledge);
            if self.acks > 0 && (silence_acknowledged || (silence.is_none() && events.cp_ack)) {
                self.silence = silence_acknowledged;
                self.last_frame = Some(self.queued_frames + ED_FRAMES);
                self.queue_bits(&vec![false; self.frame_bits() * ED_FRAMES]);
            } else {
                let acknowledge = events.cp.is_some();
                self.acks += usize::from(acknowledge);
                let mut mp = Mp { acknowledge, ..mp };
                if self.clearing {
                    mp.max_answer_to_call = 0;
                }
                let frame = mp.frame();
                self.queue_bits(&frame);
            }
        }
        self.emit(false);
        if self.last_frame == Some(self.sent_frames) {
            if self.silence {
                self.send = Send::Silent;
            } else {
                self.start_data(events);
            }
        }
    }

    // § 9.6.1.2.5 and § 9.6.1.2.6: silence in whole data frames, until a CP without the request for it.
    fn silent(&mut self, events: &Events) {
        let (Some(training), Some(cp)) = (&self.training, &self.cp) else {
            return;
        };
        if events.cp.as_ref().is_none_or(|cp| cp.silence) {
            self.queue.extend([Codeword::SILENCE; FRAME]);
            return;
        }
        let Some(data) = Mapping::from_cp(cp) else {
            return;
        };
        let largest: [u8; FRAME] =
            std::array::from_fn(|i| training.sets[i].first().copied().unwrap_or(0));
        self.queue.extend(training::r(largest, false, RD_SYMBOLS));
        self.queue.extend(training::r(largest, true, R_BAR_SYMBOLS));
        let mapping = Mapping::renegotiating(training, &data);
        self.restart_encoder(mapping);
        self.acks = 0;
        self.silence = false;
        self.send = Send::Training;
    }

    // § 9.7: after a rate sequence of 0 bit/s from either end, the call is over.
    fn start_data(&mut self, events: &Events) {
        let far_clears = events.cp.as_ref().is_some_and(|cp| cp.rate == 0);
        if self.clearing || far_clears {
            self.send = Send::Cleared;
            return;
        }
        let Some(mapping) = events.cp.as_ref().and_then(Mapping::from_cp) else {
            return;
        };
        self.rate = events.cp.as_ref().map_or(0, Cp::bit_rate);
        self.cp.clone_from(&events.cp);
        self.restart_encoder(mapping);
        self.queue_bits(&vec![true; self.frame_bits() * B1D_FRAMES]);
        self.send = Send::Data;
    }

    fn data(&mut self) {
        while self.wants_bits() {
            let bits: Vec<bool> = (0..self.frame_bits())
                .map(|_| self.data.pop_front().unwrap_or(true))
                .collect();
            self.queue_bits(&bits);
        }
        self.emit(true);
        self.b1_sent |= self.sent_frames >= B1D_FRAMES;
    }
}

/// The digital modem, from phase 2 on.
#[derive(Debug)]
pub struct Digital {
    jd: Jd,
    phase2: Phase2,
    outcome: Option<PcmOutcome>,
    downstream: Option<Downstream>,
    upstream: Option<Upstream>,
    /// Connected once, and still in the call through renegotiations and retrains.
    online: bool,
    retrain_tone: tones::Detector,
    tone_heard: usize,
    /// The bit rate before a retrain, until the retrain sets a new one.
    retrained_from: u32,
}

impl Default for Digital {
    fn default() -> Self {
        Self::new()
    }
}

impl Digital {
    #[must_use]
    pub fn new() -> Self {
        Self {
            jd: Jd {
                rates: ALL_RATES,
                sixteen_points: false,
                sixteen_points_renegotiating: false,
                lookahead: LOOKAHEAD,
            },
            phase2: Phase2::digital(),
            outcome: None,
            downstream: None,
            upstream: None,
            online: false,
            retrain_tone: tones::Detector::new(Role::Answer),
            tone_heard: 0,
            retrained_from: 0,
        }
    }

    /// As a digital modem that has CP and E sent on 16 points, as many do.
    #[cfg(test)]
    pub(crate) fn asking_sixteen_points() -> Self {
        let mut digital = Self::new();
        digital.jd.sixteen_points = true;
        digital.jd.sixteen_points_renegotiating = true;
        digital
    }

    // § 9.5.1: phase 2 again from the tones, with the far INFO0a kept.
    fn restart(&mut self) {
        let Some(outcome) = self.outcome.take() else {
            return;
        };
        self.retrained_from = self.bit_rate();
        self.phase2 = Phase2::retrain_digital(outcome.far);
        self.downstream = None;
        self.upstream = None;
        self.tone_heard = 0;
    }

    // § 9.3.1 and § 9.4.1: from phase 3 on, tone A starts a retrain.
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
        let (Some(downstream), Some(upstream)) = (&mut self.downstream, &mut self.upstream) else {
            return;
        };
        if downstream.send != Send::Data {
            return;
        }
        downstream.renegotiate = true;
        downstream.clearing = clearing;
        downstream.b1_sent = false;
        upstream.expect_renegotiation();
    }

    // Table 16: what this end's receiver asks of the analogue modem's transmitter.
    fn mp(&self) -> Mp {
        let (Some(outcome), Some(upstream)) = (self.outcome, &self.upstream) else {
            return Mp::default();
        };
        let trained = upstream
            .events()
            .trained
            .unwrap_or(outcome.upstream.max_rate);
        Mp {
            max_answer_to_call: trained.min(14),
            rates: rates::mask(upstream.symbol_rate(), 14) & !1,
            trellis: upstream::TRELLIS,
            ..Mp::default()
        }
    }

    // § 9.4.2.4: the highest rate both enable, up to the maximum in MP.
    fn agree_upstream(&mut self) {
        let mp = self.mp();
        let Some(upstream) = &mut self.upstream else {
            return;
        };
        let Some(cp) = &upstream.events().cp else {
            return;
        };
        if !upstream.awaits_rate() {
            return;
        }
        let both = mp.rates & (cp.upstream_rates << 1);
        let rate = (1..=mp.max_answer_to_call)
            .rev()
            .find(|&n| both >> (n - 1) & 1 == 1)
            .unwrap_or(0);
        if rate > 0 {
            upstream.start_data(u32::from(rate) * 2400);
        }
    }
}

impl DataPump for Digital {
    fn engaged(&self) -> bool {
        self.phase2.engaged()
    }

    fn bit_rate(&self) -> u32 {
        self.downstream
            .as_ref()
            .map_or(self.retrained_from, |d| d.rate)
    }

    fn decoder(&self) -> Characters {
        Characters::v14()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        if let Some(downstream) = &mut self.downstream {
            downstream.data.extend(bits);
        }
    }

    fn pending(&self) -> usize {
        self.downstream.as_ref().map_or(0, |d| d.data.len())
    }

    fn transmit(&mut self, out: &mut [i16]) {
        let mp = self.mp();
        let (Some(downstream), Some(upstream)) = (&mut self.downstream, &self.upstream) else {
            self.phase2.transmit(out);
            return;
        };
        let law = downstream.outcome.digital.law;
        for sample in out {
            *sample = downstream.next(upstream.events(), mp).linear(law);
        }
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        if self.far_retrains(input) {
            self.restart();
            return;
        }
        let Some(upstream) = &mut self.upstream else {
            self.phase2.receive(input);
            if let Some(outcome) = self.phase2.pcm_outcome().filter(|_| self.phase2.done()) {
                self.outcome = Some(outcome);
                self.upstream = Some(Upstream::new(&outcome.upstream, &self.jd));
                self.downstream = Some(Downstream::new(outcome, self.jd));
            }
            return;
        };
        upstream.receive(input, bits);
        self.agree_upstream();
        self.online |= self.connected();
    }

    fn retrain(&mut self) {
        if self.connected() {
            self.restart();
        }
    }

    // § 9.6.1.1.
    fn renegotiate(&mut self) {
        self.start_renegotiation(false);
    }

    fn clear_down(&mut self) {
        self.start_renegotiation(true);
    }

    fn cleared(&self) -> bool {
        self.downstream
            .as_ref()
            .is_some_and(|d| d.send == Send::Cleared)
    }

    fn carrier(&self) -> bool {
        self.online && !self.cleared()
    }

    fn connected(&self) -> bool {
        self.downstream.as_ref().is_some_and(|d| d.b1_sent)
            && self.upstream.as_ref().is_some_and(Upstream::in_data)
    }
}
