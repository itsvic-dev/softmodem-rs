//! Automode: V.8 bis, then V.8 to agree on a modulation, and Annex A of
//! V.32 bis with one step of this modem's own for a far end that does not
//! speak V.8.

use crate::ansam::{AnswerTone, AnswerToneDetector, AnswerToneKind};
use crate::fsk::{self, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE};
use crate::pump::{DataPump, Modulation, Role};
use crate::tone::ToneDetector;
use crate::uart::Decoder;
use crate::v8::{self, Heard, Menu, Modes};
use crate::v8bis::{Answering, Responding, Startup};
use crate::v21::V21;

// V.8 bis § 10.2.2.
const V8BIS_SILENCE_SAMPLES: usize = 3_200;
// V.25 § 4.3, and V.8 §§ 8.1.1, 8.1.2, 8.2.2.
const SILENCE_SAMPLES: usize = 16_000;
const ANSAM_SAMPLES: usize = 40_000;
const TE_SAMPLES: usize = 8_000;
const GAP_SAMPLES: usize = 600;
// Annex A.2.2 of V.32 bis.
const TA_SAMPLES: usize = 24_000;
const MARK_SAMPLES: usize = 1_600;
const SIGC_BITS: usize = 150;
const ANSWER_TONE_DBM0: f64 = -13.0;
const V21_MARK_HZ: f64 = 1650.0;

/// A pump that finds, with the far end, the best modulation both have, up to
/// `top`, and then carries the call on it.
#[must_use]
pub fn pump(top: Modulation, role: Role) -> Box<dyn DataPump> {
    started(
        Offered {
            top,
            fallback: true,
        },
        role,
    )
}

/// V.8 that offers `top` alone, for a modulation such as V.34 whose phase 1
/// is V.8.
#[must_use]
pub fn only(top: Modulation, role: Role) -> Box<dyn DataPump> {
    started(
        Offered {
            top,
            fallback: false,
        },
        role,
    )
}

fn started(offer: Offered, role: Role) -> Box<dyn DataPump> {
    match role {
        Role::Answer => Box::new(Answer::new(offer)),
        Role::Originate => Box::new(Call::new(offer)),
    }
}

// V.34 sends the 75 ms after CJ itself, as it listens for INFO0 during them.
fn gap(modes: Modes) -> usize {
    if modes.v34 { 0 } else { GAP_SAMPLES }
}

/// The top modulation, and whether the ones below it may be offered too.
#[derive(Debug, Clone, Copy)]
struct Offered {
    top: Modulation,
    fallback: bool,
}

impl Offered {
    fn modes(self) -> Modes {
        let top = self.top;
        let family = matches!(top, Modulation::V22 | Modulation::V22bis);
        if self.fallback {
            Modes {
                v34: top == Modulation::V34,
                v22bis: top != Modulation::V21,
                v21: true,
            }
        } else {
            Modes {
                v34: top == Modulation::V34,
                v22bis: family,
                v21: top == Modulation::V21,
            }
        }
    }

    // The V.22 family's own start-up, for a far end without V.8.
    fn legacy(self) -> Modulation {
        if self.top == Modulation::V34 {
            Modulation::V22bis
        } else {
            self.top
        }
    }

    fn chosen(self, common: Modes, role: Role) -> Box<dyn DataPump> {
        if common.v34 {
            Modulation::V34.pump(role)
        } else if common.v22bis {
            self.legacy().pump(role)
        } else {
            Modulation::V21.pump(role)
        }
    }
}

#[derive(Debug, Default)]
struct Repeats {
    last: Option<Menu>,
    count: u32,
}

impl Repeats {
    // V.8 §§ 7.4 and 8.1.2: a menu counts once it has come twice in a row.
    fn push(&mut self, menu: Menu) -> Option<Menu> {
        if self.last == Some(menu) {
            self.count += 1;
        } else {
            self.last = Some(menu);
            self.count = 1;
        }
        (self.count >= 2).then_some(menu)
    }
}

#[derive(Debug)]
struct MenuLink {
    modulator: fsk::Modulator,
    demodulator: fsk::Demodulator,
    reader: v8::Reader,
    repeats: Repeats,
    bits_without_menu: usize,
}

