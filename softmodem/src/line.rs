//! One call's audio: the answer sequence, the carrier handshake and the data.

use softmodem_dsp::fsk::{
    Channel, Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE,
};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone, ToneDetector};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_transport::{Call, FRAME_SAMPLES};

// V.25: silence, answer tone, a short gap, then the answering carrier.
const ANSWER_SILENCE: usize = 16_000;
const ANSWER_TONE: usize = 26_400;
const ANSWER_GAP: usize = 600;
// Longer than the far end's carrier detect, so its first bytes are not lost.
const ORIGINATE_CARRIER_BEFORE_CONNECT: usize = 4_800;

/// Which end of the V.21 link this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Originate,
    Answer,
}

impl Role {
    fn channels(self) -> (Channel, Channel) {
        match self {
            Self::Originate => (V21_ORIGINATE, V21_ANSWER),
            Self::Answer => (V21_ANSWER, V21_ORIGINATE),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Received {
    pub(crate) bytes: Vec<u8>,
    pub(crate) connected: bool,
}

#[derive(Debug)]
pub(crate) struct Line {
    pub(crate) call: Call,
    handshake: Option<Handshake>,
}

#[derive(Debug)]
struct Handshake {
    role: Role,
    receive_channel: Channel,
    modulator: Modulator,
    demodulator: Demodulator,
    answer_tone: Tone,
    answer_tone_detector: ToneDetector,
    decoder: Decoder,
    sent: usize,
    carrier_since: Option<usize>,
    heard_carrier: bool,
    connected: bool,
    early: Vec<u8>,
}

impl Line {
    /// A line in `role`, or silent with no handshake for a call placed with
    /// `;`, until [`Line::start`] is called.
    pub(crate) fn new(call: Call, role: Option<Role>) -> Self {
        Self {
            call,
            handshake: role.map(Handshake::new),
        }
    }

    pub(crate) fn start(&mut self, role: Role) {
        self.handshake = Some(Handshake::new(role));
    }

    pub(crate) fn has_handshake(&self) -> bool {
        self.handshake.is_some()
    }

    pub(crate) fn pending_bits(&self) -> usize {
        self.handshake.as_ref().map_or(0, |h| h.modulator.pending())
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        if let Some(handshake) = &mut self.handshake {
            handshake
                .modulator
                .push_bits(bytes.iter().flat_map(|&b| frame(b)));
        }
    }

    pub(crate) fn carrier(&self) -> bool {
        self.handshake
            .as_ref()
            .is_some_and(|h| h.demodulator.carrier())
    }

    /// The next 20 ms to send.
    pub(crate) fn transmit(&mut self) -> Vec<i16> {
        let mut samples = vec![0; FRAME_SAMPLES];
        if let Some(handshake) = &mut self.handshake {
            handshake.transmit(&mut samples);
        }
        samples
    }

    pub(crate) fn receive(&mut self, samples: &[i16]) -> Received {
        self.handshake
            .as_mut()
            .map(|h| h.receive(samples))
            .unwrap_or_default()
    }
}

impl Handshake {
    fn new(role: Role) -> Self {
        let (transmit, receive) = role.channels();
        Self {
            role,
            receive_channel: receive,
            modulator: Modulator::new(transmit, V21_MAX_LEVEL_DBM0),
            demodulator: Demodulator::new(receive),
            answer_tone: Tone::new(ANSWER_TONE_HZ, V21_MAX_LEVEL_DBM0),
            answer_tone_detector: ToneDetector::new(ANSWER_TONE_HZ),
            decoder: Decoder::new(),
            sent: 0,
            carrier_since: None,
            heard_carrier: false,
            connected: false,
            early: Vec::new(),
        }
    }

    fn transmit(&mut self, samples: &mut [i16]) {
        match self.role {
            Role::Answer if self.sent < ANSWER_SILENCE => {}
            Role::Answer if self.sent < ANSWER_SILENCE + ANSWER_TONE => {
                self.answer_tone.render(samples);
            }
            Role::Answer if self.sent < ANSWER_SILENCE + ANSWER_TONE + ANSWER_GAP => {}
            Role::Answer => self.modulator.render(samples),
            Role::Originate if self.heard_carrier => {
                self.carrier_since.get_or_insert(self.sent);
                self.modulator.render(samples);
            }
            Role::Originate => {}
        }
        self.sent += samples.len();
    }

    fn receive(&mut self, samples: &[i16]) -> Received {
        let mut received = Received::default();
        if self.role == Role::Originate
            && !self.heard_carrier
            && self.answer_tone_detector.process(samples)
        {
            self.demodulator = Demodulator::new(self.receive_channel);
            return received;
        }

        let mut bits = Vec::new();
        self.demodulator.process(samples, &mut bits);
        let bytes = bits.into_iter().filter_map(|b| self.decoder.push(b));
        if self.connected {
            received.bytes.extend(bytes);
        } else {
            // The far end may send before we report CONNECT; a real modem keeps it.
            self.early.extend(bytes);
        }
        if !self.demodulator.carrier() {
            self.decoder.reset();
        }
        if self.connected {
            return received;
        }

        self.heard_carrier |= self.demodulator.carrier();
        let ready = match self.role {
            Role::Answer => self.heard_carrier,
            Role::Originate => self
                .carrier_since
                .is_some_and(|since| self.sent - since >= ORIGINATE_CARRIER_BEFORE_CONNECT),
        };
        if ready {
            self.connected = true;
            received.connected = true;
            received.bytes = std::mem::take(&mut self.early);
        }
        received
    }
}
