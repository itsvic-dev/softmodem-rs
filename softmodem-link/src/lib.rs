// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.42 error correction over a data pump's bits, with plain start-stop
//! characters as V.14 carries them when the far end has no V.42. No IO.

pub mod detect;
pub mod frame;
pub mod lapm;
pub mod v42bis;
pub mod xid;

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use softmodem_dsp::hdlc::{self, Deframer};
use softmodem_dsp::pump::Role;
use softmodem_dsp::uart::{self, Decoder};

use crate::detect::{Adp, Heard};
use crate::lapm::{DEFAULT_N401, Lapm};
use crate::v42bis::Directions;

/// The detection phase timer (V.42 § 9.1.1).
const T400: Duration = Duration::from_millis(750);
// Longer, as the answerer starts before the caller's carrier detect is on.
const ANSWERER_T400: Duration = Duration::from_millis(1500);
// At least ten, and more until flags are heard (V.42 § III.1).
const ADP_REPEATS: usize = 10;
const OPENING_FLAGS: usize = 16;
const FLAGS_HEARD: usize = 3;
// V.42 runs where V.14 does, so not over V.21 at 300 bit/s.
const LOWEST_RATE: u32 = 1200;
// Address and one control octet, the shortest a frame can be (§ 8.1.3).
const MIN_FRAME: usize = 2;

/// How a call tries V.42, as V.250's `+ES` sets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Setup {
    /// Whether to try LAPM at all.
    pub lapm: bool,
    /// Whether an originator starts with the detection phase, rather than
    /// with LAPM's flags.
    pub detection: bool,
    /// Whether to end the call, rather than fall back to plain characters,
    /// when there is no V.42, even if it was not tried.
    pub required: bool,
    /// V.42 bis over LAPM, as V.250's `+DS` sets it.
    pub compression: Option<CompressionSetup>,
}

/// The V.42 bis a call asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionSetup {
    pub offer: Directions,
    /// Whether to end the call unless the far end agrees to all of `offer`.
    pub required: bool,
}

impl Setup {
    /// Plain start-stop characters only.
    pub const NORMAL: Self = Self {
        lapm: false,
        detection: false,
        required: false,
        compression: None,
    };

