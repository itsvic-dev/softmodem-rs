// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Our V.42 against spandsp's, bit for bit with no modulation under them.

use std::time::{Duration, Instant};

use softmodem_dsp::pump::Role;
use softmodem_dsp::uart::Decoder;
use softmodem_interop::{CompressionMode, V42, V42bis};
use softmodem_link::v42bis::{Directions, Parameters};
use softmodem_link::{CompressionSetup, Link, Setup, Status};

const RATE: u32 = 2400;
const STEP: Duration = Duration::from_millis(10);
const STEP_BITS: usize = 24;

const DETECTING: Setup = Setup {
    lapm: true,
    detection: true,
    required: true,
    compression: None,
};

struct Call {
    now: Instant,
    ours: Link,
    theirs: V42,
    /// Every this many bits, each way, one is flipped once data flows.
    error_every: Option<usize>,
    sent: usize,
}

impl Call {
    fn new(role: Role, setup: Setup, detect: bool) -> Self {
        let now = Instant::now();
        let mut ours = Link::new(role, setup, Decoder::v14());
        ours.start(RATE, now);
        let theirs = V42::new(role == Role::Answer, detect);
        Self {
            now,
            ours,
            theirs,
            error_every: None,
            sent: 0,
        }
    }

    fn run(&mut self, time: Duration) {
        let end = self.now + time;
        while self.now < end {
            self.now += STEP;
            let mut bits = self.ours.transmit(STEP_BITS, self.now);
            bits.resize(STEP_BITS, true);
            let mut theirs: Vec<bool> = (0..STEP_BITS).map(|_| self.theirs.tx_bit()).collect();
            for (ours, theirs) in bits.iter_mut().zip(&mut theirs) {
                self.sent += 1;
                if self
                    .error_every
                    .is_some_and(|n| self.sent.is_multiple_of(n))
                {
                    *ours = !*ours;
                    *theirs = !*theirs;
                }
            }
            for bit in bits {
                self.theirs.rx_bit(bit);
            }
            self.ours.receive(&theirs, self.now);
        }
    }

    fn carries_data_both_ways(&mut self) {
        self.run(Duration::from_secs(5));
        assert_eq!(self.ours.status(), Status::Reliable);
        assert!(self.theirs.connected(), "spandsp never connected");
        self.carries_data();
    }

    fn carries_data(&mut self) {
        let ours: Vec<u8> = (0..=255).cycle().take(3000).collect();
        let theirs: Vec<u8> = ours.iter().rev().copied().collect();
        self.ours.send(&ours);
        self.theirs.send(&theirs);
        self.run(Duration::from_secs(60));
        assert_eq!(self.theirs.received(), ours);
        assert_eq!(self.ours.take_received(), theirs);
        assert_eq!(self.ours.status(), Status::Reliable);
    }
}

#[test]
fn we_call_spandsp_with_detection() {
    Call::new(Role::Originate, DETECTING, true).carries_data_both_ways();
}

#[test]
fn spandsp_calls_us_with_detection() {
    Call::new(Role::Answer, DETECTING, true).carries_data_both_ways();
}

#[test]
fn we_call_spandsp_straight_into_lapm() {
    let setup = Setup {
        detection: false,
        ..DETECTING
    };
    Call::new(Role::Originate, setup, false).carries_data_both_ways();
}

#[test]
fn spandsp_calls_us_straight_into_lapm() {
    Call::new(Role::Answer, DETECTING, false).carries_data_both_ways();
}

fn text(len: usize) -> Vec<u8> {
    b"the carrier carries the data over the line, \0\x33\x66 and back\r\n"
        .iter()
        .copied()
        .cycle()
        .take(len)
        .collect()
}

#[test]
fn negotiates_v42bis_with_spandsp_and_compresses_from_the_caller() {
    let offer = Directions {
        transmit: true,
        receive: true,
        parameters: Parameters {
            codewords: 2048,
            max_string: 32,
        },
    };
    let setup = Setup {
        compression: Some(CompressionSetup {
            offer,
            required: false,
        }),
        ..DETECTING
    };
    // spandsp's V.42 proposes and accepts V.42bis only from the caller, at 512 and 6.
    let expected = Parameters {
        codewords: 512,
        max_string: 6,
    };
    for role in [Role::Originate, Role::Answer] {
        let mut call = Call::new(role, setup, true);
        call.run(Duration::from_secs(5));
        assert_eq!(call.ours.status(), Status::Reliable, "{role:?}");
        let agreed = call.ours.compression().expect("spandsp agreed to V.42bis");
        let caller = role == Role::Originate;
        assert_eq!(
            (agreed.transmit, agreed.receive, agreed.parameters),
            (caller, !caller, expected)
        );
        let mut codec = V42bis::new(
            expected.codewords,
            expected.max_string,
            CompressionMode::Dynamic,
        );

        let ours = text(15_000);
        let theirs: Vec<u8> = ours.iter().rev().copied().collect();
        call.ours.send(&ours);
        call.theirs.send(&if caller {
            theirs.clone()
        } else {
            codec.compress(&theirs)
        });
        call.run(Duration::from_secs(90));
        let sent_to_them = call.theirs.received();
        let from_us = if caller {
            assert!(sent_to_them.len() * 2 < ours.len(), "we did not compress");
            codec.decompress(sent_to_them)
        } else {
            sent_to_them.to_vec()
        };
        assert!(
            from_us == ours,
            "{role:?}: {} of {}",
            from_us.len(),
            ours.len()
        );
        assert!(call.ours.take_received() == theirs, "{role:?}");
    }
}

#[test]
fn both_recover_from_line_errors() {
    for role in [Role::Originate, Role::Answer] {
        let mut call = Call::new(role, DETECTING, true);
        call.run(Duration::from_secs(5));
        assert_eq!(call.ours.status(), Status::Reliable);
        call.error_every = Some(10_007);
        call.carries_data();
    }
}
