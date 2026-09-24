//! V.8 bis: the tone signals that start a transaction, and the messages
//! that carry it over V.21.

use std::collections::VecDeque;

use crate::correlator::Correlator;
use crate::fsk::{Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE};
use crate::hdlc::{self, Deframer};
use crate::sine_peak;
use crate::tone::Tone;
use crate::v8::{Modes, Pcm};

// § 7.1.1, tables 1 and 2.
const INITIATING_HZ: [f64; 2] = [1375.0, 2002.0];
const RESPONDING_HZ: [f64; 2] = [1529.0, 2225.0];
// § 7.1.2: segment 1 is 400 ms, or 285 ms for MRe and CRe, and segment 2 is 100 ms.
const SEGMENT_1_SAMPLES: usize = 3200;
const SHORT_SEGMENT_1_SAMPLES: usize = 2280;
const SEGMENT_2_SAMPLES: usize = 800;

const WINDOW: usize = 160;
const PURITY: f64 = 0.7;
const DETECT_DBM0: f64 = -43.0;
// Most of a short segment 1, and half of segment 2.
const PAIR_SAMPLES: usize = 1600;
const SINGLE_SAMPLES: usize = 400;
const SEGMENT_2_WAIT_SAMPLES: usize = 1200;

/// A V.8 bis signal, named by its segment 2 tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    MRe,
    MRd,
    CRe,
    CRd,
    ESi,
    ESr,
}

/// The dual tone of segment 1: the one a transaction starts with, or the
/// one that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pair {
    Initiating,
    Responding,
}

impl Pair {
    fn hz(self) -> [f64; 2] {
        match self {
            Self::Initiating => INITIATING_HZ,
            Self::Responding => RESPONDING_HZ,
        }
    }

    fn signals(self) -> &'static [Signal] {
        match self {
            Self::Initiating => &[
                Signal::MRe,
                Signal::MRd,
                Signal::CRe,
                Signal::CRd,
                Signal::ESi,
            ],
            Self::Responding => &[Signal::MRd, Signal::CRd, Signal::ESr],
        }
    }
}

impl Signal {
    fn segment_2_hz(self) -> f64 {
        match self {
            Self::MRe => 650.0,
            Self::MRd => 1150.0,
            Self::CRe => 400.0,
            Self::CRd => 1900.0,
            Self::ESi => 980.0,
            Self::ESr => 1650.0,
        }
    }

    // § 7.1.2 allows the shorter segment 1 so a modem without V.8 bis ignores it.
    fn segment_1_samples(self) -> usize {
        match self {
            Self::MRe | Self::CRe => SHORT_SEGMENT_1_SAMPLES,
            _ => SEGMENT_1_SAMPLES,
        }
    }
}

/// Sends one signal, then silence.
#[derive(Debug)]
pub struct SignalSender {
    pair: [Tone; 2],
    single: Tone,
    segment_1: usize,
    sent: usize,
}

impl SignalSender {
    /// A sender of `signal` after the dual tone of `pair`, at a total power
    /// of `level_dbm0`.
    #[must_use]
    pub fn new(signal: Signal, pair: Pair, level_dbm0: f64) -> Self {
        let [low, high] = pair.hz();
        let half = level_dbm0 - 10.0 * 2f64.log10();
        Self {
            pair: [Tone::new(low, half), Tone::new(high, half)],
            single: Tone::new(signal.segment_2_hz(), level_dbm0),
            segment_1: signal.segment_1_samples(),
            sent: 0,
        }
    }

    #[must_use]
    pub fn done(&self) -> bool {
        self.sent >= self.segment_1 + SEGMENT_2_SAMPLES
    }

    pub fn render(&mut self, out: &mut [i16]) {
        let mut high = vec![0; out.len()];
        let mut single = vec![0; out.len()];
        self.pair[0].render(out);
        self.pair[1].render(&mut high);
        self.single.render(&mut single);
        for (i, sample) in out.iter_mut().enumerate() {
            let at = self.sent + i;
            *sample = if at < self.segment_1 {
                sample.saturating_add(high[i])
            } else if at < self.segment_1 + SEGMENT_2_SAMPLES {
                single[i]
            } else {
                0
            };
        }
        self.sent += out.len();
    }
}

#[derive(Debug)]
struct Power {
    squares: VecDeque<f64>,
    sum: f64,
}

