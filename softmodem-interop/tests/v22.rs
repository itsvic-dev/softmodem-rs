use softmodem_dsp::pump::{DataPump, Modulation, Role};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_interop::{GuardTone, V22};

const FRAME: usize = 160;
const FRAMES: usize = 150;

fn payload() -> Vec<u8> {
    (0..=255).collect()
}

fn reversed() -> Vec<u8> {
    payload().into_iter().rev().collect()
}

fn exchange(caller: &mut V22, answerer: &mut V22, frames: usize) {
    let mut up = [0; FRAME];
    let mut down = [0; FRAME];
    for _ in 0..frames {
        caller.render(&mut up);
        answerer.render(&mut down);
        answerer.process(&up);
        caller.process(&down);
    }
}

#[test]
fn spandsp_trains_and_carries_data_with_itself() {
    let mut caller = V22::new(true, GuardTone::Hz1800);
    let mut answerer = V22::new(false, GuardTone::Hz1800);

    exchange(&mut caller, &mut answerer, FRAMES);
    assert!(caller.trained() && answerer.trained());
    assert_eq!((caller.bit_rate(), answerer.bit_rate()), (1200, 1200));

    caller.send(&payload());
    answerer.send(&reversed());
    exchange(&mut caller, &mut answerer, FRAMES);
    assert_eq!(answerer.bytes(), payload());
    assert_eq!(caller.bytes(), reversed());
}

struct Ours {
    pump: Box<dyn DataPump>,
    decoder: Decoder,
    bytes: Vec<u8>,
}

impl Ours {
    fn new(role: Role) -> Self {
        let pump = Modulation::V22.pump(role);
        Self {
            decoder: pump.decoder(),
            pump,
            bytes: Vec::new(),
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        let bits: Vec<bool> = bytes.iter().flat_map(|&b| frame(b)).collect();
        self.pump.push_bits(&bits);
    }

    fn receive(&mut self, samples: &[i16]) {
        let mut bits = Vec::new();
        self.pump.receive(samples, &mut bits);
        self.bytes
            .extend(bits.into_iter().filter_map(|b| self.decoder.push(b)));
    }
}

fn with_spandsp(ours: &mut Ours, theirs: &mut V22, frames: usize) {
    let mut from_us = [0; FRAME];
    let mut from_them = [0; FRAME];
    for _ in 0..frames {
        ours.pump.transmit(&mut from_us);
        theirs.render(&mut from_them);
        theirs.process(&from_us);
        ours.receive(&from_them);
    }
}

fn round_trip(role: Role, theirs: &mut V22) {
    let mut ours = Ours::new(role);
    with_spandsp(&mut ours, theirs, FRAMES);
    assert!(ours.pump.connected(), "we did not train with spandsp");
    assert!(theirs.trained(), "spandsp did not train with us");

    ours.send(&payload());
    theirs.send(&reversed());
    with_spandsp(&mut ours, theirs, FRAMES);
    assert_eq!(theirs.bytes(), payload(), "spandsp lost what we sent");
    assert_eq!(ours.bytes, reversed(), "we lost what spandsp sent");
}

#[test]
fn we_answer_a_spandsp_caller() {
    round_trip(Role::Answer, &mut V22::new(true, GuardTone::Hz1800));
}

#[test]
fn we_call_a_spandsp_answerer() {
    round_trip(Role::Originate, &mut V22::new(false, GuardTone::Hz1800));
}

#[test]
fn we_call_a_spandsp_answerer_without_a_guard_tone() {
    round_trip(Role::Originate, &mut V22::new(false, GuardTone::None));
}