impl MenuLink {
    fn new(role: Role) -> Self {
        let (transmit, receive) = match role {
            Role::Originate => (V21_ORIGINATE, V21_ANSWER),
            Role::Answer => (V21_ANSWER, V21_ORIGINATE),
        };
        Self {
            modulator: fsk::Modulator::new(transmit, V21_MAX_LEVEL_DBM0),
            demodulator: fsk::Demodulator::new(receive),
            reader: v8::Reader::new(),
            repeats: Repeats::default(),
            bits_without_menu: 0,
        }
    }

    // A V.21 caller's mark or data rather than CM, which repeats every 60 bits.
    fn plain_v21(&self) -> bool {
        self.bits_without_menu >= SIGC_BITS
    }

    fn take_v21(&mut self, role: Role) -> Box<dyn DataPump> {
        let receive = match role {
            Role::Originate => V21_ANSWER,
            Role::Answer => V21_ORIGINATE,
        };
        let demodulator = std::mem::replace(&mut self.demodulator, fsk::Demodulator::new(receive));
        Box::new(V21::resuming(role, demodulator))
    }

    fn repeat(&mut self, out: &mut [i16], menu: Menu) {
        if self.modulator.pending() == 0 {
            self.modulator.push_bits(menu.sequence());
        }
        self.modulator.render(out);
    }

    fn hear(&mut self, input: &[i16]) -> Vec<Heard> {
        let mut bits = Vec::new();
        self.demodulator.process(input, &mut bits);
        self.bits_without_menu += bits.len();
        let heard: Vec<Heard> = bits
            .into_iter()
            .filter_map(|b| self.reader.push(b))
            .collect();
        if heard.iter().any(|h| matches!(h, Heard::Menu(_))) {
            self.bits_without_menu = 0;
        }
        heard
    }

    fn menu(&mut self, input: &[i16]) -> Option<Menu> {
        let heard = self.hear(input);
        heard.into_iter().find_map(|h| match h {
            Heard::Menu(menu) => self.repeats.push(menu),
            Heard::Cj => None,
        })
    }
}

#[derive(Debug)]
enum AnswerStage {
    Silence,
    V8bis(Box<Answering>),
    Tone {
        until: usize,
    },
    Jm {
        menu: Menu,
    },
    Gap {
        until: usize,
        next: Option<Box<dyn DataPump>>,
    },
    Trying {
        pump: Box<dyn DataPump>,
        until: usize,
    },
    Chosen(Box<dyn DataPump>),
}

// Answers with CRe, then ANSam and V.8, or with USB1 then V.21 mark for a far end without V.8.
#[derive(Debug)]
struct Answer {
    offer: Offered,
    stage: AnswerStage,
    tone: AnswerTone,
    link: MenuLink,
    sent: usize,
}

impl Answer {
    fn new(offer: Offered) -> Self {
        Self {
            offer,
            stage: AnswerStage::Silence,
            tone: AnswerTone::new(AnswerToneKind::Ansam, true, ANSWER_TONE_DBM0),
            link: MenuLink::new(Role::Answer),
            sent: 0,
        }
    }

    fn advance(&mut self) {
        let sent = self.sent;
        let stage = std::mem::replace(&mut self.stage, AnswerStage::Silence);
        let tone = AnswerStage::Tone {
            until: sent + ANSAM_SAMPLES,
        };
        self.stage = match stage {
            AnswerStage::Silence if sent >= V8BIS_SILENCE_SAMPLES => {
                AnswerStage::V8bis(Box::new(Answering::new(self.offer.modes())))
            }
            AnswerStage::V8bis(answering) => match answering.startup() {
                Some(Startup::V8) => tone,
                Some(Startup::V25) => {
                    self.tone = AnswerTone::new(AnswerToneKind::Ans, true, ANSWER_TONE_DBM0);
                    tone
                }
                Some(Startup::Caller) => AnswerStage::Chosen(Box::new(Call::new(self.offer))),
                None if !answering.engaged() && sent >= SILENCE_SAMPLES => tone,
                None => AnswerStage::V8bis(answering),
            },
            AnswerStage::Tone { until } if sent >= until => AnswerStage::Gap {
                until: sent + GAP_SAMPLES,
                next: None,
            },
            AnswerStage::Gap { until, next } if sent >= until => match next {
                Some(pump) => AnswerStage::Chosen(pump),
                None if self.offer.top == Modulation::V21 => {
                    AnswerStage::Chosen(Modulation::V21.pump(Role::Answer))
                }
                None => AnswerStage::Trying {
                    pump: self.offer.legacy().pump(Role::Answer),
                    until: sent + TA_SAMPLES,
                },
            },
            AnswerStage::Trying { pump, until } if sent >= until && !pump.engaged() => {
                AnswerStage::Chosen(Modulation::V21.pump(Role::Answer))
            }
            stage => stage,
        };
    }