impl Power {
    fn new() -> Self {
        Self {
            squares: VecDeque::from(vec![0.0; WINDOW]),
            sum: 0.0,
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    fn push(&mut self, x: f64) -> f64 {
        let square = x * x;
        self.sum += square - self.squares.pop_front().unwrap_or_default();
        self.squares.push_back(square);
        self.sum.max(0.0) / WINDOW as f64
    }
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    Pair {
        lasted: usize,
    },
    Single {
        waited: usize,
        lasted: usize,
        at: usize,
    },
}

/// Hears the signals that start with one dual tone and tells them apart by
/// their segment 2 tone.
#[derive(Debug)]
pub struct SignalDetector {
    signals: &'static [Signal],
    pair: [Correlator; 2],
    singles: Vec<Correlator>,
    power: Power,
    threshold: f64,
    stage: Stage,
}

impl SignalDetector {
    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    #[must_use]
    pub fn new(pair: Pair) -> Self {
        let [low, high] = pair.hz();
        let signals = pair.signals();
        Self {
            signals,
            pair: [
                Correlator::new(low, WINDOW as f64),
                Correlator::new(high, WINDOW as f64),
            ],
            singles: signals
                .iter()
                .map(|s| Correlator::new(s.segment_2_hz(), WINDOW as f64))
                .collect(),
            power: Power::new(),
            threshold: sine_peak(DETECT_DBM0 - 10.0 * 2f64.log10()),
            stage: Stage::Pair { lasted: 0 },
        }
    }

    /// The signal whose segment 2 tone stopped within `input`, if any, or
    /// that goes on into the mark of a message.
    pub fn process(&mut self, input: &[i16]) -> Option<Signal> {
        let mut heard = None;
        for &sample in input {
            let x = f64::from(sample);
            let mean_square = self.power.push(x);
            let pair = self.pair.each_mut().map(|c| c.push(x));
            let singles: Vec<f64> = self.singles.iter_mut().map(|c| c.push(x)).collect();
            let loud = |power: f64| 2.0 * power.sqrt() >= self.threshold;
            let pure = |power: f64| 2.0 * power >= PURITY * mean_square;
            let paired = pair.iter().all(|&p| loud(p)) && pure(pair[0] + pair[1]);
            let single = singles.iter().position(|&p| loud(p) && pure(p));

            self.stage = match self.stage {
                Stage::Pair { lasted } if paired => Stage::Pair { lasted: lasted + 1 },
                Stage::Pair { lasted } if lasted >= PAIR_SAMPLES => Stage::Single {
                    waited: 0,
                    lasted: 0,
                    at: usize::MAX,
                },
                Stage::Pair { .. } => Stage::Pair { lasted: 0 },
                Stage::Single { waited, lasted, at }
                    if lasted >= SINGLE_SAMPLES
                        && (single != Some(at) || waited >= SEGMENT_2_WAIT_SAMPLES) =>
                {
                    heard = Some(self.signals[at]);
                    Stage::Pair { lasted: 0 }
                }
                Stage::Single { waited, .. } if waited >= SEGMENT_2_WAIT_SAMPLES => {
                    Stage::Pair { lasted: 0 }
                }
                Stage::Single { waited, lasted, at } => match single {
                    Some(i) if i == at => Stage::Single {
                        waited: waited + 1,
                        lasted: lasted + 1,
                        at,
                    },
                    Some(i) => Stage::Single {
                        waited: waited + 1,
                        lasted: 1,
                        at: i,
                    },
                    None => Stage::Single {
                        waited: waited + 1,
                        lasted: 0,
                        at: usize::MAX,
                    },
                },
            };
        }
        heard
    }
}

// § 7.2.4, 100 ms of mark at 300 bit/s, and § 7.2.5.
const PREAMBLE_BITS: usize = 30;
const OPENING_FLAGS: usize = 2;
// § 7.2.9: fewer than three octets between the flags is an invalid frame.
const MIN_CONTENT: usize = 1;

// § 8.3.2, table 4.
const REVISION: u8 = 2;

// § 8.2.3: the delimiting bits of level 1 and level 2 blocks.
const LAST_OF_LEVEL_1: u8 = 0x80;
const LAST_OF_LEVEL_2: u8 = 0x40;

// Table 5-1.
const V8: u8 = 0x01;
const SHORT_V8: u8 = 0x02;
const TRANSMIT_ACK: u8 = 0x08;
// Table 6-2a.
const DATA: u8 = 0x01;
// Tables 6-3a and 6-3c.
const TRANSPARENT_DATA: u8 = 0x01;
const V22BIS: u8 = 0x02;
const V22: u8 = 0x04;
const V21: u8 = 0x08;

/// The message type of § 8.3.1, table 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ms,
    Cl,
    Clr,
    Ack1,
    Ack2,
    Nak1,
    Nak2,
    Nak3,
    Nak4,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::Ms => 0x1,
            Self::Cl => 0x2,
            Self::Clr => 0x3,
            Self::Ack1 => 0x4,
            Self::Ack2 => 0x5,
            Self::Nak1 => 0x8,
            Self::Nak2 => 0x9,
            Self::Nak3 => 0xA,
            Self::Nak4 => 0xB,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        [
            Self::Ms,
            Self::Cl,
            Self::Clr,
            Self::Ack1,
            Self::Ack2,
            Self::Nak1,
            Self::Nak2,
            Self::Nak3,
            Self::Nak4,
        ]
        .into_iter()
        .find(|k| k.code() == code)
    }

