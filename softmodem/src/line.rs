//! One call's audio: the answer sequence, then a data pump.

use std::time::Duration;

use softmodem_dsp::pump::{DataPump, Offer, Role};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone, ToneDetector};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_transport::{Call, FRAME_SAMPLES};

// V.25: silence, answer tone, a short gap, then the data pump.
const ANSWER_SILENCE: usize = 16_000;
const ANSWER_TONE: usize = 26_400;
const ANSWER_GAP: usize = 600;
const ANSWER_TONE_DBM0: f64 = -13.0;

#[derive(Debug, Default)]
pub(crate) struct Received {
    pub(crate) bytes: Vec<u8>,
    pub(crate) connected: bool,
}

#[derive(Debug)]
pub(crate) struct Line {
    pub(crate) call: Call,
    offer: Offer,
    handshake: Option<Handshake>,
}

#[derive(Debug)]
struct Handshake {
    role: Role,
    offer: Offer,
    pump: Box<dyn DataPump>,
    answer_tone: Tone,
    answer_tone_detector: ToneDetector,
    decoder: Option<Decoder>,
    sent: usize,
    heard_carrier: bool,
    connected: bool,
    early: Vec<u8>,
}

impl Line {
    /// A line in `role`, or silent with no handshake for a call placed with
    /// `;`, until [`Line::start`] is called.
    pub(crate) fn new(call: Call, offer: Offer, role: Option<Role>) -> Self {
        Self {
            call,
            offer,
            handshake: role.map(|role| Handshake::new(offer, role)),
        }
    }

    pub(crate) fn start(&mut self, role: Role) {
        self.handshake = Some(Handshake::new(self.offer, role));
    }

    pub(crate) fn retrain(&mut self) {
        if let Some(handshake) = &mut self.handshake {
            handshake.pump.retrain();
        }
    }

    pub(crate) fn has_handshake(&self) -> bool {
        self.handshake.is_some()
    }

    pub(crate) fn bit_rate(&self) -> Option<u32> {
        self.handshake.as_ref().map(|h| h.pump.bit_rate())
    }

    /// How long the bits queued to send will take.
    #[expect(clippy::cast_precision_loss, reason = "a few hundred bits")]
    pub(crate) fn queued(&self) -> Duration {
        self.handshake.as_ref().map_or(Duration::ZERO, |h| {
            Duration::from_secs_f64(h.pump.pending() as f64 / f64::from(h.pump.bit_rate()))
        })
    }

    pub(crate) fn send(&mut self, bytes: &[u8]) {
        if let Some(handshake) = &mut self.handshake {
            let bits: Vec<bool> = bytes.iter().flat_map(|&b| frame(b)).collect();
            handshake.pump.push_bits(&bits);
        }
    }

    pub(crate) fn carrier(&self) -> bool {
        self.handshake.as_ref().is_some_and(|h| h.pump.carrier())
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
    fn new(offer: Offer, role: Role) -> Self {
        let pump = offer.pump(role);
        Self {
            role,
            offer,
            decoder: None,
            pump,
            answer_tone: Tone::new(ANSWER_TONE_HZ, ANSWER_TONE_DBM0),
            answer_tone_detector: ToneDetector::new(ANSWER_TONE_HZ),
            sent: 0,
            heard_carrier: false,
            connected: false,
            early: Vec::new(),
        }
    }

    fn transmit(&mut self, samples: &mut [i16]) {
        match self.role {
            _ if self.pump.sends_own_answer_tone() => self.pump.transmit(samples),
            Role::Answer if self.sent < ANSWER_SILENCE => {}
            Role::Answer if self.sent < ANSWER_SILENCE + ANSWER_TONE => {
                self.answer_tone.render(samples);
            }
            Role::Answer if self.sent < ANSWER_SILENCE + ANSWER_TONE + ANSWER_GAP => {}
            _ => self.pump.transmit(samples),
        }
        self.sent += samples.len();
    }

    fn receive(&mut self, samples: &[i16]) -> Received {
        let mut received = Received::default();
        if self.role == Role::Originate
            && !self.heard_carrier
            && !self.pump.sends_own_answer_tone()
            && self.answer_tone_detector.process(samples)
        {
            self.pump = self.offer.pump(self.role);
            return received;
        }

        let mut bits = Vec::new();
        self.pump.receive(samples, &mut bits);
        if !bits.is_empty() {
            let pump = &self.pump;
            let decoder = self.decoder.get_or_insert_with(|| pump.decoder());
            let bytes = bits.into_iter().filter_map(|b| decoder.push(b));
            if self.connected {
                received.bytes.extend(bytes);
            } else {
                // The far end may send before we report CONNECT; a real modem keeps it.
                self.early.extend(bytes);
            }
        }
        if !self.pump.carrier()
            && let Some(decoder) = &mut self.decoder
        {
            decoder.reset();
        }
        if self.connected {
            return received;
        }

        self.heard_carrier |= self.pump.carrier();
        if self.pump.connected() {
            self.connected = true;
            received.connected = true;
            received.bytes = std::mem::take(&mut self.early);
        }
        received
    }
}