    fn pump(&self) -> Option<&dyn DataPump> {
        match &self.stage {
            AnswerStage::Trying { pump, .. } | AnswerStage::Chosen(pump) => Some(pump.as_ref()),
            _ => None,
        }
    }
}

impl DataPump for Answer {
    fn sends_own_answer_tone(&self) -> bool {
        true
    }

    fn bit_rate(&self) -> u32 {
        self.pump().map_or(0, DataPump::bit_rate)
    }

    fn decoder(&self) -> Decoder {
        self.pump().map_or_else(Decoder::new, DataPump::decoder)
    }

    fn push_bits(&mut self, bits: &[bool]) {
        if let AnswerStage::Trying { pump, .. } | AnswerStage::Chosen(pump) = &mut self.stage {
            pump.push_bits(bits);
        }
    }

    fn retrain(&mut self) {
        if let AnswerStage::Chosen(pump) = &mut self.stage {
            pump.retrain();
        }
    }

    fn pending(&self) -> usize {
        self.pump().map_or(0, DataPump::pending)
    }

    fn transmit(&mut self, out: &mut [i16]) {
        self.advance();
        match &mut self.stage {
            AnswerStage::Silence | AnswerStage::Gap { .. } => out.fill(0),
            AnswerStage::V8bis(answering) => answering.transmit(out),
            AnswerStage::Tone { .. } => self.tone.render(out),
            AnswerStage::Jm { menu } => self.link.repeat(out, *menu),
            AnswerStage::Trying { pump, .. } | AnswerStage::Chosen(pump) => pump.transmit(out),
        }
        self.sent += out.len();
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        match &mut self.stage {
            AnswerStage::V8bis(answering) => answering.receive(input),
            AnswerStage::Silence | AnswerStage::Tone { .. } => {
                if let Some(cm) = self.link.menu(input) {
                    let modes = self.offer.modes().common(cm.modes);
                    self.stage = AnswerStage::Jm {
                        menu: Menu {
                            data: cm.data,
                            modes,
                        },
                    };
                } else if self.link.plain_v21() {
                    // V.8 § 8.2.2: a V.21 caller's sigC during ANSam.
                    self.stage = AnswerStage::Gap {
                        until: self.sent + GAP_SAMPLES,
                        next: Some(self.link.take_v21(Role::Answer)),
                    };
                }
            }
            AnswerStage::Jm { menu } => {
                let modes = menu.modes;
                // V.8 § 8.2.3 allows the end of CM in place of a lost CJ.
                let cj = self.link.hear(input).contains(&Heard::Cj);
                if cj || !self.link.demodulator.carrier() {
                    self.stage = AnswerStage::Gap {
                        until: self.sent + gap(modes),
                        next: Some(self.offer.chosen(modes, Role::Answer)),
                    };
                }
            }
            AnswerStage::Gap { .. } => {}
            AnswerStage::Trying { pump, .. } => {
                pump.receive(input, bits);
                self.link.hear(input);
                if pump.engaged() {
                    let AnswerStage::Trying { pump, .. } =
                        std::mem::replace(&mut self.stage, AnswerStage::Silence)
                    else {
                        unreachable!("matched above");
                    };
                    self.stage = AnswerStage::Chosen(pump);
                } else if self.link.plain_v21() {
                    self.stage = AnswerStage::Chosen(self.link.take_v21(Role::Answer));
                }
            }
            AnswerStage::Chosen(pump) => pump.receive(input, bits),
        }
    }

    fn carrier(&self) -> bool {
        self.pump().is_some_and(DataPump::carrier)
    }

    fn connected(&self) -> bool {
        self.pump().is_some_and(DataPump::connected)
    }
}