    fn has_fields(self) -> bool {
        matches!(self, Self::Ms | Self::Cl | Self::Clr)
    }
}

/// A message, with only the parameters this modem uses. It reads and
/// skips all others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    /// A V.8 start-up after the transaction (§ 9.9.1).
    pub v8: bool,
    /// A shortened V.8 start-up (§ 9.9.2).
    pub short_v8: bool,
    /// In MS, that ACK(1) must answer it (§ 9.7).
    pub transmit_ack: bool,
    /// The data mode, with the modulations under it, if the message has it.
    pub data: Option<Modes>,
}

impl Message {
    #[must_use]
    pub fn plain(kind: Kind) -> Self {
        Self {
            kind,
            v8: false,
            short_v8: false,
            transmit_ack: false,
            data: None,
        }
    }

    /// The information field, from the identification field on.
    #[must_use]
    pub fn octets(&self) -> Vec<u8> {
        let mut octets = vec![self.kind.code() | REVISION << 4];
        if !self.kind.has_fields() {
            return octets;
        }
        let npar1 = if self.v8 { V8 } else { 0 }
            | if self.short_v8 { SHORT_V8 } else { 0 }
            | if self.transmit_ack { TRANSMIT_ACK } else { 0 };
        octets.extend([npar1 | LAST_OF_LEVEL_1, LAST_OF_LEVEL_1]);
        octets.push(LAST_OF_LEVEL_1);
        match self.data {
            None => octets.push(LAST_OF_LEVEL_1),
            Some(modes) => {
                let modulations =
                    if modes.v22bis { V22BIS | V22 } else { 0 } | if modes.v21 { V21 } else { 0 };
                octets.extend([
                    DATA | LAST_OF_LEVEL_1,
                    TRANSPARENT_DATA,
                    0,
                    modulations | LAST_OF_LEVEL_2 | LAST_OF_LEVEL_1,
                ]);
            }
        }
        octets
    }

    /// Reads an information field. Returns `None` for a message type this
    /// revision does not define.
    #[must_use]
    pub fn parse(octets: &[u8]) -> Option<Self> {
        let (&first, rest) = octets.split_first()?;
        let kind = Kind::from_code(first & 0x0F)?;
        let mut message = Self::plain(kind);
        if !kind.has_fields() {
            return Some(message);
        }
        let mut at = 0;
        let identification = Field::read(rest, &mut at);
        let npar1 = identification.npar1.first().copied().unwrap_or(0);
        message.v8 = npar1 & V8 != 0;
        message.short_v8 = npar1 & SHORT_V8 != 0;
        message.transmit_ack = npar1 & TRANSMIT_ACK != 0;
        let standard = Field::read(rest, &mut at);
        message.data = standard.data();
        Some(message)
    }

    /// The whole message as V.21 bits: the preamble, flags, the frame with
    /// its FCS, and a closing flag.
    #[must_use]
    pub fn bits(&self) -> VecDeque<bool> {
        let mut bits = VecDeque::from(vec![true; PREAMBLE_BITS]);
        for _ in 0..OPENING_FLAGS {
            hdlc::push_flag(&mut bits);
        }
        hdlc::push_frame(&self.octets(), &mut bits);
        bits
    }
}

// The parameter tree of § 8.2.
#[derive(Debug, Default)]
struct Field {
    npar1: Vec<u8>,
    spar1: Vec<u8>,
    par2: Vec<Vec<u8>>,
}

