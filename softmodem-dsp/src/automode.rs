//! Automode: V.8 to agree on a modulation, and Annex A of V.32 bis with one
//! step of this modem's own for a far end that does not speak V.8.

use crate::ansam::{AnswerTone, AnswerToneDetector, AnswerToneKind};
use crate::fsk::{self, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE};
use crate::pump::{DataPump, Modulation, Role};
use crate::tone::ToneDetector;
use crate::uart::Decoder;
use crate::v8::{self, Heard, Menu, Modes};
use crate::v21::V21;

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
    match role {
        Role::Answer => Box::new(Answer::new(top)),
        Role::Originate => Box::new(Call::new(top)),
    }
}

fn offered(top: Modulation) -> Modes {
    Modes {
        v22bis: top != Modulation::V21,
        v21: true,
    }
}

fn chosen(top: Modulation, common: Modes, role: Role) -> Box<dyn DataPump> {
    if common.v22bis {
        top.pump(role)
    } else {
        Modulation::V21.pump(role)
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
    Ansam {
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

// Answers with ANSam and V.8, or with USB1 then V.21 mark for a far end without V.8.
#[derive(Debug)]
struct Answer {
    top: Modulation,
    stage: AnswerStage,
    tone: AnswerTone,
    link: MenuLink,
    sent: usize,
}

impl Answer {
    fn new(top: Modulation) -> Self {
        Self {
            top,
            stage: AnswerStage::Silence,
            tone: AnswerTone::new(AnswerToneKind::Ansam, true, ANSWER_TONE_DBM0),
            link: MenuLink::new(Role::Answer),
            sent: 0,
        }
    }

    fn advance(&mut self) {
        let sent = self.sent;
        let stage = std::mem::replace(&mut self.stage, AnswerStage::Silence);
        self.stage = match stage {
            AnswerStage::Silence if sent >= SILENCE_SAMPLES => AnswerStage::Ansam {
                until: sent + ANSAM_SAMPLES,
            },
            AnswerStage::Ansam { until } if sent >= until => AnswerStage::Gap {
                until: sent + GAP_SAMPLES,
                next: None,
            },
            AnswerStage::Gap { until, next } if sent >= until => match next {
                Some(pump) => AnswerStage::Chosen(pump),
                None if self.top == Modulation::V21 => {
                    AnswerStage::Chosen(Modulation::V21.pump(Role::Answer))
                }
                None => AnswerStage::Trying {
                    pump: self.top.pump(Role::Answer),
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
            AnswerStage::Ansam { .. } => self.tone.render(out),
            AnswerStage::Jm { menu } => self.link.repeat(out, *menu),
            AnswerStage::Trying { pump, .. } | AnswerStage::Chosen(pump) => pump.transmit(out),
        }
        self.sent += out.len();
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        match &mut self.stage {
            AnswerStage::Silence | AnswerStage::Ansam { .. } => {
                if let Some(cm) = self.link.menu(input) {
                    let modes = offered(self.top).common(cm.modes);
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
                        until: self.sent + GAP_SAMPLES,
                        next: Some(chosen(self.top, modes, Role::Answer)),
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
    fn new(top: Modulation) -> Self {
        Self {
            candidate: (top != Modulation::V21).then(|| top.pump(Role::Originate)),
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

// Calls with V.8 after ANSam, or follows USB1 or V.21 mark after plain ANS.
#[derive(Debug)]
struct Call {
    top: Modulation,
    stage: CallStage,
    detector: AnswerToneDetector,
    sig_a: SigA,
    link: MenuLink,
    sent: usize,
}

impl Call {
    fn new(top: Modulation) -> Self {
        Self {
            top,
            stage: CallStage::Listening { heard_ans: false },
            detector: AnswerToneDetector::new(),
            sig_a: SigA::new(top),
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
        Menu::data(offered(self.top))
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
            CallStage::Listening { .. } | CallStage::Te { .. } | CallStage::Gap { .. } => {
                out.fill(0);
            }
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
                        until: sent + out.len() + GAP_SAMPLES,
                        next: chosen(self.top, modes, Role::Originate),
                    };
                }
            }
            CallStage::Legacy => out.fill(0),
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
            CallStage::Listening { heard_ans } => match self.detector.process(input) {
                Some(AnswerToneKind::Ansam) => {
                    self.stage = CallStage::Te {
                        until: self.sent + TE_SAMPLES,
                    };
                }
                Some(AnswerToneKind::Ans) => *heard_ans = true,
                None if *heard_ans => self.stage = CallStage::Legacy,
                None => {}
            },
            CallStage::Cm { joint } => {
                if joint.is_none()
                    && let Some(jm) = self.link.menu(input)
                {
                    *joint = Some(offered(self.top).common(jm.modes));
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
    use super::*;

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
        assert!(frames * 20 < 7000, "V.8 took {} ms", frames * 20);
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
        let menu = Menu::data(offered(Modulation::V22bis));
        assert_eq!(repeats.push(menu), None);
        assert_eq!(repeats.push(menu), Some(menu));
    }
}
