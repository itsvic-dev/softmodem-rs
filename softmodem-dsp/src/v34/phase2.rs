// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Phase 2 of § 11.2, as its error-free procedures give it: INFO0, the round
//! trip delay from the phase reversals of tones A and B, line probing with
//! L1 and L2, and INFO1, which fixes the symbol rate and carrier each way.
//! Phase 2 of V.90 (§ 9.2/V.90) is the same, with the digital modem in the
//! part of the call modem and its own INFO0d and INFO1a.

use std::collections::VecDeque;

use super::SymbolRate;
use super::info::{Deframer, Info, Info0, Info1a, Info1c, Probe, TransmitClock};
use super::probing::{self, Analyser, L1_DBM0, L1_SAMPLES, L2_DBM0, Probing};
use super::{GUARD_DBM0, GUARD_HZ, NOMINAL_DBM0, dpsk, rates, tones};
use crate::pump::Role;
use crate::tone::Tone;
use crate::v90::info::{self as pcm, Info0d};
use crate::v90::ucode;

// § 11.1: 75 ms of silence after CJ.
const CJ_SILENCE: usize = 600;
// § 11.5: 70 ms of silence before the tone of a retrain.
const RETRAIN_SILENCE: usize = 560;
// § 11.2.1: a reversal answers one 40 ms after it arrives.
const ANSWER_DELAY: usize = 320;
// § 11.2.1: each tone goes on 10 ms after its reversal.
const TAIL: usize = 80;
// § 11.2.1.2.3 and § 11.2.1.2.6: tone A for at least 50 ms first.
const TONE_FIRST: usize = 400;
// 150 ms past the reversal, not 10: slmodemd hears silence sooner as a restart.
const HELD_TAIL: usize = 1200;
// § 11.2.1.1.7 and § 11.2.1.2.6: L2 for at most 550 ms and a round trip.
const L2_MOST: usize = 4400;
// L2 is measured from 20 ms in, for 400 ms, within the 500 ms of § 11.2.1.1.5.
const L2_SETTLE: usize = 160;
const L2_MEASURED: usize = 3200;
// The call modem listens for tone A again only after this much of its L2.
const L2_LEAST: usize = 800;
// INFO flips its carrier on each 1, so its fill bits must not pass for tone A's reversal.
const AFTER_INFO: usize = 160;
// A reversal counts once the tone after it shows no INFO, still before the 40 ms reply.
const CONFIRM: usize = 160;
const LONE_BEFORE: usize = 160;
// § 11.2.2.2.2: the answer reverses tone A again after 2 s with no reply.
const REPLY_MOST: usize = 16_000;
// § 11.2.2.2.4: the answer waits 2 s and two round trips for the far INFO1.
const INFO1_MOST: usize = 16_000;
// The far tone counts as alone after this long with no 1s, which no INFO0 has.
const QUIET: usize = 320;
const LONE_ONES: usize = 2;
const ONES_KEPT: usize = 64;
// Bits 79:88 of INFO1c and 40:49 of INFO1a count 0.02 Hz.
const STEPS_PER_HZ: f64 = 50.0;
// § 8.2.3.2/V.90: UINFO above 66.
const LEAST_UINFO: u8 = 67;
// Bits 29:32 of INFO0d: the nominal power in dB below -6 dBm0.
const NOMINAL_STEPS: u8 = 7;
// Bits 33:37 of INFO0d: -12 dBm0, the limit on V.90 in the United States.
const MAX_POWER_STEPS: u8 = 23;

/// One direction of the call as phase 2 left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Direction {
    pub symbol_rate: SymbolRate,
    pub high_carrier: bool,
    pub pre_emphasis: u8,
    /// In multiples of 2400 bit/s, as probing judged it.
    pub max_rate: u8,
}

/// What phase 2 settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Outcome {
    pub transmit: Direction,
    pub receive: Direction,
    /// In samples.
    pub round_trip: usize,
    pub far: Info0,
}

/// What phase 2 of V.90 settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcmOutcome {
    /// From the analogue modem to the digital modem.
    pub upstream: Direction,
    /// The Ucode of the two points that the digital modem trains with.
    pub uinfo: u8,
    /// In samples.
    pub round_trip: usize,
    /// The far INFO0, or the V.34 part of the far INFO0d.
    pub far: Info0,
    /// The digital modem's INFO0d.
    pub digital: Info0d,
}

/// Which INFO sequences phase 2 exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    V34,
    Digital,
    Analogue,
}

fn own_info0d() -> Info0d {
    Info0d {
        v34: own_info0(),
        nominal_power: NOMINAL_STEPS,
        max_power: MAX_POWER_STEPS,
        power_at_codec: true,
        law: ucode::Law::A,
        upstream_3429: true,
    }
}

// The two points at the nominal power, or at the far maximum if that is lower.
fn uinfo(digital: &Info0d) -> u8 {
    let dbm0 = NOMINAL_DBM0.min(digital.max_power_dbm0());
    let rms = crate::sine_peak(dbm0) / std::f64::consts::SQRT_2;
    ucode::nearest(rms, digital.law).max(LEAST_UINFO)
}

