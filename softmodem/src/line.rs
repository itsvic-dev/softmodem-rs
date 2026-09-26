// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! One call's audio: the answer sequence, then a data pump, and over it V.42
//! or plain start-stop characters.

use std::time::{Duration, Instant};

use softmodem_dsp::pump::{DataPump, Offer, Role};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone, ToneDetector};
use softmodem_link::v42bis::Directions;
use softmodem_link::{Link, Setup, Status};
use softmodem_transport::{Call, FRAME_SAMPLES};
use tracing::info;

use crate::journal::{Event, Header, Journal};

// V.25: silence, answer tone, a short gap, then the data pump.
const ANSWER_SILENCE: usize = 16_000;
const ANSWER_TONE: usize = 26_400;
const ANSWER_GAP: usize = 600;
const ANSWER_TONE_DBM0: f64 = -13.0;
// Bits kept queued in the pump, so that it never idles between frames.
const PUMP_AHEAD: Duration = Duration::from_millis(60);
const LOW_WATER: Duration = Duration::from_millis(67);
// Well within the ten T401 retries after which LAPM gives up.
const QUIET: Duration = Duration::from_secs(2);

/// How a call tries V.42 in each role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Received {
    /// The error control, as `+ER` reports it.
    pub(crate) fn protocol(&self) -> &'static str {
        if self.reliable { "LAPM" } else { "NONE" }
    }

    /// The compression, as `+DR` reports it.
    pub(crate) fn compression_name(&self) -> &'static str {
        match self.compression.map(|c| (c.transmit, c.receive)) {
            Some((true, true)) => "V42B",
            Some((false, true)) => "V42B RD",
            Some((true, false)) => "V42B TD",
            _ => "NONE",
        }
    }
}

/// A call's line, or for a replay a line with no call under it.
#[derive(Debug)]
pub(crate) struct Line<C = Call> {
    pub(crate) call: C,
    offer: Offer,
    setups: Setups,
    handshake: Option<Handshake>,
    journal: Option<Journal>,
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
    rate: u32,
    recovery: Recovery,
    stage: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Renegotiate,
    Retrain,
}

// Data mode with no sound bits for QUIET renegotiates first, and then retrains.
#[derive(Debug, Default)]
struct Recovery {
    quiet_from: Option<Instant>,
    tried: Option<(Action, Instant)>,
}

impl Recovery {
    fn poll(&mut self, now: Instant, in_data: bool, sound: Option<Instant>) -> Option<Action> {
        if !in_data {
            self.quiet_from = None;
            return None;
        }
        if let (Some((_, at)), Some(sound)) = (self.tried, sound)
            && sound > at
        {
            self.tried = None;
        }
        let from = self.quiet_from.get_or_insert(now);
        if let Some(sound) = sound {
            *from = (*from).max(sound);
        }
        if now - *from < QUIET {
            return None;
        }
        let action = match self.tried {
            None => Action::Renegotiate,
            Some((Action::Renegotiate, _)) => Action::Retrain,
            Some((Action::Retrain, _)) => return None,
        };
        self.tried = Some((action, now));
        self.quiet_from = Some(now);
        Some(action)
    }
}

impl<C> Line<C> {
    /// A line in `role`, or silent with no handshake for a call placed with
    /// `;`, until [`Line::start`] is called, that writes all it is given to
    /// `journal`.
    pub(crate) fn new(call: C, header: Header, mut journal: Option<Journal>) -> Self {
        if let Some(journal) = &mut journal {
            journal.header(&header);
        }
        let Header {
            offer,
            setups,
            role,
        } = header;
        Self {
            call,
            offer,
            setups,
            handshake: role.map(|role| Handshake::new(offer, setups, role)),
            journal,
        }
    }

    fn note(&mut self, now: Instant, event: &Event) {
        if let Some(journal) = &mut self.journal {
            journal.event(now, event);
        }
    }

    pub(crate) fn start(&mut self, role: Role, now: Instant) {
        self.note(now, &Event::Start(role));
        self.handshake = Some(Handshake::new(self.offer, self.setups, role));
    }

    pub(crate) fn retrain(&mut self, now: Instant) {
        self.note(now, &Event::Retrain);
        if let Some(handshake) = &mut self.handshake {
            handshake.pump.retrain();
        }
    }

    /// Starts ending the call with the far end, and says whether the
    /// modulation can, so that the modem waits for [`Line::cleared`].
    pub(crate) fn clear_down(&mut self, now: Instant) -> bool {
        self.note(now, &Event::ClearDown);
        let Some(handshake) = &mut self.handshake else {
            return false;
        };
        if !handshake.pump.connected() {
            return false;
        }
        handshake.pump.clear_down();
        !handshake.pump.connected()
    }

    pub(crate) fn cleared(&self) -> bool {
        self.handshake.as_ref().is_some_and(|h| h.pump.cleared())
    }

    pub(crate) fn has_handshake(&self) -> bool {
        self.handshake.is_some()
    }

    pub(crate) fn bit_rate(&self) -> Option<u32> {
        self.handshake.as_ref().map(|h| h.pump.bit_rate())
    }

    pub(crate) fn transmit_rate(&self) -> Option<u32> {
        self.handshake.as_ref().map(|h| h.pump.transmit_rate())
    }