#[derive(Debug)]
enum CallStage {
    Listening {
        heard_ans: bool,
    },
    Te {
        until: usize,
    },
    Cm {
        joint: Option<Modes>,
    },
    Cj {
        modes: Modes,
    },
    Gap {
        until: usize,
        next: Box<dyn DataPump>,
    },
    Legacy,
    Chosen(Box<dyn DataPump>),
}

// V.8 § 8.1.1: the caller goes on as soon as it hears a modulation's own sigA.
#[derive(Debug)]
struct SigA {
    candidate: Option<Box<dyn DataPump>>,
    mark: ToneDetector,
    marked: usize,
}

impl SigA {
    fn new(offer: Offered) -> Self {
        Self {
            candidate: (offer.top != Modulation::V21).then(|| offer.legacy().pump(Role::Originate)),
            mark: ToneDetector::new(V21_MARK_HZ),
            marked: 0,
        }
    }

    fn tick(&mut self, samples: usize) {
        if let Some(pump) = &mut self.candidate {
            pump.transmit(&mut vec![0; samples]);
        }
    }

    fn hear(&mut self, input: &[i16]) -> Option<Box<dyn DataPump>> {
        if let Some(pump) = &mut self.candidate {
            pump.receive(input, &mut Vec::new());
        }
        self.marked = if self.mark.process(input) {
            self.marked + input.len()
        } else {
            0
        };
        if self.candidate.as_ref().is_some_and(|p| p.engaged()) {
            self.candidate.take()
        } else if self.marked >= MARK_SAMPLES {
            Some(Modulation::V21.pump(Role::Originate))
        } else {
            None
        }
    }
}

// Answers CRe, calls with V.8 after ANSam, or follows USB1 or V.21 mark after plain ANS.
#[derive(Debug)]
struct Call {
    offer: Offered,
    stage: CallStage,
    responder: Responding,
    detector: AnswerToneDetector,
    sig_a: SigA,
    link: MenuLink,
    sent: usize,
}

impl Call {
    fn new(offer: Offered) -> Self {
        Self {
            offer,
            stage: CallStage::Listening { heard_ans: false },
            responder: Responding::new(offer.modes()),
            detector: AnswerToneDetector::new(),
            sig_a: SigA::new(offer),
            link: MenuLink::new(Role::Originate),
            sent: 0,
        }
    }

    fn pump(&self) -> Option<&dyn DataPump> {
        match &self.stage {
            CallStage::Chosen(pump) => Some(pump.as_ref()),
            _ => None,
        }
    }

    fn cm(&self) -> Menu {
        Menu::data(self.offer.modes())
    }
}

impl DataPump for Call {
    fn sends_own_answer_tone(&self) -> bool {
        true
    }

    fn bit_rate(&self) -> u32 {
        self.pump().map_or(0, DataPump::bit_rate)
    }

    fn decoder(&self) -> Decoder {
        self.pump().map_or_else(Decoder::new, DataPump::decoder)
    }

    fn push_bits(&mut self, bits: &[bool]) {
        if let CallStage::Chosen(pump) = &mut self.stage {
            pump.push_bits(bits);
        }
    }

    fn retrain(&mut self) {
        if let CallStage::Chosen(pump) = &mut self.stage {
            pump.retrain();
        }
    }

    fn pending(&self) -> usize {
        self.pump().map_or(0, DataPump::pending)
    }

