//! Calls between one of our data pumps and spandsp's V.22bis modem.

#![allow(dead_code)]

use softmodem_dsp::pump::{DataPump, Modulation, Role};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_interop::V22bis;

pub const FRAME: usize = 160;
pub const FRAMES: usize = 150;

pub fn payload() -> Vec<u8> {
    (0..=255).collect()
}

pub fn reversed() -> Vec<u8> {
    payload().into_iter().rev().collect()
}

pub fn exchange(caller: &mut V22bis, answerer: &mut V22bis, frames: usize) {
    let mut up = [0; FRAME];
    let mut down = [0; FRAME];
    for _ in 0..frames {
        caller.render(&mut up);
        answerer.render(&mut down);
        answerer.process(&up);
        caller.process(&down);
    }
}

pub struct Ours {
    pub pump: Box<dyn DataPump>,
    decoder: Option<Decoder>,
    pub bytes: Vec<u8>,
}

impl Ours {
    pub fn new(modulation: Modulation, role: Role) -> Self {
        Self::from_pump(modulation.pump(role))
    }

    pub fn from_pump(pump: Box<dyn DataPump>) -> Self {
        Self {
            pump,
            decoder: None,
            bytes: Vec::new(),
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        let bits: Vec<bool> = bytes.iter().flat_map(|&b| frame(b)).collect();
        self.pump.push_bits(&bits);
    }

    fn receive(&mut self, samples: &[i16]) {
        let mut bits = Vec::new();
        self.pump.receive(samples, &mut bits);
        if bits.is_empty() {
            return;
        }
        let pump = &self.pump;
        let decoder = self.decoder.get_or_insert_with(|| pump.decoder());
        self.bytes
            .extend(bits.into_iter().filter_map(|b| decoder.push(b)));
    }
}

/// Sends data both ways over a trained link and checks that it arrives.
pub fn carry_data(ours: &mut Ours, theirs: &mut V22bis) {
    ours.send(&payload());
    theirs.send(&reversed());
    with_spandsp(ours, theirs, FRAMES);
    assert_eq!(theirs.bytes(), payload(), "spandsp lost what we sent");
    assert_eq!(ours.bytes, reversed(), "we lost what spandsp sent");
}

pub fn with_spandsp(ours: &mut Ours, theirs: &mut V22bis, frames: usize) {
    let mut from_us = [0; FRAME];
    let mut from_them = [0; FRAME];
    for _ in 0..frames {
        ours.pump.transmit(&mut from_us);
        theirs.render(&mut from_them);
        theirs.process(&from_us);
        ours.receive(&from_them);
    }
}

/// Trains `modulation` in `role` against `theirs`, checks that both ends
/// settle at `bit_rate`, and sends data both ways.
pub fn round_trip(modulation: Modulation, role: Role, theirs: &mut V22bis, bit_rate: u32) {
    let mut ours = Ours::new(modulation, role);
    with_spandsp(&mut ours, theirs, FRAMES);
    assert!(ours.pump.connected(), "we did not train with spandsp");
    assert!(theirs.trained(), "spandsp did not train with us");
    assert_eq!(
        ours.pump.bit_rate(),
        bit_rate,
        "we settled at the wrong rate"
    );
    assert_eq!(
        u32::try_from(theirs.bit_rate()).unwrap(),
        bit_rate,
        "spandsp settled at the wrong rate"
    );
    carry_data(&mut ours, theirs);
}