    /// What the modem logs as `received` connects the call.
    pub(crate) fn connect_log(&self, received: &Received) -> String {
        let rate = self.bit_rate().unwrap_or_default();
        let (protocol, compression) = (received.protocol(), received.compression_name());
        match self.transmit_rate() {
            Some(up) if up != rate => format!(
                "CONNECT {rate}, transmitting at {up}, error control {protocol}, compression {compression}"
            ),
            _ => format!("CONNECT {rate}, error control {protocol}, compression {compression}"),
        }
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
        self.note(now, &Event::Send(bytes.to_vec()));
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
        self.note(now, &Event::Tx);
        let mut samples = vec![0; FRAME_SAMPLES];
        if let Some(handshake) = &mut self.handshake {
            handshake.fill(now);
            handshake.transmit(&mut samples);
            handshake.follow_stage();
        }
        samples
    }

    /// The frame from [`Line::transmit`] was not sent after all.
    pub(crate) fn dropped(&mut self, now: Instant) {
        self.note(now, &Event::Dropped);
    }

    pub(crate) fn receive(&mut self, samples: &[i16], now: Instant) -> Received {
        self.note(now, &Event::Rx(samples.len()));
        let Some(handshake) = &mut self.handshake else {
            return Received::default();
        };
        let received = handshake.receive(samples, now);
        handshake.follow_stage();
        received
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
            rate: 0,
            recovery: Recovery::default(),
            stage: String::new(),
        }
    }

    fn follow_stage(&mut self) {
        let answer_tone = self.role == Role::Answer && !self.pump.sends_own_answer_tone();
        let stage = match self.sent {
            sent if answer_tone && sent <= ANSWER_SILENCE => "V.25 silence".into(),
            sent if answer_tone && sent <= ANSWER_SILENCE + ANSWER_TONE => {
                "V.25 answer tone".into()
            }
            sent if answer_tone && sent <= ANSWER_SILENCE + ANSWER_TONE + ANSWER_GAP => {
                "V.25 gap".into()
            }
            _ => self.pump.stage(),
        };
        if stage != self.stage {
            info!(stage, "handshake");
            self.stage = stage;
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

    fn recover(&mut self, now: Instant, connected: bool) {
        let Some(link) = &self.link else {
            return;
        };
        let in_data = connected && link.status() == Status::Reliable;
        match self.recovery.poll(now, in_data, link.last_sound()) {
            Some(Action::Renegotiate) => {
                info!("nothing sound from the far end, renegotiating");
                self.pump.renegotiate();
            }
            Some(Action::Retrain) => {
                info!("nothing sound from the far end after a renegotiation, retraining");
                self.pump.retrain();
            }
            None => {}
        }
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
        if connected && self.connected && rate != self.rate {
            info!(rate, "bit rate changed");
        }
        if connected {
            self.rate = rate;
        }
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
        self.recover(now, connected);
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

#[cfg(test)]
mod tests {
    use super::*;

    const STEP: Duration = Duration::from_millis(20);

    // Polls every 20 ms for `time`, and gives each action with when it came.
    fn watch(
        recovery: &mut Recovery,
        from: Instant,
        time: Duration,
        in_data: impl Fn(Instant) -> bool,
        sound: impl Fn(Instant) -> Option<Instant>,
    ) -> Vec<(Action, Duration)> {
        let mut actions = Vec::new();
        let mut now = from;
        while now < from + time {
            now += STEP;
            if let Some(action) = recovery.poll(now, in_data(now), sound(now)) {
                actions.push((action, now - from));
            }
        }
        actions
    }

    #[test]
    fn renegotiates_then_retrains_then_leaves_it_to_lapm() {
        let start = Instant::now();
        let actions = watch(
            &mut Recovery::default(),
            start,
            Duration::from_secs(20),
            |_| true,
            |_| Some(start),
        );
        assert_eq!(
            actions,
            [
                (Action::Renegotiate, STEP + QUIET),
                (Action::Retrain, STEP + 2 * QUIET)
            ]
        );
    }

    #[test]
    fn times_the_quiet_only_in_data_mode() {
        let start = Instant::now();
        let back = start + QUIET + Duration::from_secs(5);
        let actions = watch(
            &mut Recovery::default(),
            start,
            Duration::from_secs(20),
            |now| now <= start + STEP + QUIET || now >= back,
            |_| Some(start),
        );
        assert_eq!(
            actions,
            [
                (Action::Renegotiate, STEP + QUIET),
                (Action::Retrain, back - start + QUIET)
            ]
        );
    }

    #[test]
    fn renegotiates_again_once_sound_came_back_in_between() {
        let start = Instant::now();
        let sound_again = start + QUIET + Duration::from_secs(1);
        let actions = watch(
            &mut Recovery::default(),
            start,
            Duration::from_secs(10),
            |_| true,
            |now| {
                Some(if now >= sound_again {
                    sound_again
                } else {
                    start
                })
            },
        );
        assert_eq!(
            actions,
            [
                (Action::Renegotiate, STEP + QUIET),
                (Action::Renegotiate, sound_again - start + QUIET),
                (Action::Retrain, sound_again - start + 2 * QUIET)
            ]
        );
    }

    #[test]
    fn never_acts_while_sound_comes() {
        let start = Instant::now();
        let actions = watch(
            &mut Recovery::default(),
            start,
            Duration::from_secs(20),
            |_| true,
            Some,
        );
        assert!(actions.is_empty());
    }
}