    fn transmit(&mut self, out: &mut [i16]) {
        let sent = self.sent;
        let cm = self.cm();
        match &mut self.stage {
            CallStage::Te { until } if sent >= *until => {
                self.stage = CallStage::Cm { joint: None };
            }
            CallStage::Gap { until, .. } if sent >= *until => {
                let CallStage::Gap { next, .. } =
                    std::mem::replace(&mut self.stage, CallStage::Listening { heard_ans: false })
                else {
                    unreachable!("matched above");
                };
                self.stage = CallStage::Chosen(next);
            }
            _ => {}
        }
        match &mut self.stage {
            CallStage::Listening { .. } => self.responder.transmit(out),
            CallStage::Te { .. } | CallStage::Gap { .. } | CallStage::Legacy => out.fill(0),
            CallStage::Cm { joint } => {
                if self.link.modulator.pending() == 0
                    && let Some(modes) = *joint
                {
                    self.link.modulator.push_bits(v8::cj());
                    self.link.modulator.render(out);
                    self.stage = CallStage::Cj { modes };
                } else {
                    self.link.repeat(out, cm);
                }
            }
            CallStage::Cj { modes } => {
                let modes = *modes;
                self.link.modulator.render(out);
                if self.link.modulator.pending() == 0 {
                    self.stage = CallStage::Gap {
                        until: sent + out.len() + gap(modes),
                        next: self.offer.chosen(modes, Role::Originate),
                    };
                }
            }
            CallStage::Chosen(pump) => pump.transmit(out),
        }
        if matches!(
            self.stage,
            CallStage::Te { .. } | CallStage::Cm { .. } | CallStage::Legacy
        ) {
            self.sig_a.tick(out.len());
        }
        self.sent += out.len();
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        let waiting = matches!(
            self.stage,
            CallStage::Te { .. } | CallStage::Cm { joint: None } | CallStage::Legacy
        );
        if waiting && let Some(pump) = self.sig_a.hear(input) {
            self.stage = CallStage::Chosen(pump);
            return;
        }
        match &mut self.stage {
            CallStage::Listening { heard_ans } => {
                self.responder.receive(input);
                match self.detector.process(input) {
                    Some(AnswerToneKind::Ansam) => {
                        self.stage = CallStage::Te {
                            until: self.sent + TE_SAMPLES,
                        };
                    }
                    Some(AnswerToneKind::Ans) => *heard_ans = true,
                    None if *heard_ans => self.stage = CallStage::Legacy,
                    None => {}
                }
            }
            CallStage::Cm { joint } => {
                if joint.is_none()
                    && let Some(jm) = self.link.menu(input)
                {
                    *joint = Some(self.offer.modes().common(jm.modes));
                }
            }
            CallStage::Chosen(pump) => pump.receive(input, bits),
            CallStage::Te { .. }
            | CallStage::Cj { .. }
            | CallStage::Gap { .. }
            | CallStage::Legacy => {}
        }
    }

    fn carrier(&self) -> bool {
        self.pump().is_some_and(DataPump::carrier)
    }

    fn connected(&self) -> bool {
        self.pump().is_some_and(DataPump::connected)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;
    use crate::v8bis::{Pair, Signal, SignalDetector};

    const FRAME: usize = 160;

    fn connect(caller: &mut dyn DataPump, answerer: &mut dyn DataPump) -> usize {
        let mut up = [0; FRAME];
        let mut down = [0; FRAME];
        let mut frames = 0;
        while !(caller.connected() && answerer.connected()) {
            caller.transmit(&mut up);
            answerer.transmit(&mut down);
            answerer.receive(&up, &mut Vec::new());
            caller.receive(&down, &mut Vec::new());
            frames += 1;
            assert!(frames < 1500, "no connection after {} ms", frames * 20);
        }
        frames
    }

    #[test]
    fn two_automode_ends_agree_on_v22bis_through_v8() {
        let mut caller = pump(Modulation::V22bis, Role::Originate);
        let mut answerer = pump(Modulation::V22bis, Role::Answer);
        let frames = connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
        assert!(
            frames * 20 < 8000,
            "V.8 bis and V.8 took {} ms",
            frames * 20
        );
    }

    #[test]
    fn two_v34_ends_agree_on_v34_through_v8() {
        let mut caller = pump(Modulation::V34, Role::Originate);
        let mut answerer = pump(Modulation::V34, Role::Answer);
        let frames = connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (33_600, 33_600));
        assert!(frames * 20 < 15_000, "V.8 and V.34 took {} ms", frames * 20);
    }

    #[test]
    fn two_v34_ends_connect_whatever_the_delay_each_way() {
        let mut failed = Vec::new();
        for up_delay in [0, 1, 160, 320, 555] {
            for down_delay in [0, 1, 160, 320, 555] {
                let mut caller = pump(Modulation::V34, Role::Originate);
                let mut answerer = pump(Modulation::V34, Role::Answer);
                let mut lines = [
                    VecDeque::from(vec![0; up_delay]),
                    VecDeque::from(vec![0; down_delay]),
                ];
                let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
                let mut frames = 0;
                while !(caller.connected() && answerer.connected()) && frames < 1000 {
                    caller.transmit(&mut up);
                    answerer.transmit(&mut down);
                    lines[0].extend(up);
                    lines[1].extend(down);
                    let heard: Vec<i16> = lines[0].drain(..FRAME).collect();
                    answerer.receive(&heard, &mut Vec::new());
                    let heard: Vec<i16> = lines[1].drain(..FRAME).collect();
                    caller.receive(&heard, &mut Vec::new());
                    frames += 1;
                }
                if !(caller.connected() && answerer.connected() && caller.bit_rate() == 33_600) {
                    failed.push((up_delay, down_delay));
                }
            }
        }
        assert!(
            failed.is_empty(),
            "no V.34 connection over lines of these delays up and down, in samples: {failed:?}"
        );
    }

