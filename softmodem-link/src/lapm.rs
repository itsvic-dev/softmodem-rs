//! The LAPM procedures of V.42 § 8 over whole frames: negotiation with XID,
//! establishment, numbered data transfer with REJ and timer recovery, and
//! release. Selective reject, TEST and the 32-bit FCS are never agreed.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use softmodem_dsp::pump::Role;

use crate::frame::{Control, Frame, Rejected, Supervisory, Unnumbered};
use crate::v42bis::Directions;
use crate::xid::Xid;

pub const DEFAULT_N401: u16 = 128;
pub const DEFAULT_K: u8 = 15;
const MAX_N401: u16 = 2048;
const MAX_K: u8 = 127;
const MODULUS: u8 = 128;
/// Retries before giving up (§ 9.2.2).
pub const N400: u8 = 10;

const BRK: u8 = 0x40;
const BRKACK: u8 = 0x60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Establishing,
    Connected,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// An answerer waiting for XID or SABME until the deadline.
    Waiting(Instant),
    /// An originator whose XID command is unanswered.
    Negotiating,
    /// An originator whose SABME is unanswered.
    Establishing,
    Connected,
    Released,
}

/// The exception conditions of § 8.5 and the peer-receiver busy condition.
#[derive(Debug, Default)]
struct Conditions {
    peer_busy: bool,
    reject: bool,
    recovery: bool,
}

/// Supervisory frames owed to the far end.
#[derive(Debug, Default)]
struct Owed {
    /// REJ, with its F bit.
    reject: Option<bool>,
    /// RR with the F bit set, answering a poll.
    final_rr: bool,
    /// RR with the P bit set, in timer recovery.
    poll: bool,
    /// RR acknowledging an I frame.
    ack: bool,
}

/// One end of an error-corrected connection on DLCI 0.
#[derive(Debug)]
pub struct Lapm {
    role: Role,
    state: State,
    t401: Duration,
    timer: Option<Instant>,
    retries: u8,
    n401_tx: u16,
    k_tx: u8,
    vs: u8,
    va: u8,
    vr: u8,
    rb: bool,
    /// I frames from N(S) = V(A) on, sent but not acknowledged.
    sent: VecDeque<Vec<u8>>,
    queue: VecDeque<u8>,
    received: Vec<u8>,
    conditions: Conditions,
    /// Unnumbered frames to send first, each with whether it starts T401.
    urgent: VecDeque<(Frame, bool)>,
    owed: Owed,
    /// The V.42 bis this end will run, in its own directions.
    offer: Option<Directions>,
    compression: Option<Directions>,
}

impl Lapm {
    /// An originator, which negotiates with XID, asking for `offer`, and
    /// then sends SABME.
    #[must_use]
    pub fn originate(t401: Duration, offer: Option<Directions>) -> Self {
        let mut lapm = Self::new(Role::Originate, State::Negotiating, t401, offer);
        lapm.send_xid_command();
        lapm
    }

    /// An answerer, which agrees to what it can of `offer`, and gives up if
    /// the originator has not established the connection within `N400`
    /// times `t401`.
    #[must_use]
    pub fn answer(t401: Duration, now: Instant, offer: Option<Directions>) -> Self {
        let deadline = now + t401 * u32::from(N400);
        Self::new(Role::Answer, State::Waiting(deadline), t401, offer)
    }

    fn new(role: Role, state: State, t401: Duration, offer: Option<Directions>) -> Self {
        Self {
            offer,
            compression: None,
            role,
            state,
            t401,
            timer: None,
            retries: 0,
            n401_tx: DEFAULT_N401,
            k_tx: DEFAULT_K,
            vs: 0,
            va: 0,
            vr: 0,
            rb: false,
            sent: VecDeque::new(),
            queue: VecDeque::new(),
            received: Vec::new(),
            conditions: Conditions::default(),
            urgent: VecDeque::new(),
            owed: Owed::default(),
        }
    }

    #[must_use]
    pub fn status(&self) -> Status {
        match self.state {
            State::Waiting(_) | State::Negotiating | State::Establishing => Status::Establishing,
            State::Connected => Status::Connected,
            State::Released => Status::Released,
        }
    }