    fn offer(self) -> Option<Directions> {
        self.compression.map(|c| c.offer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Finding out what the far end has. The DTE should not send yet.
    Starting,
    /// An error-corrected connection with LAPM.
    Reliable,
    /// Plain start-stop characters.
    Normal,
    /// The call should end: V.42 was required and failed, or LAPM released
    /// the connection.
    Released,
}

#[derive(Debug)]
enum Phase {
    Idle,
    /// An originator sending the ODP until the deadline.
    Originating(Instant),
    /// An answerer sending mark and listening for the ODP or flags.
    Listening(Instant),
    Protocol(Lapm),
    Normal,
    Released,
}

/// Carries the DTE's data over one call's bits.
#[derive(Debug)]
pub struct Link {
    role: Role,
    setup: Setup,
    phase: Phase,
    t401: Duration,
    decoder: Decoder,
    heard: Heard,
    deframer: Deframer,
    tx: VecDeque<bool>,
    /// Characters heard while starting, for the DTE if the link falls back.
    held: Vec<u8>,
    received: Vec<u8>,
    /// ADPs sent, while the answerer still sends them.
    adps: usize,
    flags_heard: bool,
    /// V.42 bis, set up once LAPM is connected.
    codec: Option<Codec>,
}

#[derive(Debug)]
struct Codec {
    encoder: Option<v42bis::Encoder>,
    decoder: Option<v42bis::Decoder>,
}

impl Link {
    /// A link that hears start-stop characters with `decoder`, as the
    /// modulation carries them.
    #[must_use]
    pub fn new(role: Role, setup: Setup, decoder: Decoder) -> Self {
        Self {
            role,
            setup,
            phase: Phase::Idle,
            t401: Duration::ZERO,
            decoder,
            heard: Heard::default(),
            deframer: Deframer::new(MIN_FRAME),
            tx: VecDeque::new(),
            held: Vec::new(),
            received: Vec::new(),
            adps: 0,
            flags_heard: false,
            codec: None,
        }
    }

    /// Starts the link once the data pump is connected at `bit_rate`.
    pub fn start(&mut self, bit_rate: u32, now: Instant) {
        if !matches!(self.phase, Phase::Idle) {
            return;
        }
        if bit_rate < LOWEST_RATE {
            self.setup.lapm = false;
        }
        // V.42 Appendix IV: the far end may be sending a whole frame of its own.
        let frame_bits = (u32::from(DEFAULT_N401) + 8) * 10;
        self.t401 = Duration::from_secs(1)
            + Duration::from_secs_f64(2.0 * f64::from(frame_bits) / f64::from(bit_rate.max(1)));
        self.phase = match (self.setup, self.role) {
            (Setup { lapm: false, .. }, _) => self.fall_back(),
            (
                Setup {
                    detection: true, ..
                },
                Role::Originate,
            ) => Phase::Originating(now + T400),
            (_, Role::Originate) => self.protocol(Lapm::originate(self.t401, self.setup.offer())),
            (_, Role::Answer) => Phase::Listening(now + ANSWERER_T400),
        };
    }

    #[must_use]
    pub fn status(&self) -> Status {
        match &self.phase {
            Phase::Idle | Phase::Originating(_) | Phase::Listening(_) => Status::Starting,
            Phase::Protocol(lapm) => match lapm.status() {
                lapm::Status::Establishing => Status::Starting,
                lapm::Status::Connected => Status::Reliable,
                lapm::Status::Released => Status::Released,
            },
            Phase::Normal => Status::Normal,
            Phase::Released => Status::Released,
        }
    }

    /// The V.42 bis in use, in this end's directions.
    #[must_use]
    pub fn compression(&self) -> Option<Directions> {
        match &self.phase {
            Phase::Protocol(lapm) => lapm.compression(),
            _ => None,
        }
    }

    /// Queues data from the DTE.
    pub fn send(&mut self, bytes: &[u8]) {
        self.set_up_codec();
        match &mut self.phase {
            Phase::Protocol(lapm) => match self.codec.as_mut().and_then(|c| c.encoder.as_mut()) {
                Some(encoder) => lapm.send(&encoder.encode(bytes)),
                None => lapm.send(bytes),
            },
            Phase::Normal => self.tx.extend(bytes.iter().flat_map(|&b| uart::frame(b))),
            _ => {}
        }
    }

    /// Octets from the DTE that have not started on the line.
    #[must_use]
    pub fn queued(&self) -> usize {
        match &self.phase {
            Phase::Protocol(lapm) => lapm.queued(),
            Phase::Normal => self.tx.len().div_ceil(10),
            _ => 0,
        }
    }

    /// The octets an I frame carries, or 1 without LAPM.
    #[must_use]
    pub fn frame_size(&self) -> usize {
        match &self.phase {
            Phase::Protocol(lapm) => usize::from(lapm.n401()),
            _ => 1,
        }
    }

    /// Data for the DTE.
    pub fn take_received(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.received)
    }

    /// Drops a partly received character, for when carrier is lost.
    pub fn carrier_lost(&mut self) {
        self.decoder.reset();
    }

    /// Up to `max` bits to send. Fewer mean the line idles on mark.
    pub fn transmit(&mut self, max: usize, now: Instant) -> Vec<bool> {
        self.expire(now);
        while self.tx.len() < max {
            match &mut self.phase {
                Phase::Originating(deadline) if now < *deadline => {
                    detect::push_odp(&mut self.tx);
                }
                Phase::Protocol(_)
                    if self.adps > 0 && (self.adps < ADP_REPEATS || !self.flags_heard) =>
                {
                    detect::push_adp(&mut self.tx, Adp::Lapm);
                    self.adps += 1;
                }
                Phase::Protocol(lapm) => {
                    if self.adps > 0 {
                        self.adps = 0;
                        hdlc::push_flag(&mut self.tx);
                    }
                    if lapm.queued() == 0
                        && let Some(encoder) = self.codec.as_mut().and_then(|c| c.encoder.as_mut())
                    {
                        lapm.send(&encoder.flush());
                    }
                    match lapm.next_frame(now) {
                        Some(frame) => hdlc::push_frame(&frame, &mut self.tx),
                        None => hdlc::push_flag(&mut self.tx),
                    }
                }
                _ => break,
            }
        }
        let n = max.min(self.tx.len());
        self.tx.drain(..n).collect()
    }

    /// Takes bits heard on the line.
    pub fn receive(&mut self, bits: &[bool], now: Instant) {
        self.expire(now);
        for &bit in bits {
            match &mut self.phase {
                Phase::Protocol(lapm) => {
                    if let Some(frame) = self.deframer.push(bit) {
                        self.flags_heard = true;
                        lapm.receive(&frame, now);
                    }
                    self.flags_heard |= self.deframer.flags_in_a_row() >= FLAGS_HEARD;
                }
                Phase::Normal => {
                    if let Some(byte) = self.decoder.push(bit) {
                        self.received.push(byte);
                    }
                }
                Phase::Released => {}
                Phase::Idle | Phase::Originating(_) | Phase::Listening(_) => self.detect(bit, now),
            }
        }
        self.set_up_codec();
        if let Phase::Protocol(lapm) = &mut self.phase {
            let data = lapm.take_received();
            match self.codec.as_mut().and_then(|c| c.decoder.as_mut()) {
                Some(decoder) => {
                    if decoder.decode(&data, &mut self.received).is_err() {
                        self.phase = Phase::Released;
                    }
                }
                None => self.received.extend(data),
            }
        }
        self.expire(now);
    }

    /// V.42 bis § 5.2: starts as LAPM connects, or ends a call that required it.
    fn set_up_codec(&mut self) {
        let Phase::Protocol(lapm) = &self.phase else {
            return;
        };
        if self.codec.is_some() || lapm.status() != lapm::Status::Connected {
            return;
        }
        let agreed = lapm.compression();
        let directions = agreed.map(|a| (a.transmit, a.receive));
        if let Some(wanted) = self.setup.compression
            && wanted.required
            && directions != Some((wanted.offer.transmit, wanted.offer.receive))
        {
            self.phase = Phase::Released;
            return;
        }
        self.codec = Some(Codec {
            encoder: agreed
                .filter(|a| a.transmit)
                .map(|a| v42bis::Encoder::new(a.parameters)),
            decoder: agreed
                .filter(|a| a.receive)
                .map(|a| v42bis::Decoder::new(a.parameters)),
        });
    }

    fn detect(&mut self, bit: bool, now: Instant) {
        let frame = self.deframer.push(bit);
        if let Some(byte) = self.decoder.push(bit) {
            self.held.push(byte);
            self.heard.push(byte);
        }
        match self.phase {
            Phase::Originating(_) => match self.heard.adp() {
                Some(Adp::Lapm) => {
                    let lapm = Lapm::originate(self.t401, self.setup.offer());
                    self.phase = self.protocol(lapm);
                }
                Some(Adp::NoErrorCorrection) => {
                    self.held.clear();
                    self.phase = self.fall_back();
                }
                None => {}
            },
            Phase::Listening(_) if self.heard.odp() => {
                self.phase = self.protocol(Lapm::answer(self.t401, now, self.setup.offer()));
                self.adps = 1;
                detect::push_adp(&mut self.tx, Adp::Lapm);
            }
            Phase::Listening(_)
                if frame.is_some() || self.deframer.flags_in_a_row() >= FLAGS_HEARD =>
            {
                let mut lapm = Lapm::answer(self.t401, now, self.setup.offer());
                if let Some(frame) = frame {
                    lapm.receive(&frame, now);
                }
                self.phase = self.protocol(lapm);
                hdlc::push_flag(&mut self.tx);
            }
            _ => {}
        }
    }

    fn protocol(&mut self, lapm: Lapm) -> Phase {
        self.held.clear();
        if self.role == Role::Originate {
            for _ in 0..OPENING_FLAGS {
                hdlc::push_flag(&mut self.tx);
            }
        }
        Phase::Protocol(lapm)
    }

    fn fall_back(&mut self) -> Phase {
        if self.setup.required {
            return Phase::Released;
        }
        self.received.append(&mut self.held);
        Phase::Normal
    }

    fn expire(&mut self, now: Instant) {
        match &self.phase {
            Phase::Originating(deadline) | Phase::Listening(deadline) if now >= *deadline => {
                self.phase = self.fall_back();
            }
            Phase::Protocol(lapm) if lapm.status() == lapm::Status::Released => {
                self.phase = Phase::Released;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 2400;
    const STEP: Duration = Duration::from_millis(10);
    const STEP_BITS: usize = 24;

    const V42: Setup = Setup {
        lapm: true,
        detection: true,
        required: false,
        compression: None,
    };

    const BOTH_WAYS: Directions = Directions {
        transmit: true,
        receive: true,
        parameters: v42bis::Parameters {
            codewords: 2048,
            max_string: 32,
        },
    };

    fn compressing(offer: Directions, required: bool) -> Setup {
        Setup {
            compression: Some(CompressionSetup { offer, required }),
            ..V42
        }
    }

    fn text(len: usize) -> Vec<u8> {
        b"the carrier carries the data over the line to the other end\r\n"
            .iter()
            .copied()
            .cycle()
            .take(len)
            .collect()
    }

    struct Call {
        now: Instant,
        caller: Link,
        answerer: Link,
    }

    impl Call {
        fn new(caller: Setup, answerer: Setup) -> Self {
            let now = Instant::now();
            let mut call = Self {
                now,
                caller: Link::new(Role::Originate, caller, Decoder::v14()),
                answerer: Link::new(Role::Answer, answerer, Decoder::v14()),
            };
            call.answerer.start(RATE, now);
            call.caller.start(RATE, now);
            call
        }

        fn run(&mut self, time: Duration) {
            let end = self.now + time;
            while self.now < end {
                self.now += STEP;
                let bits = line(self.caller.transmit(STEP_BITS, self.now));
                self.answerer.receive(&bits, self.now);
                let bits = line(self.answerer.transmit(STEP_BITS, self.now));
                self.caller.receive(&bits, self.now);
            }
        }

        fn statuses(&self) -> (Status, Status) {
            (self.caller.status(), self.answerer.status())
        }
    }

    fn line(mut bits: Vec<bool>) -> Vec<bool> {
        bits.resize(STEP_BITS, true);
        bits
    }

    #[test]
    fn two_v42_ends_detect_each_other_and_carry_data() {
        let mut call = Call::new(V42, V42);
        call.run(Duration::from_secs(1));
        assert_eq!(call.statuses(), (Status::Reliable, Status::Reliable));
        let data: Vec<u8> = (0..=255).cycle().take(2000).collect();
        call.caller.send(&data);
        call.answerer.send(b"from the answerer");
        call.run(Duration::from_secs(10));
        assert_eq!(call.answerer.take_received(), data);
        assert_eq!(call.caller.take_received(), b"from the answerer");
    }

    #[test]
    fn an_originator_without_detection_is_heard_by_its_flags() {
        let caller = Setup {
            detection: false,
            ..V42
        };
        let mut call = Call::new(caller, V42);
        call.run(Duration::from_millis(500));
        assert_eq!(call.statuses(), (Status::Reliable, Status::Reliable));
    }

    #[test]
    fn falls_back_when_the_answerer_has_no_v42() {
        let mut call = Call::new(V42, Setup::NORMAL);
        call.run(Duration::from_millis(500));
        assert_eq!(call.caller.status(), Status::Starting);
        call.run(Duration::from_millis(500));
        assert_eq!(call.statuses(), (Status::Normal, Status::Normal));
        call.caller.take_received();
        call.answerer.take_received();
        call.caller.send(b"plain");
        call.answerer.send(b"text");
        call.run(Duration::from_millis(200));
        assert_eq!(call.answerer.take_received(), b"plain");
        assert_eq!(call.caller.take_received(), b"text");
    }

    #[test]
    fn keeps_what_a_plain_originator_sent_while_it_listened() {
        let mut call = Call::new(Setup::NORMAL, V42);
        call.caller.send(b"early");
        call.run(Duration::from_millis(1000));
        assert_eq!(call.answerer.status(), Status::Starting);
        call.run(Duration::from_millis(1000));
        assert_eq!(call.statuses(), (Status::Normal, Status::Normal));
        assert_eq!(call.answerer.take_received(), b"early");
    }

    #[test]
    fn ends_the_call_when_v42_is_required_and_missing() {
        let required = Setup {
            required: true,
            ..V42
        };
        let mut call = Call::new(required, Setup::NORMAL);
        call.run(Duration::from_secs(1));
        assert_eq!(call.caller.status(), Status::Released);
    }

    #[test]
    fn stays_plain_at_300_bit_s() {
        let mut link = Link::new(Role::Originate, V42, Decoder::new());
        link.start(300, Instant::now());
        assert_eq!(link.status(), Status::Normal);
    }

    #[test]
    fn ends_the_call_at_once_when_v42_is_required_and_cannot_be_tried() {
        let setup = Setup {
            lapm: false,
            required: true,
            ..V42
        };
        let mut link = Link::new(Role::Answer, setup, Decoder::v14());
        link.start(RATE, Instant::now());
        assert_eq!(link.status(), Status::Released);
    }

    #[test]
    fn compresses_both_ways_when_both_ends_offer_it() {
        let setup = compressing(BOTH_WAYS, false);
        let mut call = Call::new(setup, setup);
        call.run(Duration::from_secs(1));
        assert_eq!(call.caller.compression(), Some(BOTH_WAYS));
        assert_eq!(call.answerer.compression(), Some(BOTH_WAYS));
        let data = text(20_000);
        call.caller.send(&data);
        call.answerer.send(&data[..5000]);
        call.run(Duration::from_secs(40));
        assert_eq!(call.answerer.take_received(), data);
        assert_eq!(call.caller.take_received(), &data[..5000]);
    }

    #[test]
    fn short_writes_are_flushed_and_arrive() {
        let setup = compressing(BOTH_WAYS, false);
        let mut call = Call::new(setup, setup);
        call.run(Duration::from_secs(1));
        let data = text(3000);
        for chunk in data.chunks(50) {
            call.caller.send(chunk);
            call.run(Duration::from_millis(100));
        }
        call.run(Duration::from_secs(2));
        assert_eq!(call.answerer.take_received(), data);
    }

    #[test]
    fn compression_agreed_one_way_runs_one_way() {
        let receive_only = Directions {
            transmit: false,
            ..BOTH_WAYS
        };
        let mut call = Call::new(
            compressing(BOTH_WAYS, false),
            compressing(receive_only, false),
        );
        call.run(Duration::from_secs(1));
        let caller = call.caller.compression().unwrap();
        assert!(caller.transmit && !caller.receive);
        let data = text(5000);
        call.caller.send(&data);
        call.answerer.send(&data);
        call.run(Duration::from_secs(30));
        assert_eq!(call.answerer.take_received(), data);
        assert_eq!(call.caller.take_received(), data);
    }

    #[test]
    fn a_far_end_without_v42bis_leaves_plain_lapm_or_ends_a_call_that_required_it() {
        let mut call = Call::new(compressing(BOTH_WAYS, false), V42);
        call.run(Duration::from_secs(1));
        assert_eq!(call.statuses(), (Status::Reliable, Status::Reliable));
        assert_eq!(call.caller.compression(), None);
        let mut call = Call::new(compressing(BOTH_WAYS, true), V42);
        call.run(Duration::from_secs(1));
        assert_eq!(call.caller.status(), Status::Released);
    }
}