    #[test]
    fn two_v34_ends_connect_when_each_sends_and_hears_in_either_order() {
        let mut failed = Vec::new();
        for seed in 1..=40u32 {
            let mut state = seed.wrapping_mul(2_654_435_761);
            let mut coin = move || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state & 1 == 1
            };
            let mut ends = [
                pump(Modulation::V34, Role::Originate),
                pump(Modulation::V34, Role::Answer),
            ];
            let mut queues = [VecDeque::new(), VecDeque::new()];
            let mut frames = 0;
            while !ends.iter().all(|end| end.connected()) && frames < 1000 {
                for end in 0..2 {
                    let first = coin();
                    for transmit in [first, !first] {
                        if transmit {
                            let mut frame = [0; FRAME];
                            ends[end].transmit(&mut frame);
                            queues[end].push_back(frame);
                        } else if let Some(heard) = queues[1 - end].pop_front() {
                            ends[end].receive(&heard, &mut Vec::new());
                        }
                    }
                }
                frames += 1;
            }
            if !(ends.iter().all(|end| end.connected()) && ends[0].bit_rate() == 33_600) {
                failed.push(seed);
            }
        }
        assert!(
            failed.is_empty(),
            "no V.34 connection when each end sends and hears in either order, for seeds {failed:?}"
        );
    }

    #[test]
    fn a_v34_caller_meets_a_v22bis_answerer_at_2400() {
        let mut caller = pump(Modulation::V34, Role::Originate);
        let mut answerer = pump(Modulation::V22bis, Role::Answer);
        connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));
    }

    #[test]
    fn a_fixed_v34_still_runs_v8_first() {
        let mut caller = only(Modulation::V34, Role::Originate);
        let mut answerer = only(Modulation::V34, Role::Answer);
        connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (33_600, 33_600));
    }

    #[test]
    fn a_caller_that_ignores_cre_hears_ansam_at_2_s() {
        let mut answerer = pump(Modulation::V22bis, Role::Answer);
        let mut line = vec![0; 20_000];
        for chunk in line.chunks_mut(FRAME) {
            answerer.transmit(chunk);
            answerer.receive(&[0; FRAME], &mut Vec::new());
        }
        assert!(line[..V8BIS_SILENCE_SAMPLES].iter().all(|&s| s == 0));
        let mut cre = SignalDetector::new(Pair::Initiating);
        assert_eq!(cre.process(&line[..8000]), Some(Signal::CRe));
        assert!(line[8000..SILENCE_SAMPLES].iter().all(|&s| s == 0));
        let mut ansam = AnswerToneDetector::new();
        assert_eq!(
            ansam.process(&line[SILENCE_SAMPLES..]),
            Some(AnswerToneKind::Ansam)
        );
    }

    #[test]
    fn a_v21_only_end_brings_v8_down_to_v21() {
        let mut caller = pump(Modulation::V22bis, Role::Originate);
        let mut answerer = pump(Modulation::V21, Role::Answer);
        connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (300, 300));
    }

    #[test]
    fn v22_top_offers_v22_and_meets_v22bis_at_1200() {
        let mut caller = pump(Modulation::V22, Role::Originate);
        let mut answerer = pump(Modulation::V22bis, Role::Answer);
        connect(caller.as_mut(), answerer.as_mut());
        assert_eq!((caller.bit_rate(), answerer.bit_rate()), (1200, 1200));
    }

    #[test]
    fn a_menu_counts_on_its_second_arrival() {
        let mut repeats = Repeats::default();
        let menu = Menu::data(
            Offered {
                top: Modulation::V22bis,
                fallback: true,
            }
            .modes(),
        );
        assert_eq!(repeats.push(menu), None);
        assert_eq!(repeats.push(menu), Some(menu));
    }
}