impl Field {
    fn read(octets: &[u8], at: &mut usize) -> Self {
        let npar1 = block(octets, at, LAST_OF_LEVEL_1);
        let spar1 = block(octets, at, LAST_OF_LEVEL_1);
        let set = spar1
            .iter()
            .map(|o| (o & !LAST_OF_LEVEL_1).count_ones())
            .sum::<u32>();
        let par2 = (0..set).map(|_| par2(octets, at)).collect();
        Self { npar1, spar1, par2 }
    }

    fn data(&self) -> Option<Modes> {
        if self.spar1.first().is_none_or(|o| o & DATA == 0) {
            return None;
        }
        let modulations = self.par2.first()?.get(2).copied().unwrap_or(0);
        Some(Modes {
            v90: Pcm::NONE,
            v34: false,
            v22bis: modulations & (V22BIS | V22) != 0,
            v21: modulations & V21 != 0,
        })
    }
}

fn block(octets: &[u8], at: &mut usize, last: u8) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(&octet) = octets.get(*at) {
        *at += 1;
        out.push(octet);
        if octet & last != 0 {
            break;
        }
    }
    out
}

// § 8.2.3: both delimiting bits on the last NPar(2) mean no SPar(2)s follow.
fn par2(octets: &[u8], at: &mut usize) -> Vec<u8> {
    let npar2 = block(octets, at, LAST_OF_LEVEL_2);
    let both = LAST_OF_LEVEL_1 | LAST_OF_LEVEL_2;
    if npar2.last().is_none_or(|o| o & both == both) {
        return npar2;
    }
    let spar2 = block(octets, at, LAST_OF_LEVEL_2);
    let set = spar2.iter().map(|o| (o & 0x3F).count_ones()).sum::<u32>();
    for _ in 0..set {
        block(octets, at, LAST_OF_LEVEL_2);
    }
    npar2
}

/// What the reader found in the bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    Message(Message),
    /// A frame that § 7.2.9 calls invalid, such as one with a bad FCS.
    Invalid,
}

/// Finds messages in a stream of V.21 bits.
#[derive(Debug)]
pub struct Reader {
    deframer: Deframer,
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            deframer: Deframer::new(MIN_CONTENT),
        }
    }

    pub fn push(&mut self, bit: bool) -> Option<Heard> {
        let bad = self.deframer.bad_frames();
        match self.deframer.push(bit) {
            Some(frame) => Message::parse(&frame).map(Heard::Message),
            None if self.deframer.bad_frames() > bad => Some(Heard::Invalid),
            None => None,
        }
    }
}

// § 9.8: a station that has waited 5 s goes back to its initial state.
const TRANSACTION_SAMPLES: usize = 40_000;
const CARRIER_ON_SAMPLES: u32 = 160;
const TRAILING_MARK_BITS: usize = 4;
// § 7.1.4: CRe goes 12 to 15 dB below continuous signals.
const CRE_DBM0: f64 = V21_MAX_LEVEL_DBM0 - 13.0;

/// What follows a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Startup {
    /// This end sends `ANSam` and answers V.8 (§ 9.9.1).
    V8,
    /// This end sends ANS and answers as V.25 (§ 9.9.3).
    V25,
    /// The far end sends the answer tone, and this end calls.
    Caller,
}

// V.21(L) for the initiating station's messages, V.21(H) for the other's (§ 7.2).
#[derive(Debug)]
struct Messages {
    modulator: Modulator,
    demodulator: Demodulator,
    reader: Reader,
}

impl Messages {
    fn new(pair: Pair) -> Self {
        let (transmit, receive) = match pair {
            Pair::Initiating => (V21_ORIGINATE, V21_ANSWER),
            Pair::Responding => (V21_ANSWER, V21_ORIGINATE),
        };
        Self {
            modulator: Modulator::new(transmit, V21_MAX_LEVEL_DBM0),
            demodulator: Demodulator::with_carrier_on(receive, CARRIER_ON_SAMPLES),
            reader: Reader::new(),
        }
    }

    fn send(&mut self, messages: &[Message]) {
        for message in messages {
            self.modulator.push_bits(message.bits());
        }
        self.modulator.push_bits([true; TRAILING_MARK_BITS]);
    }

    fn sending(&self) -> bool {
        self.modulator.pending() > 0
    }

    fn transmit(&mut self, out: &mut [i16]) {
        if self.sending() {
            self.modulator.render(out);
        } else {
            out.fill(0);
        }
    }

    fn hear(&mut self, input: &[i16]) -> Vec<Heard> {
        let mut bits = Vec::new();
        self.demodulator.process(input, &mut bits);
        bits.into_iter()
            .filter_map(|b| self.reader.push(b))
            .collect()
    }
}