    /// The V.42 bis the two ends agreed on, in this end's directions.
    #[must_use]
    pub fn compression(&self) -> Option<Directions> {
        self.compression
    }

    /// The largest information field this end sends.
    #[must_use]
    pub fn n401(&self) -> u16 {
        self.n401_tx
    }

    /// Queues data from the DTE.
    pub fn send(&mut self, bytes: &[u8]) {
        if self.state != State::Released {
            self.queue.extend(bytes);
        }
    }

    /// Octets queued that no I frame has carried yet.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Data for the DTE, in order.
    pub fn take_received(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.received)
    }

    /// The next frame to send, without its FCS, or `None` for flags.
    pub fn next_frame(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.expire(now);
        if let Some((frame, timed)) = self.urgent.pop_front() {
            if timed {
                self.timer = Some(now + self.t401);
            }
            return Some(frame.bytes());
        }
        if self.state != State::Connected {
            return None;
        }
        if let Some(final_bit) = self.owed.reject.take() {
            self.owed.final_rr &= !final_bit;
            self.owed.ack = false;
            return Some(self.supervisory(false, Supervisory::Rej, final_bit).bytes());
        }
        if std::mem::take(&mut self.owed.final_rr) {
            self.owed.ack = false;
            return Some(self.supervisory(false, Supervisory::Rr, true).bytes());
        }
        if std::mem::take(&mut self.owed.poll) {
            self.owed.ack = false;
            self.timer = Some(now + self.t401);
            return Some(self.supervisory(true, Supervisory::Rr, true).bytes());
        }
        if let Some(frame) = self.next_i_frame(now) {
            return Some(frame);
        }
        if std::mem::take(&mut self.owed.ack) {
            return Some(self.supervisory(false, Supervisory::Rr, false).bytes());
        }
        None
    }

    /// Takes a frame that passed its FCS check.
    pub fn receive(&mut self, bytes: &[u8], now: Instant) {
        self.expire(now);
        let frame = match Frame::parse(bytes) {
            Ok(frame) => frame,
            Err(Rejected::Undefined) if self.state == State::Connected => {
                return self.release();
            }
            Err(_) => return,
        };
        let command = frame.cr == (self.role == Role::Answer);
        match frame.control {
            Control::U { kind, pf } => self.unnumbered(kind, pf, command, &frame.info),
            Control::S { .. } | Control::I { .. } => {
                if self.state == State::Establishing {
                    // § 8.3.2.1: the UA was lost, and the far end is connected.
                    self.connect();
                }
                if self.state != State::Connected {
                    return;
                }
                match frame.control {
                    Control::I { ns, nr, poll } if command => {
                        self.information(ns, nr, poll, frame.info, now);
                    }
                    Control::S { kind, nr, pf } => self.supervisory_in(kind, nr, pf, command, now),
                    _ => {}
                }
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        if let State::Waiting(deadline) = self.state
            && now >= deadline
        {
            self.release();
            return;
        }
        if self.timer.is_none_or(|t| now < t) {
            return;
        }
        self.timer = None;
        match self.state {
            State::Negotiating | State::Establishing => {
                self.retries += 1;
                if self.retries >= N400 {
                    return self.release();
                }
                if self.state == State::Negotiating {
                    self.send_xid_command();
                } else {
                    self.send_sabme();
                }
            }
            State::Connected => {
                if self.conditions.recovery {
                    self.retries += 1;
                } else {
                    self.conditions.recovery = true;
                    self.retries = 0;
                }
                if self.retries >= N400 {
                    return self.release();
                }
                self.owed.poll = true;
            }
            State::Waiting(_) | State::Released => {}
        }
    }

    fn release(&mut self) {
        self.state = State::Released;
        self.timer = None;
        self.queue.clear();
        self.sent.clear();
    }

    fn cr(&self, command: bool) -> bool {
        command == (self.role == Role::Originate)
    }

    fn unnumbered_frame(&self, command: bool, kind: Unnumbered, pf: bool) -> Frame {
        Frame::new(self.cr(command), Control::U { kind, pf })
    }

    fn supervisory(&self, command: bool, kind: Supervisory, pf: bool) -> Frame {
        let nr = self.vr;
        Frame::new(self.cr(command), Control::S { kind, nr, pf })
    }

    fn send_xid_command(&mut self) {
        let proposal = Xid {
            n401_tx: Some(DEFAULT_N401),
            n401_rx: Some(DEFAULT_N401),
            k_tx: Some(DEFAULT_K),
            k_rx: Some(DEFAULT_K),
            compression: self.offer.map(Directions::proposal),
            ..Xid::default()
        };
        let frame = self
            .unnumbered_frame(true, Unnumbered::Xid, false)
            .with_info(proposal.bytes());
        self.urgent.push_back((frame, true));
    }

    fn send_sabme(&mut self) {
        let frame = self.unnumbered_frame(true, Unnumbered::Sabme, true);
        self.urgent.push_back((frame, true));
    }

    fn respond(&mut self, kind: Unnumbered, pf: bool, info: Vec<u8>) {
        let frame = self.unnumbered_frame(false, kind, pf).with_info(info);
        self.urgent.push_back((frame, false));
    }

    fn connect(&mut self) {
        self.state = State::Connected;
        self.timer = None;
        self.retries = 0;
        self.vs = 0;
        self.va = 0;
        self.vr = 0;
        self.sent.clear();
        self.conditions = Conditions::default();
    }

    fn unnumbered(&mut self, kind: Unnumbered, pf: bool, command: bool, info: &[u8]) {
        match (kind, command, self.state) {
            (_, _, State::Released) => {}
            (Unnumbered::Xid, true, _) => {
                if let Some(theirs) = Xid::parse(info) {
                    match self.agree(&theirs) {
                        Some(ours) => self.respond(Unnumbered::Xid, false, ours.bytes()),
                        None => self.release(),
                    }
                }
            }
            (Unnumbered::Xid, false, State::Negotiating) => {
                if let Some(theirs) = Xid::parse(info) {
                    let Some(offer) = self
                        .offer
                        .map_or(Ok(None), |offer| offer.conclude(theirs.compression))
                        .ok()
                    else {
                        return self.release();
                    };
                    self.compression = offer;
                    self.n401_tx = theirs
                        .n401_rx
                        .map_or(DEFAULT_N401, |n| n.clamp(1, MAX_N401));
                    self.k_tx = theirs.k_rx.map_or(DEFAULT_K, |k| k.clamp(1, MAX_K));
                    self.state = State::Establishing;
                    self.retries = 0;
                    self.timer = None;
                    self.send_sabme();
                }
            }
            (Unnumbered::Sabme, true, State::Waiting(_)) => {
                self.connect();
                self.respond(Unnumbered::Ua, pf, Vec::new());
            }
            (Unnumbered::Sabme, true, State::Connected) => {
                let fresh = self.vs == 0 && self.va == 0 && self.vr == 0 && self.sent.is_empty();
                if fresh {
                    self.respond(Unnumbered::Ua, pf, Vec::new());
                } else {
                    self.release();
                }
            }
            (Unnumbered::Ua, false, State::Establishing) if pf => self.connect(),
            (Unnumbered::Dm, false, State::Establishing) if pf => self.release(),
            (Unnumbered::Disc, true, State::Connected) => {
                self.respond(Unnumbered::Ua, pf, Vec::new());
                self.release();
            }
            (Unnumbered::Disc, true, _) => self.respond(Unnumbered::Dm, pf, Vec::new()),
            (Unnumbered::Dm, false, State::Connected) if !pf || self.conditions.recovery => {
                self.release();
            }
            (Unnumbered::Frmr, false, State::Connected) => self.release(),
            (Unnumbered::Ui, true, State::Connected) => self.break_signal(pf, info),
            _ => {}
        }
    }

    /// The XID response, or `None` for bad V.42 bis values (§ 9.2.3, § 9.2.4).
    fn agree(&mut self, theirs: &Xid) -> Option<Xid> {
        let n401 = |n: Option<u16>| n.map_or(DEFAULT_N401, |n| n.clamp(1, MAX_N401));
        let k = |k: Option<u8>| k.map_or(DEFAULT_K, |k| k.clamp(1, MAX_K));
        let mut compression = None;
        // V.42 bis § 5.1: the parameters hold for the whole connection.
        if self.state != State::Connected
            && let Some(proposal) = theirs.compression
        {
            let (reply, agreed) = Directions::answer(self.offer, proposal).ok()?;
            self.compression = agreed;
            compression = Some(reply);
        }
        self.n401_tx = n401(theirs.n401_rx);
        self.k_tx = k(theirs.k_rx);
        Some(Xid {
            functions: 0,
            n401_tx: Some(self.n401_tx),
            n401_rx: Some(n401(theirs.n401_tx)),
            k_tx: Some(self.k_tx),
            k_rx: Some(k(theirs.k_tx)),
            compression,
        })
    }

    /// Acknowledges a BRK (§ 8.13.3.2) without passing the break to the DTE.
    fn break_signal(&mut self, pf: bool, info: &[u8]) {
        let Some(&first) = info.first() else {
            return;
        };
        if first & 0x7f != BRK {
            return;
        }
        if (first & 0x80 != 0) == self.rb {
            self.rb = !self.rb;
        }
        self.respond(Unnumbered::Ui, pf, vec![BRKACK | u8::from(self.rb) << 7]);
    }

    fn information(&mut self, ns: u8, nr: u8, poll: bool, info: Vec<u8>, now: Instant) {
        if !self.acknowledge(nr, now) {
            return self.release();
        }
        if ns == self.vr {
            self.received.extend(info);
            self.vr = (self.vr + 1) % MODULUS;
            self.conditions.reject = false;
            if poll {
                self.owed.final_rr = true;
            } else {
                self.owed.ack = true;
            }
        } else if !self.conditions.reject {
            self.conditions.reject = true;
            self.owed.reject = Some(poll);
        } else if poll {
            self.owed.final_rr = true;
        }
    }

    fn supervisory_in(&mut self, kind: Supervisory, nr: u8, pf: bool, command: bool, now: Instant) {
        if kind == Supervisory::Srej {
            return self.release();
        }
        // Table 9 ignores an unsolicited F=1, but spandsp acknowledges with one.
        if !command && pf && self.conditions.recovery {
            if !self.acknowledge(nr, now) {
                return self.release();
            }
            self.conditions.recovery = false;
            self.vs = nr;
            self.conditions.peer_busy = kind == Supervisory::Rnr;
            self.timer = self.conditions.peer_busy.then(|| now + self.t401);
            return;
        }
        if !self.acknowledge(nr, now) {
            return self.release();
        }
        if command && pf {
            self.owed.final_rr = true;
        }
        match kind {
            Supervisory::Rnr => {
                self.conditions.peer_busy = true;
                if !self.conditions.recovery {
                    self.timer = Some(now + self.t401);
                }
            }
            Supervisory::Rej if !self.conditions.recovery => {
                self.conditions.peer_busy = false;
                self.vs = nr;
                self.timer = None;
            }
            _ => self.conditions.peer_busy = false,
        }
    }

    /// Takes N(R) as an acknowledgement, if valid (§ 8.4.3.2, § 8.5.2).
    fn acknowledge(&mut self, nr: u8, now: Instant) -> bool {
        let acked = usize::from(nr.wrapping_sub(self.va) % MODULUS);
        if acked > self.sent.len() {
            return false;
        }
        let in_flight = usize::from(self.vs.wrapping_sub(self.va) % MODULUS);
        self.sent.drain(..acked);
        self.va = nr;
        if in_flight < acked {
            self.vs = nr;
        }
        if acked > 0 && !self.conditions.recovery {
            let outstanding = self.vs != self.va;
            self.timer = outstanding.then(|| now + self.t401);
        }
        true
    }

    fn next_i_frame(&mut self, now: Instant) -> Option<Vec<u8>> {
        if self.conditions.peer_busy || self.conditions.recovery {
            return None;
        }
        let index = usize::from(self.vs.wrapping_sub(self.va) % MODULUS);
        if index == self.sent.len() {
            if self.sent.len() >= usize::from(self.k_tx) || self.queue.is_empty() {
                return None;
            }
            let n = self.queue.len().min(usize::from(self.n401_tx));
            self.sent.push_back(self.queue.drain(..n).collect());
        }
        let control = Control::I {
            ns: self.vs,
            nr: self.vr,
            poll: false,
        };
        let frame = Frame::new(self.cr(true), control).with_info(self.sent[index].clone());
        self.vs = (self.vs + 1) % MODULUS;
        self.owed.ack = false;
        if self.timer.is_none() {
            self.timer = Some(now + self.t401);
        }
        Some(frame.bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T401: Duration = Duration::from_secs(1);
    const STEP: Duration = Duration::from_millis(50);

    struct Pair {
        now: Instant,
        caller: Lapm,
        answerer: Lapm,
    }

    impl Pair {
        fn new() -> Self {
            let now = Instant::now();
            Self {
                now,
                caller: Lapm::originate(T401, None),
                answerer: Lapm::answer(T401, now, None),
            }
        }

        /// Each step, each end sends at most one frame, which `lose` may drop.
        fn run(&mut self, steps: usize, mut lose: impl FnMut(&[u8]) -> bool) {
            for _ in 0..steps {
                self.now += STEP;
                if let Some(frame) = self.caller.next_frame(self.now)
                    && !lose(&frame)
                {
                    self.answerer.receive(&frame, self.now);
                }
                if let Some(frame) = self.answerer.next_frame(self.now)
                    && !lose(&frame)
                {
                    self.caller.receive(&frame, self.now);
                }
            }
        }

        fn connected() -> Self {
            let mut pair = Self::new();
            pair.run(10, |_| false);
            assert_eq!(pair.caller.status(), Status::Connected);
            assert_eq!(pair.answerer.status(), Status::Connected);
            pair
        }
    }

    fn text(len: usize) -> Vec<u8> {
        (0..len).map(|i| b"0123456789abcdef"[i % 16]).collect()
    }

    fn is_i_frame(frame: &[u8]) -> bool {
        frame[1] & 1 == 0
    }

    #[test]
    fn negotiates_establishes_and_carries_data_both_ways() {
        let mut pair = Pair::connected();
        let data = text(3000);
        pair.caller.send(&data);
        pair.answerer.send(b"hello");
        pair.run(200, |_| false);
        assert_eq!(pair.answerer.take_received(), data);
        assert_eq!(pair.caller.take_received(), b"hello");
        assert!(pair.caller.sent.is_empty());
    }

    #[test]
    fn sends_no_more_than_n401_octets_in_a_frame() {
        let mut pair = Pair::connected();
        pair.caller.send(&text(1000));
        let mut longest = 0;
        pair.run(100, |frame| {
            longest = longest.max(frame.len());
            false
        });
        assert_eq!(longest, 3 + usize::from(DEFAULT_N401));
    }

    #[test]
    fn a_lost_i_frame_is_sent_again_after_a_reject() {
        let mut pair = Pair::connected();
        let data = text(2000);
        pair.caller.send(&data);
        let mut i_frames = 0;
        let mut rejects = 0;
        pair.run(200, |frame| {
            rejects += usize::from(frame[..2] == [0x03, 0x09]);
            if frame[0] == 0x03 && is_i_frame(frame) {
                i_frames += 1;
                return i_frames == 3;
            }
            false
        });
        assert_eq!(pair.answerer.take_received(), data);
        assert_eq!(rejects, 1);
    }

    #[test]
    fn a_lost_last_frame_is_recovered_by_polling() {
        let mut pair = Pair::connected();
        pair.caller.send(b"only frame");
        let mut first = true;
        pair.run(100, |frame| is_i_frame(frame) && std::mem::take(&mut first));
        assert_eq!(pair.answerer.take_received(), b"only frame");
        assert!(!pair.caller.conditions.recovery);
    }

    #[test]
    fn lost_acknowledgements_are_recovered_by_polling() {
        let mut pair = Pair::connected();
        pair.caller.send(&text(500));
        let mut lost = 0;
        pair.run(300, |frame| {
            let rr = frame[0] == 0x03 && frame[1] == 0x01 && frame[2] & 1 == 0;
            if rr && lost < 5 {
                lost += 1;
                return true;
            }
            false
        });
        assert_eq!(pair.answerer.take_received(), text(500));
        assert!(pair.caller.sent.is_empty());
        assert_eq!(pair.caller.status(), Status::Connected);
    }

    #[test]
    fn keeps_to_the_window() {
        let mut pair = Pair::connected();
        pair.caller.send(&text(128 * 40));
        pair.run(40, |frame| frame[0] == 0x03 && !is_i_frame(frame));
        assert_eq!(pair.caller.sent.len(), usize::from(DEFAULT_K));
    }

    #[test]
    fn a_repeated_sabme_after_a_lost_ua_is_answered() {
        let mut pair = Pair::new();
        let mut lost = false;
        pair.run(60, |frame| {
            let ua = frame == [0x03, 0x73];
            ua && !std::mem::replace(&mut lost, true)
        });
        assert_eq!(pair.caller.status(), Status::Connected);
        assert_eq!(pair.answerer.status(), Status::Connected);
    }

    #[test]
    fn the_originator_gives_up_on_a_silent_answerer() {
        let mut pair = Pair::new();
        pair.run(400, |_| true);
        assert_eq!(pair.caller.status(), Status::Released);
        assert_eq!(pair.answerer.status(), Status::Released);
    }

    #[test]
    fn disc_releases_the_connection() {
        let mut pair = Pair::connected();
        pair.answerer.receive(&[0x03, 0x53], pair.now);
        assert_eq!(pair.answerer.status(), Status::Released);
        assert_eq!(pair.answerer.next_frame(pair.now), Some(vec![0x03, 0x73]));
    }

    #[test]
    fn the_answerer_takes_the_originators_smaller_frames() {
        let now = Instant::now();
        let mut answerer = Lapm::answer(T401, now, None);
        let theirs = Xid {
            n401_tx: Some(64),
            n401_rx: Some(32),
            k_tx: Some(4),
            k_rx: Some(2),
            ..Xid::default()
        };
        let mut xid = vec![0x03, 0xaf];
        xid.extend(theirs.bytes());
        answerer.receive(&xid, now);
        let reply = answerer.next_frame(now).unwrap();
        assert_eq!(reply[..2], [0x03, 0xaf]);
        assert_eq!(
            Xid::parse(&reply[2..]),
            Some(Xid {
                n401_tx: Some(32),
                n401_rx: Some(64),
                k_tx: Some(2),
                k_rx: Some(4),
                ..Xid::default()
            })
        );
        assert_eq!(answerer.n401(), 32);
    }

    #[test]
    fn a_break_is_acknowledged_once_per_sequence_number() {
        let mut pair = Pair::connected();
        let brk = [0x03, 0x03, BRK, 0x40];
        pair.answerer.receive(&brk, pair.now);
        assert_eq!(
            pair.answerer.next_frame(pair.now),
            Some(vec![0x03, 0x03, 0xe0])
        );
        pair.answerer.receive(&brk, pair.now);
        assert_eq!(
            pair.answerer.next_frame(pair.now),
            Some(vec![0x03, 0x03, 0xe0])
        );
    }

    #[test]
    fn an_unsolicited_final_rr_still_acknowledges() {
        let mut pair = Pair::connected();
        pair.caller.send(b"one frame");
        let frame = pair.caller.next_frame(pair.now).unwrap();
        pair.answerer.receive(&frame, pair.now);
        let rr_final = [0x03, 0x01, 1 << 1 | 1];
        pair.caller.receive(&rr_final, pair.now);
        assert!(pair.caller.sent.is_empty());
        assert_eq!(pair.caller.timer, None);
    }

    #[test]
    fn an_undefined_frame_ends_the_connection() {
        let mut pair = Pair::connected();
        pair.answerer.receive(&[0x03, 0x2f], pair.now);
        assert_eq!(pair.answerer.status(), Status::Released);
    }
}
