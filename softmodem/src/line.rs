//! One call's audio: the answer sequence, then a data pump, and over it V.42
//! or plain start-stop characters.

use std::time::{Duration, Instant};

use softmodem_dsp::pump::{DataPump, Offer, Role};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone, ToneDetector};
use softmodem_link::v42bis::Directions;
use softmodem_link::{Link, Setup, Status};
use softmodem_transport::{Call, FRAME_SAMPLES};

// V.25: silence, answer tone, a short gap, then the data pump.
const ANSWER_SILENCE: usize = 16_000;
const ANSWER_TONE: usize = 26_400;
const ANSWER_GAP: usize = 600;
const ANSWER_TONE_DBM0: f64 = -13.0;
// Bits kept queued in the pump, so that it never idles between frames.
const PUMP_AHEAD: Duration = Duration::from_millis(60);
const LOW_WATER: Duration = Duration::from_millis(67);

/// How a call tries V.42 in each role.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Setups {
    pub(crate) originate: Setup,
    pub(crate) answer: Setup,
}

#[derive(Debug, Default)]
pub(crate) struct Received {
    pub(crate) bytes: Vec<u8>,
    /// The call is connected now, with LAPM if `reliable`.
    pub(crate) connected: bool,
    pub(crate) reliable: bool,
    /// V.42 bis on the connection, in this end's directions.
    pub(crate) compression: Option<Directions>,
}

#[derive(Debug)]
pub(crate) struct Line {
    pub(crate) call: Call,
    offer: Offer,
    setups: Setups,
    handshake: Option<Handshake>,
}

#[derive(Debug)]
struct Handshake {
    role: Role,
    offer: Offer,
    setup: Setup,
    pump: Box<dyn DataPump>,
    answer_tone: Tone,
    answer_tone_detector: ToneDetector,
    link: Option<Link>,
    sent: usize,
    heard_carrier: bool,
    connected: bool,
}

impl Line {
    /// A line in `role`, or silent with no handshake for a call placed with
    /// `;`, until [`Line::start`] is called.
    pub(crate) fn new(call: Call, offer: Offer, setups: Setups, role: Option<Role>) -> Self {
        Self {
            call,
            offer,
            setups,
            handshake: role.map(|role| Handshake::new(offer, setups, role)),
        }
    }

    pub(crate) fn start(&mut self, role: Role) {
        self.handshake = Some(Handshake::new(self.offer, self.setups, role));
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

    /// Whether the modem should read more from the computer.
    #[expect(clippy::cast_precision_loss, reason = "a few hundred bits")]
    pub(crate) fn wants_input(&self) -> bool {
        let Some(handshake) = &self.handshake else {
            return true;
        };
        let Some(link) = &handshake.link else {
            return true;
        };
        if link.status() == Status::Reliable {
            return link.queued() < 2 * link.frame_size();
        }
        let bits = handshake.pump.pending() + link.queued() * 10;
        let queued = Duration::from_secs_f64(bits as f64 / f64::from(handshake.pump.bit_rate()));
        queued < LOW_WATER
    }

    pub(crate) fn send(&mut self, bytes: &[u8], now: Instant) {
        if let Some(handshake) = &mut self.handshake
            && let Some(link) = &mut handshake.link
        {
            link.send(bytes);
            handshake.fill(now);
        }
    }

    pub(crate) fn carrier(&self) -> bool {
        self.handshake.as_ref().is_some_and(|h| h.pump.carrier())
    }

    /// Whether V.42 has ended the call, or was required and failed.
    pub(crate) fn released(&self) -> bool {
        self.handshake
            .as_ref()
            .and_then(|h| h.link.as_ref())
            .is_some_and(|link| link.status() == Status::Released)
    }

    /// The next 20 ms to send.
    pub(crate) fn transmit(&mut self, now: Instant) -> Vec<i16> {
        let mut samples = vec![0; FRAME_SAMPLES];
        if let Some(handshake) = &mut self.handshake {
            handshake.fill(now);
            handshake.transmit(&mut samples);
        }
        samples
    }

    pub(crate) fn receive(&mut self, samples: &[i16], now: Instant) -> Received {
        self.handshake
            .as_mut()
            .map(|h| h.receive(samples, now))
            .unwrap_or_default()
    }
}

impl Handshake {
    fn new(offer: Offer, setups: Setups, role: Role) -> Self {
        let pump = offer.pump(role);
        Self {
            role,
            offer,
            setup: match role {
                Role::Originate => setups.originate,
                Role::Answer => setups.answer,
            },
            pump,
            answer_tone: Tone::new(ANSWER_TONE_HZ, ANSWER_TONE_DBM0),
            answer_tone_detector: ToneDetector::new(ANSWER_TONE_HZ),
            link: None,
            sent: 0,
            heard_carrier: false,
            connected: false,
        }
    }

    fn link(&mut self) -> &mut Link {
        let (role, setup, pump) = (self.role, self.setup, &self.pump);
        self.link
            .get_or_insert_with(|| Link::new(role, setup, pump.decoder()))
    }

    /// Tops up the pump's queue from the link.
    fn fill(&mut self, now: Instant) {
        if !self.pump.connected() {
            return;
        }
        let rate = self.pump.bit_rate();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a few hundred bits"
        )]
        let target = (PUMP_AHEAD.as_secs_f64() * f64::from(rate)) as usize;
        let pending = self.pump.pending();
        if pending >= target {
            return;
        }
        let link = self.link();
        link.start(rate, now);
        let bits = link.transmit(target - pending, now);
        self.pump.push_bits(&bits);
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

    fn receive(&mut self, samples: &[i16], now: Instant) -> Received {
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
        let carrier = self.pump.carrier();
        let connected = self.pump.connected();
        let rate = self.pump.bit_rate();
        // The far end may send before we report CONNECT; a real modem keeps it.
        if !bits.is_empty() || connected {
            let link = self.link();
            if connected {
                link.start(rate, now);
            }
            link.receive(&bits, now);
            if !carrier {
                link.carrier_lost();
            }
        }
        self.heard_carrier |= carrier;
        let Some(link) = &mut self.link else {
            return received;
        };
        let status = link.status();
        if !self.connected {
            if !matches!(status, Status::Reliable | Status::Normal) {
                return received;
            }
            self.connected = true;
            received.connected = true;
            received.reliable = status == Status::Reliable;
            received.compression = link.compression();
        }
        received.bytes = link.take_received();
        received
    }
}