fn data_message(kind: Kind, v8: bool, modes: Modes) -> Message {
    Message {
        v8,
        transmit_ack: true,
        data: Some(modes),
        ..Message::plain(kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AnswerStage {
    Cre,
    Awaiting,
    Escaped,
    SentCl,
    SentMs,
    Ending(Startup),
}

/// The answering station's side (§ 10.2.2): `CRe`, then the transaction the
/// calling station picks with its answer.
#[derive(Debug)]
pub struct Answering {
    modes: Modes,
    stage: AnswerStage,
    cre: SignalSender,
    detector: SignalDetector,
    messages: Messages,
    elapsed: usize,
    since: usize,
}

impl Answering {
    /// An answering station that offers the data mode over `modes`.
    #[must_use]
    pub fn new(modes: Modes) -> Self {
        Self {
            modes,
            stage: AnswerStage::Cre,
            cre: SignalSender::new(Signal::CRe, Pair::Initiating, CRE_DBM0),
            detector: SignalDetector::new(Pair::Responding),
            messages: Messages::new(Pair::Initiating),
            elapsed: 0,
            since: 0,
        }
    }

    /// Whether the calling station has answered `CRe`, so that the
    /// transaction goes on.
    #[must_use]
    pub fn engaged(&self) -> bool {
        !matches!(self.stage, AnswerStage::Cre | AnswerStage::Awaiting)
    }

    /// The start-up to go on with, once the transaction has ended and its
    /// last message has been sent.
    #[must_use]
    pub fn startup(&self) -> Option<Startup> {
        match self.stage {
            AnswerStage::Ending(startup) if !self.messages.sending() => Some(startup),
            _ => None,
        }
    }

    pub fn transmit(&mut self, out: &mut [i16]) {
        if self.stage == AnswerStage::Cre {
            self.cre.render(out);
            if self.cre.done() {
                self.stage = AnswerStage::Awaiting;
            }
        } else {
            self.messages.transmit(out);
        }
        self.elapsed += out.len();
        let waiting = self.engaged() && !matches!(self.stage, AnswerStage::Ending(_));
        if waiting && self.elapsed - self.since > TRANSACTION_SAMPLES {
            self.stage = AnswerStage::Ending(Startup::V8);
        }
    }

    pub fn receive(&mut self, input: &[i16]) {
        let signal = self.detector.process(input);
        if self.stage == AnswerStage::Awaiting {
            match signal {
                Some(Signal::CRd) => {
                    let cl = data_message(Kind::Cl, true, self.modes);
                    self.reply(&[cl], AnswerStage::SentCl);
                }
                Some(Signal::ESr) => self.enter(AnswerStage::Escaped),
                _ => {}
            }
        }
        for heard in self.messages.hear(input) {
            self.take(heard);
        }
    }

    fn take(&mut self, heard: Heard) {
        let waiting = self.engaged() && !matches!(self.stage, AnswerStage::Ending(_));
        let message = match heard {
            Heard::Message(message) if waiting => message,
            Heard::Invalid if waiting => {
                let nak = Message::plain(Kind::Nak1);
                self.reply(&[nak], AnswerStage::Ending(Startup::V8));
                return;
            }
            _ => return,
        };
        let ms = data_message(Kind::Ms, true, self.modes);
        match (self.stage, message.kind) {
            (AnswerStage::Escaped, Kind::Cl) => self.reply(&[ms], AnswerStage::SentMs),
            (AnswerStage::Escaped, Kind::Clr) => {
                let cl = data_message(Kind::Cl, true, self.modes);
                self.reply(&[cl, ms], AnswerStage::SentMs);
            }
            (AnswerStage::SentCl, Kind::Ms) => self.select(message),
            (AnswerStage::SentMs, Kind::Ack1) => self.enter(AnswerStage::Ending(Startup::Caller)),
            (AnswerStage::SentMs, Kind::Nak1 | Kind::Nak2 | Kind::Nak3 | Kind::Nak4) => {
                self.enter(AnswerStage::Ending(Startup::V8));
            }
            _ => {}
        }
    }

    // § 9.9: V.8 if MS asks for it, V.25 if it asks for neither kind of V.8.
    fn select(&mut self, ms: Message) {
        let startup = ms.data.and_then(|modes| {
            let common = self.modes.common(modes);
            if ms.v8 {
                Some(Startup::V8)
            } else if !ms.short_v8 && (common.v22bis || common.v21) {
                Some(Startup::V25)
            } else {
                None
            }
        });
        match startup {
            Some(startup) if ms.transmit_ack => {
                self.reply(&[Message::plain(Kind::Ack1)], AnswerStage::Ending(startup));
            }
            Some(startup) => self.enter(AnswerStage::Ending(startup)),
            None => {
                let nak = Message::plain(Kind::Nak3);
                self.reply(&[nak], AnswerStage::Ending(Startup::V8));
            }
        }
    }

    fn reply(&mut self, messages: &[Message], stage: AnswerStage) {
        self.messages.send(messages);
        self.enter(stage);
    }

    fn enter(&mut self, stage: AnswerStage) {
        self.stage = stage;
        self.since = self.elapsed;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallStage {
    Idle,
    Crd,
    AwaitingCl,
    SentMs,
    Done,
}

/// The calling station's side (§ 10.2.1): `CRd` in answer to `CRe` or `MRe`, so
/// that the answering station lists its modes, then MS for the data mode.
#[derive(Debug)]
pub struct Responding {
    modes: Modes,
    stage: CallStage,
    crd: SignalSender,
    detector: SignalDetector,
    messages: Messages,
    elapsed: usize,
    since: usize,
}

impl Responding {
    /// A calling station that selects the data mode over `modes`.
    #[must_use]
    pub fn new(modes: Modes) -> Self {
        Self {
            modes,
            stage: CallStage::Idle,
            crd: SignalSender::new(Signal::CRd, Pair::Responding, V21_MAX_LEVEL_DBM0),
            detector: SignalDetector::new(Pair::Initiating),
            messages: Messages::new(Pair::Responding),
            elapsed: 0,
            since: 0,
        }
    }

    pub fn transmit(&mut self, out: &mut [i16]) {
        if self.stage == CallStage::Crd {
            self.crd.render(out);
            if self.crd.done() {
                self.enter(CallStage::AwaitingCl);
            }
        } else {
            self.messages.transmit(out);
        }
        self.elapsed += out.len();
        let waiting = matches!(self.stage, CallStage::AwaitingCl | CallStage::SentMs);
        if waiting && self.elapsed - self.since > TRANSACTION_SAMPLES {
            self.stage = CallStage::Done;
        }
    }

    pub fn receive(&mut self, input: &[i16]) {
        let signal = self.detector.process(input);
        if self.stage == CallStage::Idle && matches!(signal, Some(Signal::CRe | Signal::MRe)) {
            self.enter(CallStage::Crd);
        }
        for heard in self.messages.hear(input) {
            match (self.stage, heard) {
                (CallStage::AwaitingCl | CallStage::SentMs, Heard::Invalid) => {
                    self.messages.send(&[Message::plain(Kind::Nak1)]);
                    self.enter(CallStage::Done);
                }
                (CallStage::AwaitingCl, Heard::Message(cl)) if cl.kind == Kind::Cl => {
                    match cl.data {
                        Some(theirs) => {
                            let ms = data_message(Kind::Ms, cl.v8, self.modes.common(theirs));
                            self.messages.send(&[ms]);
                            self.enter(CallStage::SentMs);
                        }
                        None => self.enter(CallStage::Done),
                    }
                }
                (CallStage::SentMs, Heard::Message(_)) => self.enter(CallStage::Done),
                _ => {}
            }
        }
    }

    fn enter(&mut self, stage: CallStage) {
        self.stage = stage;
        self.since = self.elapsed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansam::{AnswerTone, AnswerToneKind};

    const BOTH: Modes = Modes {
        v90: Pcm::NONE,
        v34: false,
        v22bis: true,
        v21: true,
    };
    const V21_ONLY: Modes = Modes {
        v90: Pcm::NONE,
        v34: false,
        v22bis: false,
        v21: true,
    };

    fn exchange(answering: &mut Answering, responding: &mut Responding, ms: usize) {
        let mut up = [0; 160];
        let mut down = [0; 160];
        for _ in 0..ms / 20 {
            answering.transmit(&mut down);
            responding.transmit(&mut up);
            answering.receive(&up);
            responding.receive(&down);
        }
    }

    fn drain(answering: &mut Answering) {
        let mut out = [0; 160];
        while answering.messages.sending() {
            answering.transmit(&mut out);
        }
    }

    fn answering_at(stage: AnswerStage) -> Answering {
        let mut answering = Answering::new(BOTH);
        answering.stage = stage;
        answering
    }

    #[test]
    fn a_calling_station_answers_cre_and_selects_v8() {
        let mut answering = Answering::new(BOTH);
        let mut responding = Responding::new(V21_ONLY);
        exchange(&mut answering, &mut responding, 1000);
        assert!(answering.engaged());
        assert_eq!(answering.startup(), None);
        exchange(&mut answering, &mut responding, 2000);
        assert_eq!(answering.startup(), Some(Startup::V8));
        assert_eq!(responding.stage, CallStage::Done);
    }

    #[test]
    fn without_an_answer_cre_starts_no_transaction() {
        let mut answering = Answering::new(BOTH);
        let mut out = [0; 160];
        for _ in 0..150 {
            answering.transmit(&mut out);
            answering.receive(&[0; 160]);
        }
        assert!(!answering.engaged());
        assert_eq!(answering.startup(), None);
    }

    #[test]
    fn an_ms_without_either_v8_asks_for_v25() {
        let mut answering = answering_at(AnswerStage::SentCl);
        let ms = data_message(Kind::Ms, false, V21_ONLY);
        answering.take(Heard::Message(ms));
        assert_eq!(answering.startup(), None, "ACK(1) goes first");
        drain(&mut answering);
        assert_eq!(answering.startup(), Some(Startup::V25));
    }

    #[test]
    fn an_ms_for_another_mode_is_refused_and_v8_follows() {
        let mut answering = answering_at(AnswerStage::SentCl);
        answering.take(Heard::Message(Message::plain(Kind::Ms)));
        assert!(answering.messages.sending());
        drain(&mut answering);
        assert_eq!(answering.startup(), Some(Startup::V8));
    }

    #[test]
    fn esr_and_cl_make_this_end_select_and_then_call() {
        let mut answering = answering_at(AnswerStage::Escaped);
        let cl = data_message(Kind::Cl, true, V21_ONLY);
        answering.take(Heard::Message(cl));
        assert_eq!(answering.stage, AnswerStage::SentMs);
        answering.take(Heard::Message(Message::plain(Kind::Ack1)));
        drain(&mut answering);
        assert_eq!(answering.startup(), Some(Startup::Caller));
    }

    #[test]
    fn a_stalled_transaction_ends_in_v8_after_5_s() {
        let mut answering = answering_at(AnswerStage::SentCl);
        let mut out = [0; 160];
        for _ in 0..251 {
            answering.transmit(&mut out);
        }
        assert_eq!(answering.startup(), Some(Startup::V8));
    }

    fn cl() -> Message {
        Message {
            v8: true,
            transmit_ack: true,
            data: Some(BOTH),
            ..Message::plain(Kind::Cl)
        }
    }

    fn read(bits: impl IntoIterator<Item = bool>) -> Vec<Heard> {
        let mut reader = Reader::new();
        bits.into_iter().filter_map(|b| reader.push(b)).collect()
    }

    #[test]
    fn encodes_a_cl_with_v8_and_the_data_modulations() {
        assert_eq!(
            cl().octets(),
            [0x22, 0x89, 0x80, 0x80, 0x81, 0x01, 0x00, 0xCE]
        );
    }

    #[test]
    fn ack_and_nak_are_one_octet() {
        assert_eq!(Message::plain(Kind::Ack1).octets(), [0x24]);
        assert_eq!(Message::plain(Kind::Nak3).octets(), [0x2A]);
    }

    #[test]
    fn messages_read_back_from_their_bits() {
        let ms = Message {
            v8: true,
            data: Some(V21_ONLY),
            ..Message::plain(Kind::Ms)
        };
        for message in [cl(), ms, Message::plain(Kind::Ack1)] {
            assert_eq!(read(message.bits()), [Heard::Message(message)]);
        }
    }

    #[test]
    fn a_corrupt_message_is_invalid() {
        let mut bits = cl().bits();
        bits[PREAMBLE_BITS + 8 * OPENING_FLAGS + 3] ^= true;
        assert_eq!(read(bits), [Heard::Invalid]);
    }

    #[test]
    fn skips_the_parameters_it_does_not_use() {
        let octets = [
            0x21, // MS, revision 2
            0x81, // V.8
            0x81, // network type
            0xC4, // digital PSTN access
            0xC0, // non-standard capabilities
            0xA1, // data and analogue telephony
            0x03, // transparent data, V.42
            0x10, // V.34
            0xC8, // V.21
            0xC1, // voice
        ];
        let ms = Message::parse(&octets).unwrap();
        assert!(ms.v8 && !ms.transmit_ack);
        assert_eq!(ms.data, Some(V21_ONLY));
    }

    #[test]
    fn a_message_without_the_data_mode_has_none() {
        let octets = [0x21, 0x81, 0x80, 0x80, 0xA0, 0xC1];
        assert_eq!(Message::parse(&octets).unwrap().data, None);
    }

    #[test]
    fn a_message_crosses_v21_after_its_preamble_alone() {
        let mut modulator = Modulator::new(V21_ORIGINATE, V21_MAX_LEVEL_DBM0);
        modulator.push_bits(cl().bits());
        let mut samples = vec![0; 8000];
        modulator.render(&mut samples);
        let mut demodulator = Demodulator::with_carrier_on(V21_ORIGINATE, 160);
        let mut bits = Vec::new();
        demodulator.process(&samples, &mut bits);
        assert_eq!(read(bits), [Heard::Message(cl())]);
    }

    const LEVEL: f64 = V21_MAX_LEVEL_DBM0;
    // § 7.1.4: 12 to 15 dB below the level of continuous signals.
    const LOW_LEVEL: f64 = V21_MAX_LEVEL_DBM0 - 13.0;

    fn signal(signal: Signal, pair: Pair, level: f64) -> Vec<i16> {
        let mut sender = SignalSender::new(signal, pair, level);
        let mut out = vec![0; 800];
        while !sender.done() {
            let mut chunk = [0; 160];
            sender.render(&mut chunk);
            out.extend(chunk);
        }
        out.extend([0; 800]);
        out
    }

    fn heard(pair: Pair, samples: &[i16]) -> Vec<Signal> {
        let mut detector = SignalDetector::new(pair);
        samples
            .chunks(160)
            .filter_map(|c| detector.process(c))
            .collect()
    }

    #[test]
    fn hears_each_signal_after_its_own_dual_tone() {
        for pair in [Pair::Initiating, Pair::Responding] {
            for &s in pair.signals() {
                assert_eq!(heard(pair, &signal(s, pair, LEVEL)), [s], "{s:?}");
            }
        }
    }

    #[test]
    fn hears_cre_at_its_low_level_with_the_short_segment_1() {
        let cre = signal(Signal::CRe, Pair::Initiating, LOW_LEVEL);
        assert_eq!(heard(Pair::Initiating, &cre), [Signal::CRe]);
    }

    #[test]
    fn reports_cre_only_once_it_has_ended() {
        let cre = signal(Signal::CRe, Pair::Initiating, LOW_LEVEL);
        let end = 800 + SHORT_SEGMENT_1_SAMPLES + SEGMENT_2_SAMPLES;
        let mut detector = SignalDetector::new(Pair::Initiating);
        let at = cre
            .chunks(8)
            .position(|c| detector.process(c).is_some())
            .map(|chunk| chunk * 8);
        assert!(
            at.is_some_and(|at| at >= end),
            "heard at {at:?}, ended at {end}"
        );
    }

    #[test]
    fn does_not_hear_the_other_pair() {
        let cre = signal(Signal::CRe, Pair::Initiating, LEVEL);
        assert!(heard(Pair::Responding, &cre).is_empty());
        let crd = signal(Signal::CRd, Pair::Responding, LEVEL);
        assert!(heard(Pair::Initiating, &crd).is_empty());
    }

    #[test]
    fn esr_is_heard_when_v21_mark_goes_on_after_it() {
        let mut samples = signal(Signal::ESr, Pair::Responding, LEVEL);
        samples.truncate(samples.len() - 800);
        let mut mark = vec![0; 4000];
        Modulator::new(V21_ANSWER, LEVEL).render(&mut mark);
        samples.extend(mark);
        assert_eq!(heard(Pair::Responding, &samples), [Signal::ESr]);
    }

    #[test]
    fn ignores_ansam_v21_and_silence() {
        let mut ansam = vec![0; 16_000];
        AnswerTone::new(AnswerToneKind::Ansam, true, LEVEL).render(&mut ansam);
        let mut modulator = Modulator::new(V21_ANSWER, LEVEL);
        modulator.push_bits((0..2000).map(|n| n % 3 == 0));
        let mut v21 = vec![0; 16_000];
        modulator.render(&mut v21);
        for pair in [Pair::Initiating, Pair::Responding] {
            assert!(heard(pair, &ansam).is_empty());
            assert!(heard(pair, &v21).is_empty());
            assert!(heard(pair, &[0; 8000]).is_empty());
        }
    }
}