fn own_info0_frame(mode: Mode, acknowledge: bool) -> Vec<bool> {
    if mode == Mode::Digital {
        let mut info0d = own_info0d();
        info0d.v34.acknowledge = acknowledge;
        info0d.frame()
    } else {
        Info0 {
            acknowledge,
            ..own_info0()
        }
        .frame()
    }
}

fn own_info0() -> Info0 {
    Info0 {
        supports_2743: true,
        supports_2800: true,
        supports_3429: true,
        low_carrier_3000: true,
        high_carrier_3000: true,
        low_carrier_3200: true,
        high_carrier_3200: true,
        allows_3429: true,
        reduces_power: false,
        rate_difference: 0,
        cme: false,
        constellation_1664: true,
        clock: TransmitClock::Internal,
        acknowledge: false,
    }
}

fn shared(rate: SymbolRate, far: &Info0) -> bool {
    match rate {
        SymbolRate::S2743 => far.supports_2743,
        SymbolRate::S2800 => far.supports_2800,
        SymbolRate::S3429 => far.supports_3429 && far.allows_3429,
        _ => true,
    }
}

// Bits 15 to 18 of INFO0 say which carriers the far transmitter may use.
fn far_carriers(rate: SymbolRate, far: &Info0) -> [bool; 2] {
    match rate {
        SymbolRate::S3000 => [far.low_carrier_3000, far.high_carrier_3000],
        SymbolRate::S3200 => [far.low_carrier_3200, far.high_carrier_3200],
        _ => [true, true],
    }
}

fn index(rate: SymbolRate) -> usize {
    usize::from(rate.index())
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "clamped to the ten bits first"
)]
fn offset(probing: &Probing) -> Option<i16> {
    probing
        .frequency_offset_hz
        .map(|hz| (hz * STEPS_PER_HZ).round().clamp(-511.0, 511.0) as i16)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    SendInfo0,
    /// § 11.2.2.1.1 and § 11.2.2.2.1: INFO0 again and again, as one end lacks the other's.
    RepeatInfo0,
    /// Answer: tone A, waiting for the call's INFO0 and tone B.
    ToneA,
    /// Waiting for the far reversal that answers ours.
    AwaitReply {
        ours: usize,
    },
    /// Answer: L1 and L2 until tone B.
    Probe,
    /// Answer: tone A after L2 ran its longest, until tone B.
    LateToneB,
    /// Call: tone B, waiting for the reversal of § 11.2.1.1.3.
    ToneB,
    /// Answer: tone A, reversed once, until the far reversal that starts its L1 and L2.
    AwaitProbe {
        reversed: bool,
    },
    Measure {
        from: usize,
    },
    /// The answer waits for the call's INFO1; the call for the reversal before its L1.
    AfterProbe,
    /// Call: L1 and L2 until tone A.
    ProbeFar,
    SendInfo1,
    AwaitInfo1a,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tx {
    Silence,
    Info,
    Tone,
    L1,
    L2,
}

/// Phase 2 from either end.
#[derive(Debug)]
pub struct Phase2 {
    role: Role,
    mode: Mode,
    step: Step,
    tx: Tx,
    tx_until: Option<usize>,
    sent: usize,
    heard: usize,
    tone_from: usize,
    // Where this end's tone started, in samples heard: a far reversal before it is stale.
    tone_heard_from: usize,
    l2_from: usize,
    info: dpsk::Modulator,
    tone: tones::Sender,
    guard: Option<Tone>,
    l1: probing::Sender,
    l2: probing::Sender,
    demodulator: dpsk::Demodulator,
    info0: Deframer<Info0>,
    info0d: Deframer<Info0d>,
    info1c: Deframer<Info1c>,
    info1a: Deframer<Info1a>,
    pcm_info1a: Deframer<pcm::Info1a>,
    detector: tones::Detector,
    analyser: Analyser,
    reversals: Vec<f64>,
    // Where the demodulator gave its last 1s, roughly.
    ones: VecDeque<usize>,
    // Tone B held past the call modem's reversal must stop before a tone B can end L2.
    tone_b_gone: bool,
    far: Option<Info0>,
    far_info0d: Option<Info0d>,
    far_at: usize,
    // Far INFO0 sequences that lack our own, heard since this end last started its tone.
    far_repeats: usize,
    far_acknowledged: bool,
    // Where the answer began to wait for the far INFO1.
    info1_from: usize,
    // How long the far tone has been missing, and whether it went for long enough to start a retrain.
    far_absent: usize,
    far_went_quiet: bool,
    round_trip: usize,
    probing: Option<Probing>,
    outcome: Option<Outcome>,
    pcm_outcome: Option<PcmOutcome>,
}

impl Phase2 {
    /// Starting at the end of CJ, with the 75 ms of silence that § 11.2.1
    /// listens through.
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self::with(role, Mode::V34)
    }

    /// Phase 2 of V.90 from the digital modem, which takes the part of the
    /// V.34 call modem.
    #[must_use]
    pub fn digital() -> Self {
        Self::with(Role::Originate, Mode::Digital)
    }

    /// Phase 2 of V.90 from the analogue modem, which takes the part of the
    /// V.34 answer modem.
    #[must_use]
    pub fn analogue() -> Self {
        Self::with(Role::Answer, Mode::Analogue)
    }

    fn with(role: Role, mode: Mode) -> Self {
        let far = match role {
            Role::Answer => Role::Originate,
            Role::Originate => Role::Answer,
        };
        let mut info = dpsk::Modulator::new(role);
        info.send(own_info0_frame(mode, false));
        Self {
            role,
            mode,
            step: Step::SendInfo0,
            tx: Tx::Silence,
            tx_until: Some(CJ_SILENCE),
            sent: 0,
            heard: 0,
            tone_from: 0,
            tone_heard_from: 0,
            l2_from: 0,
            info,
            tone: tones::Sender::new(role),
            guard: (role == Role::Answer).then(|| Tone::new(GUARD_HZ, GUARD_DBM0)),
            l1: probing::Sender::new(L1_DBM0),
            l2: probing::Sender::new(L2_DBM0),
            demodulator: dpsk::Demodulator::new(far),
            info0: Deframer::default(),
            info0d: Deframer::default(),
            info1c: Deframer::default(),
            info1a: Deframer::default(),
            pcm_info1a: Deframer::default(),
            detector: tones::Detector::new(far),
            analyser: Analyser::new(L2_DBM0),
            reversals: Vec::new(),
            ones: VecDeque::new(),
            tone_b_gone: false,
            far: None,
            far_info0d: None,
            far_at: 0,
            far_repeats: 0,
            far_acknowledged: false,
            info1_from: 0,
            far_absent: 0,
            far_went_quiet: false,
            round_trip: 0,
            probing: None,
            outcome: None,
            pcm_outcome: None,
        }
    }

    /// Phase 2 again, to answer or start a retrain (§ 11.5): 70 ms of
    /// silence, then tone A or B and the rest of phase 2 from the tones, with
    /// the far INFO0 heard before.
    #[must_use]
    pub fn retrain(role: Role, far: Info0) -> Self {
        Self::new(role).retraining(far)
    }

    /// Phase 2 of V.90 again from the digital modem (§ 9.5.1), with the far
    /// INFO0a heard before.
    #[must_use]
    pub fn retrain_digital(far: Info0) -> Self {
        Self::digital().retraining(far)
    }

    /// Phase 2 of V.90 again from the analogue modem (§ 9.5.2), with the far
    /// INFO0d heard before.
    #[must_use]
    pub fn retrain_analogue(far: Info0d) -> Self {
        let mut phase2 = Self::analogue().retraining(far.v34);
        phase2.far_info0d = Some(far);
        phase2
    }

    // Phase 2 again from the tones, which the far end answers if it starts it and this end if the far end does.
    fn restart(&mut self) {
        let Some(far) = self.far else {
            return;
        };
        let again = match self.mode {
            Mode::Analogue => match self.far_info0d {
                Some(digital) => Self::retrain_analogue(digital),
                None => return,
            },
            Mode::Digital => Self::retrain_digital(far),
            Mode::V34 => Self::retrain(self.role, far),
        };
        *self = again;
    }

    fn retraining(mut self, far: Info0) -> Self {
        self.info = dpsk::Modulator::new(self.role);
        self.far = Some(far);
        self.step = match self.role {
            Role::Answer => Step::ToneA,
            Role::Originate => Step::ToneB,
        };
        self.tx_until = Some(RETRAIN_SILENCE);
        self
    }

    /// What phase 2 settled, once it has.
    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    /// What phase 2 of V.90 settled, once it has. A digital modem whose far
    /// end picked V.34 in INFO1a has an [`Outcome`] instead.
    #[must_use]
    pub fn pcm_outcome(&self) -> Option<PcmOutcome> {
        self.pcm_outcome
    }

    /// Whether the far end has answered with its INFO0.
    #[must_use]
    pub fn engaged(&self) -> bool {
        self.far.is_some()
    }

    /// Whether this end has done its part of phase 2.
    #[must_use]
    pub fn done(&self) -> bool {
        self.step == Step::Done
    }

    fn start(&mut self, tx: Tx, until: Option<usize>) {
        if tx == Tx::Tone && self.tx != Tx::Tone {
            self.tone_from = self.sent;
            self.tone_heard_from = self.heard;
            self.far_repeats = 0;
        }
        if tx == Tx::L2 {
            self.l2_from = self.sent;
        }
        self.tx = tx;
        self.tx_until = until;
    }

    // Reverses the tone at sample `at`, and stops it `TAIL` later.
    fn reverse_at(&mut self, at: usize) {
        self.tone.reverse_after(at.saturating_sub(self.sent));
        let tx = self.tx;
        self.start(Tx::Tone, Some(at.max(self.sent) + TAIL));
        if tx == Tx::Tone {
            self.tx = Tx::Tone;
        }
    }

    fn segment_ended(&mut self) {
        let (tx, until) = match (self.step, self.tx) {
            (Step::SendInfo0, Tx::Silence) => (Tx::Info, None),
            (Step::ToneA | Step::ToneB, Tx::Silence) => (Tx::Tone, None),
            (Step::Probe, Tx::L2) => {
                self.step = Step::LateToneB;
                (Tx::Tone, None)
            }
            (Step::Probe | Step::ProbeFar, Tx::Tone) => (Tx::L1, Some(self.sent + L1_SAMPLES)),
            (Step::Probe | Step::ProbeFar, Tx::L1) => {
                (Tx::L2, Some(self.sent + L2_MOST + self.round_trip))
            }
            (Step::ProbeFar, Tx::L2) => {
                self.send_info1();
                return;
            }
            _ => (Tx::Silence, None),
        };
        self.start(tx, until);
    }

    pub fn transmit(&mut self, out: &mut [i16]) {
        let mut done = 0;
        while done < out.len() {
            let length = self
                .tx_until
                .map_or(out.len() - done, |until| until.saturating_sub(self.sent))
                .min(out.len() - done);
            let part = &mut out[done..done + length];
            match self.tx {
                Tx::Silence => part.fill(0),
                Tx::Info => self.info.render(part),
                Tx::Tone => self.tone.render(part),
                Tx::L1 => self.l1.render(part),
                Tx::L2 => self.l2.render(part),
            }
            if matches!(self.tx, Tx::Info | Tx::Tone)
                && let Some(guard) = &mut self.guard
            {
                let mut tone = vec![0; part.len()];
                guard.render(&mut tone);
                for (sample, tone) in part.iter_mut().zip(tone) {
                    *sample = sample.saturating_add(tone);
                }
            }
            self.sent += length;
            done += length;
            if self.tx_until.is_some_and(|until| self.sent >= until) {
                self.segment_ended();
            }
        }
        if self.tx == Tx::Info && self.info.idle() {
            self.info_sent();
        }
    }

    fn info_sent(&mut self) {
        match self.step {
            Step::SendInfo0 => self.tone_after_info0(),
            Step::RepeatInfo0
                if self.far.is_some() && (self.far_acknowledged || self.far_tone_alone()) =>
            {
                self.tone_after_info0();
            }
            Step::RepeatInfo0 => self.repeat_info0(),
            Step::SendInfo1 => {
                self.start(Tx::Silence, None);
                self.step = match self.role {
                    Role::Answer => Step::Done,
                    Role::Originate => Step::AwaitInfo1a,
                };
                self.far_went_quiet = false;
            }
            _ => self.start(Tx::Silence, None),
        }
    }

    fn tone_after_info0(&mut self) {
        self.start(Tx::Tone, None);
        self.step = match self.role {
            Role::Answer => Step::ToneA,
            Role::Originate => Step::ToneB,
        };
    }

    fn repeat_info0(&mut self) {
        self.info
            .send(own_info0_frame(self.mode, self.far.is_some()));
        self.start(Tx::Info, None);
        self.step = Step::RepeatInfo0;
    }

    fn far_tone_alone(&self) -> bool {
        self.detector.present()
            && self
                .ones
                .back()
                .is_none_or(|&one| self.heard >= one + QUIET)
    }

    // A reversal of the tone is one 1 from the demodulator, and the start of an INFO many.
    fn lone(&self, at: usize) -> bool {
        let (from, to) = (at.saturating_sub(LONE_BEFORE), at + TAIL);
        let ones = self
            .ones
            .iter()
            .filter(|&&one| (from..to).contains(&one))
            .count();
        ones <= LONE_ONES
    }

    // § 11.2.2.1.1 and § 11.2.2.2.1: the far tone with no far INFO0, or the far INFO0 again.
    fn info0_lost(&self) -> bool {
        let listening = match self.step {
            Step::ToneA | Step::ToneB => true,
            Step::AwaitReply { .. } => self.role == Role::Answer,
            _ => false,
        };
        listening
            && self.tx == Tx::Tone
            && ((self.far.is_none() && self.far_tone_alone())
                || (self.far_repeats > 1 && !self.far_acknowledged))
    }

    pub fn receive(&mut self, input: &[i16]) {
        let mut bits = Vec::new();
        self.demodulator.process(input, &mut bits);
        let step = input.len() / bits.len().max(1);
        for (n, _) in bits.iter().enumerate().filter(|&(_, &bit)| bit) {
            self.ones.push_back(self.heard + (n + 1) * step);
        }
        while self.ones.len() > ONES_KEPT {
            self.ones.pop_front();
        }
        for bit in bits {
            self.info0_bit(bit, self.heard + input.len());
            self.info1_bit(bit);
        }
        self.reversals.extend(self.detector.process(input));
        self.far_absent = if self.detector.present() {
            0
        } else {
            self.far_absent + input.len()
        };
        if let Step::Measure { from } = self.step {
            let start = from.saturating_sub(self.heard).min(input.len());
            let end = (from + L2_MEASURED)
                .saturating_sub(self.heard)
                .min(input.len());
            self.analyser.push(&input[start..end.max(start)]);
        }
        self.heard += input.len();
        self.advance();
    }

    // The far INFO0, or INFO0d, heard by sample `at`.
    fn info0_bit(&mut self, bit: bool, at: usize) {
        let far = if self.mode == Mode::Analogue {
            self.info0d.push(bit).map(|info0d| {
                self.far_info0d = Some(info0d);
                info0d.v34
            })
        } else {
            self.info0.push(bit)
        };
        if let Some(info0) = far {
            self.far.get_or_insert(info0);
            self.far_at = at;
            self.far_repeats += usize::from(!info0.acknowledge);
            self.far_acknowledged |= info0.acknowledge;
        }
    }

    fn info1_bit(&mut self, bit: bool) {
        if self.role == Role::Answer
            && let Some(info1c) = self.info1c.push(bit)
            && self.step == Step::AfterProbe
        {
            if self.mode == Mode::Analogue {
                self.decide_pcm(&info1c);
            } else {
                self.decide(&info1c);
            }
        }
        if self.role == Role::Originate
            && let Some(info1a) = self.info1a.push(bit)
            && self.step == Step::AwaitInfo1a
        {
            self.settle(&info1a);
        }
        if self.mode == Mode::Digital
            && let Some(info1a) = self.pcm_info1a.push(bit)
            && self.step == Step::AwaitInfo1a
        {
            self.settle_pcm(info1a);
        }
    }

    // A confirmed lone reversal of the far tone, after its INFO0 and while its tone is heard.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "sample positions are small and positive"
    )]
    fn next_reversal(&mut self) -> Option<usize> {
        self.reversals
            .first()
            .map(|&at| at.round() as usize)
            .filter(|&at| self.heard >= at + CONFIRM)
            .and_then(|at| {
                self.reversals.remove(0);
                self.lone(at).then_some(at)
            })
            .filter(|&at| {
                self.far.is_some() && at >= self.far_at + AFTER_INFO && at >= self.tone_heard_from
            })
    }

    fn await_reply(&mut self, ours: usize, reversal: Option<usize>) {
        if let Some(at) = reversal.filter(|&at| at > ours) {
            self.replied(ours, at);
        } else if self.role == Role::Answer && self.sent >= ours + REPLY_MOST {
            self.step = Step::ToneA;
        }
    }

    // § 11.2.2.1.6 and § 9.2.1.2.6/V.90: the far end retrains, as silence and then its tone show.
    fn await_info1a(&mut self) {
        self.far_went_quiet |= self.far_absent >= RETRAIN_SILENCE / 2;
        if self.far_went_quiet && self.far_tone_alone() {
            self.restart();
        }
    }

    fn advance(&mut self) {
        let reversal = self.next_reversal();
        self.tone_b_gone |= !self.detector.present();
        if self.info0_lost() {
            self.repeat_info0();
            return;
        }
        match self.step {
            Step::ToneA
                if self.far.is_some()
                    && self.tx == Tx::Tone
                    && self.far_tone_alone()
                    && self.sent >= self.tone_from + TONE_FIRST =>
            {
                let at = self.sent;
                self.tone.reverse_after(0);
                self.step = Step::AwaitReply { ours: at };
            }
            Step::ToneB if self.far.is_some() => {
                if let Some(at) = reversal {
                    let ours = at + ANSWER_DELAY;
                    self.reverse_at(ours);
                    self.tx_until = Some(ours.max(self.sent) + HELD_TAIL);
                    self.step = Step::AwaitReply { ours };
                }
            }
            Step::AwaitReply { ours } => self.await_reply(ours, reversal),
            Step::Probe if self.tx == Tx::L2 && self.tone_b_gone && self.detector.present() => {
                self.start(Tx::Tone, None);
                self.step = Step::AwaitProbe { reversed: false };
            }
            Step::LateToneB if self.tone_b_gone && self.detector.present() => {
                self.tone_from = self.sent;
                self.step = Step::AwaitProbe { reversed: false };
            }
            Step::AwaitProbe { reversed } => self.await_probe(reversed, reversal),
            Step::Measure { from } if self.heard >= from + L2_MEASURED => {
                self.probing = self.analyser.result();
                self.start(Tx::Tone, None);
                self.step = Step::AfterProbe;
                self.info1_from = self.heard;
            }
            // § 11.2.2.2.4 and § 9.2.2.2.4/V.90: no INFO1 in time, so a retrain.
            Step::AfterProbe
                if self.role == Role::Answer
                    && self.heard >= self.info1_from + INFO1_MOST + 2 * self.round_trip =>
            {
                self.restart();
            }
            Step::AwaitInfo1a => self.await_info1a(),
            Step::AfterProbe if self.role == Role::Originate => {
                if let Some(at) = reversal {
                    self.reverse_at(at + ANSWER_DELAY);
                    self.step = Step::ProbeFar;
                }
            }
            Step::ProbeFar
                if self.tx == Tx::L2
                    && self.sent >= self.l2_from + L2_LEAST
                    && self.detector.present() =>
            {
                self.send_info1();
            }
            _ => {}
        }
    }

    // The far reversal at `at` that answers ours at `ours` gives the round trip.
    fn replied(&mut self, ours: usize, at: usize) {
        self.round_trip = at.saturating_sub(ours + ANSWER_DELAY);
        match self.role {
            Role::Answer => {
                self.reverse_at(at + ANSWER_DELAY);
                self.step = Step::Probe;
                self.tone_b_gone = false;
            }
            Role::Originate => self.measure_from(at),
        }
    }

    fn await_probe(&mut self, reversed: bool, reversal: Option<usize>) {
        if !reversed && self.sent >= self.tone_from + TONE_FIRST {
            self.tone.reverse_after(0);
            self.tx_until = Some(self.sent + HELD_TAIL);
            self.step = Step::AwaitProbe { reversed: true };
        }
        if let Some(at) = reversal {
            self.start(Tx::Silence, None);
            self.measure_from(at);
        }
    }

    fn measure_from(&mut self, at: usize) {
        self.step = Step::Measure {
            from: at + TAIL + L1_SAMPLES + L2_SETTLE,
        };
    }

    fn probe_for(&self, rate: SymbolRate) -> Probe {
        let (Some(probing), Some(far)) = (&self.probing, &self.far) else {
            return Probe::default();
        };
        if !shared(rate, far) {
            return Probe::default();
        }
        rates::probe(probing, rate, far_carriers(rate, far))
    }

    fn send_info1(&mut self) {
        let mut probes = [Probe::default(); 6];
        for rate in SymbolRate::ALL {
            probes[index(rate)] = self.probe_for(rate);
        }
        let info1c = Info1c {
            min_power_reduction: 0,
            extra_power_reduction: 0,
            md_length: 0,
            probes,
            frequency_offset: self.probing.as_ref().and_then(offset),
        };
        self.info.send(info1c.frame());
        self.start(Tx::Info, None);
        self.step = Step::SendInfo1;
    }

    // § 11.2.1.2.9: the answer modem picks the symbol rate both ways.
    fn decide(&mut self, info1c: &Info1c) {
        let Some(far) = self.far else {
            return;
        };
        let best = SymbolRate::ALL
            .into_iter()
            .filter(|&rate| shared(rate, &far))
            .map(|rate| {
                let down = info1c.probes[index(rate)];
                let up = self.probe_for(rate);
                (rate, down, up, down.max_rate.min(up.max_rate))
            })
            .max_by_key(|&(rate, _, _, both)| (both, rate));
        let Some((rate, down, up, _)) = best else {
            return;
        };
        let reply = Info1a {
            min_power_reduction: 0,
            extra_power_reduction: 0,
            md_length: 0,
            probe: up,
            answer_to_call: rate,
            call_to_answer: rate,
            frequency_offset: self.probing.as_ref().and_then(offset),
        };
        self.info.send(reply.frame());
        self.start(Tx::Info, None);
        self.step = Step::SendInfo1;
        self.outcome = Some(Outcome {
            transmit: Direction {
                symbol_rate: rate,
                high_carrier: down.high_carrier,
                pre_emphasis: down.pre_emphasis,
                max_rate: down.max_rate,
            },
            receive: Direction {
                symbol_rate: rate,
                high_carrier: up.high_carrier,
                pre_emphasis: 0,
                max_rate: up.max_rate,
            },
            round_trip: self.round_trip,
            far,
        });
    }

    fn settle(&mut self, info1a: &Info1a) {
        let Some(far) = self.far else {
            return;
        };
        let down = self.probe_for(info1a.answer_to_call);
        self.outcome = Some(Outcome {
            transmit: Direction {
                symbol_rate: info1a.call_to_answer,
                high_carrier: info1a.probe.high_carrier,
                pre_emphasis: info1a.probe.pre_emphasis,
                max_rate: info1a.probe.max_rate,
            },
            receive: Direction {
                symbol_rate: info1a.answer_to_call,
                high_carrier: down.high_carrier,
                pre_emphasis: 0,
                max_rate: down.max_rate,
            },
            round_trip: self.round_trip,
            far,
        });
        self.step = Step::Done;
    }

    // § 9.2.2.1.9/V.90: the analogue modem picks V.90 and its upstream symbol rate.
    fn decide_pcm(&mut self, info1d: &Info1c) {
        let (Some(far), Some(digital)) = (self.far, self.far_info0d) else {
            return;
        };
        let best = [SymbolRate::S3000, SymbolRate::S3200, SymbolRate::S3429]
            .into_iter()
            .filter(|&rate| rate != SymbolRate::S3429 || digital.upstream_3429)
            .map(|rate| (rate, info1d.probes[index(rate)]))
            .max_by_key(|&(rate, probe)| (probe.max_rate, rate));
        let Some((rate, up)) = best else {
            return;
        };
        let uinfo = uinfo(&digital);
        let reply = pcm::Info1a {
            md_length: 0,
            uinfo,
            upstream: rate,
            frequency_offset: self.probing.as_ref().and_then(offset),
        };
        self.info.send(reply.frame());
        self.start(Tx::Info, None);
        self.step = Step::SendInfo1;
        self.pcm_outcome = Some(PcmOutcome {
            upstream: Direction {
                symbol_rate: rate,
                high_carrier: up.high_carrier,
                pre_emphasis: up.pre_emphasis,
                max_rate: up.max_rate,
            },
            uinfo,
            round_trip: self.round_trip,
            far,
            digital,
        });
    }

    fn settle_pcm(&mut self, info1a: pcm::Info1a) {
        let Some(far) = self.far else {
            return;
        };
        let up = self.probe_for(info1a.upstream);
        self.pcm_outcome = Some(PcmOutcome {
            upstream: Direction {
                symbol_rate: info1a.upstream,
                high_carrier: up.high_carrier,
                pre_emphasis: 0,
                max_rate: up.max_rate,
            },
            uinfo: info1a.uinfo,
            round_trip: self.round_trip,
            far,
            digital: own_info0d(),
        });
        self.step = Step::Done;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: usize = 160;

    fn run(delay: usize) -> (Phase2, Phase2, usize) {
        run_between(
            Phase2::new(Role::Originate),
            Phase2::new(Role::Answer),
            delay,
        )
    }

    // `call` and `answer` in their V.34 parts, which V.90 gives the digital and the analogue modem.
    fn run_between(call: Phase2, answer: Phase2, delay: usize) -> (Phase2, Phase2, usize) {
        run_stalled(call, answer, delay, 0)
    }

    // As `run_between`, with `stall` samples more delay toward the answer from 200 ms into its L2.
    fn run_stalled(
        mut call: Phase2,
        mut answer: Phase2,
        delay: usize,
        stall: usize,
    ) -> (Phase2, Phase2, usize) {
        let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
        let mut up_line = std::collections::VecDeque::from(vec![0; delay]);
        let mut down_line = up_line.clone();
        let mut stalled = false;
        let mut frames = 0;
        while !(call.done() && answer.done()) && frames < 400 {
            call.transmit(&mut up);
            answer.transmit(&mut down);
            if !stalled && answer.tx == Tx::L2 && answer.sent >= answer.l2_from + 1600 {
                up_line.extend(std::iter::repeat_n(0, stall));
                stalled = true;
            }
            up_line.extend(up);
            down_line.extend(down);
            let heard_up: Vec<i16> = up_line.drain(..FRAME).collect();
            let heard_down: Vec<i16> = down_line.drain(..FRAME).collect();
            answer.receive(&heard_up);
            call.receive(&heard_down);
            frames += 1;
        }
        (call, answer, frames)
    }

    // As `run_between`, with the first INFO0 of the call, or of the answer, lost on the line.
    fn run_losing_info0(
        call: Phase2,
        answer: Phase2,
        delay: usize,
        from_call: bool,
    ) -> (Phase2, Phase2, usize) {
        run_losing(call, answer, delay, from_call, Step::SendInfo0)
    }

    // As `run_between`, with what the call, or the answer, sends in `step` lost on the line the first time.
    fn run_losing(
        mut call: Phase2,
        mut answer: Phase2,
        delay: usize,
        from_call: bool,
        step: Step,
    ) -> (Phase2, Phase2, usize) {
        let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
        let mut up_line = std::collections::VecDeque::from(vec![0; delay]);
        let mut down_line = up_line.clone();
        let mut frames = 0;
        let (mut losing, mut lost) = (false, false);
        while !(call.done() && answer.done()) && frames < 1000 {
            let side = if from_call { &call } else { &answer };
            let now = side.step == step && !lost;
            lost |= losing && !now;
            losing = now;
            let (lose_up, lose_down) = (from_call && losing, !from_call && losing);
            call.transmit(&mut up);
            answer.transmit(&mut down);
            if lose_up {
                up.fill(0);
            }
            if lose_down {
                down.fill(0);
            }
            up_line.extend(up);
            down_line.extend(down);
            let heard_up: Vec<i16> = up_line.drain(..FRAME).collect();
            let heard_down: Vec<i16> = down_line.drain(..FRAME).collect();
            answer.receive(&heard_up);
            call.receive(&heard_down);
            frames += 1;
        }
        (call, answer, frames)
    }

    #[test]
    fn recovers_an_info0_lost_on_the_line() {
        for from_call in [true, false] {
            for delay in [0, 400, 1200, 2400] {
                let pairs = [
                    (Phase2::new(Role::Originate), Phase2::new(Role::Answer)),
                    (Phase2::digital(), Phase2::analogue()),
                ];
                for (call, answer) in pairs {
                    let (call, answer, frames) = run_losing_info0(call, answer, delay, from_call);
                    assert!(
                        call.done() && answer.done(),
                        "losing the {} INFO0 over {delay} samples, phase 2 did not finish in {} ms: \
                         call {:?}, answer {:?}",
                        if from_call { "call's" } else { "answer's" },
                        frames * 20,
                        call.step,
                        answer.step
                    );
                    let (call, answer) = (call.round_trip, answer.round_trip);
                    assert!(
                        call.abs_diff(2 * delay) <= 8 && answer.abs_diff(2 * delay) <= 8,
                        "round trips of {call} and {answer} samples, not {}",
                        2 * delay
                    );
                }
            }
        }
    }

    #[test]
    fn retrains_when_the_calls_info1_is_lost_on_the_line() {
        for delay in [0, 1200] {
            let pairs = [
                (Phase2::new(Role::Originate), Phase2::new(Role::Answer)),
                (Phase2::digital(), Phase2::analogue()),
            ];
            for (call, answer) in pairs {
                let (call, answer, frames) = run_losing(call, answer, delay, true, Step::SendInfo1);
                assert!(
                    call.done() && answer.done(),
                    "losing the call's INFO1 over {delay} samples, phase 2 did not finish in {} ms: \
                     call {:?}, answer {:?}",
                    frames * 20,
                    call.step,
                    answer.step
                );
            }
        }
    }

    #[test]
    fn two_ends_settle_phase_2_on_a_clean_line() {
        let (call, answer, frames) = run(0);
        let (Some(call), Some(answer)) = (call.outcome(), answer.outcome()) else {
            panic!(
                "phase 2 did not finish in {} ms: call {:?}, answer {:?}",
                frames * 20,
                call.step,
                answer.step
            );
        };
        assert_eq!(call.transmit.symbol_rate, answer.receive.symbol_rate);
        assert_eq!(call.receive.symbol_rate, answer.transmit.symbol_rate);
        assert_eq!(call.transmit.high_carrier, answer.receive.high_carrier);
        assert_eq!(call.receive.high_carrier, answer.transmit.high_carrier);
        assert_eq!(call.receive.symbol_rate, SymbolRate::S3429);
        assert_eq!(call.receive.max_rate, 14);
        assert!(frames < 150, "phase 2 took {} ms", frames * 20);
    }

    #[test]
    fn a_digital_and_an_analogue_modem_settle_phase_2_of_v90() {
        for delay in [0, 400] {
            let (digital, analogue, frames) =
                run_between(Phase2::digital(), Phase2::analogue(), delay);
            let (Some(digital), Some(analogue)) = (digital.pcm_outcome(), analogue.pcm_outcome())
            else {
                panic!(
                    "V.90 phase 2 did not finish in {} ms: digital {:?}, analogue {:?}",
                    frames * 20,
                    digital.step,
                    analogue.step
                );
            };
            assert_eq!(digital.upstream.symbol_rate, analogue.upstream.symbol_rate);
            assert_eq!(
                digital.upstream.high_carrier,
                analogue.upstream.high_carrier
            );
            assert_eq!(analogue.upstream.symbol_rate, SymbolRate::S3429);
            assert_eq!((digital.uinfo, analogue.uinfo), (75, 75));
            assert_eq!(digital.digital, analogue.digital);
            assert!(digital.round_trip.abs_diff(2 * delay) <= 8);
        }
    }

    #[test]
    fn the_answer_waits_past_l2_for_a_late_tone_b() {
        let pairs = [
            (Phase2::new(Role::Originate), Phase2::new(Role::Answer)),
            (Phase2::digital(), Phase2::analogue()),
        ];
        for (call, answer) in pairs {
            let (call, answer, frames) = run_stalled(call, answer, 0, 1600);
            assert!(
                call.done() && answer.done(),
                "phase 2 did not finish in {} ms: call {:?}, answer {:?}",
                frames * 20,
                call.step,
                answer.step
            );
        }
    }

    #[test]
    fn a_digital_and_an_analogue_modem_retrain_phase_2_of_v90() {
        let (digital, analogue, _) = run_between(Phase2::digital(), Phase2::analogue(), 0);
        let (digital, analogue) = (
            digital.pcm_outcome().unwrap(),
            analogue.pcm_outcome().unwrap(),
        );
        let (again_digital, again_analogue, frames) = run_between(
            Phase2::retrain_digital(digital.far),
            Phase2::retrain_analogue(analogue.digital),
            0,
        );
        let (Some(again_digital), Some(again_analogue)) =
            (again_digital.pcm_outcome(), again_analogue.pcm_outcome())
        else {
            panic!(
                "a V.90 retrain did not finish phase 2 in {} ms: digital {:?}, analogue {:?}",
                frames * 20,
                again_digital.step,
                again_analogue.step
            );
        };
        assert_eq!(again_digital.upstream, digital.upstream);
        assert_eq!(again_analogue.upstream, analogue.upstream);
        assert_eq!((again_digital.uinfo, again_analogue.uinfo), (75, 75));
        assert_eq!(again_analogue.digital, analogue.digital);
    }

    #[test]
    fn measures_the_round_trip() {
        for delay in [0, 400] {
            let (call, answer, _) = run(delay);
            let (call, answer) = (call.outcome().unwrap(), answer.outcome().unwrap());
            for measured in [call.round_trip, answer.round_trip] {
                assert!(
                    measured.abs_diff(2 * delay) <= 8,
                    "the round trip delay would be {measured} samples, not {}",
                    2 * delay
                );
            }
        }
    }
}
