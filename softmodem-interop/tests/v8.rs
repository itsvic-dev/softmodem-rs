mod common;

use common::{FRAME, FRAMES, Ours, carry_data, with_spandsp};
use softmodem_dsp::automode;
use softmodem_dsp::pump::{Modulation, Role};
use softmodem_interop::{GuardTone, V8, V8Outcome, V22bis};

const V8_FRAMES: usize = 600;

/// Runs V.8 between our automode pump and spandsp until spandsp agrees.
fn negotiate(ours: &mut Ours, theirs: &mut V8) -> V8Outcome {
    let mut from_us = [0; FRAME];
    let mut from_them = [0; FRAME];
    for _ in 0..V8_FRAMES {
        ours.pump.transmit(&mut from_us);
        theirs.render(&mut from_them);
        theirs.process(&from_us);
        let mut bits = Vec::new();
        ours.pump.receive(&from_them, &mut bits);
        if let Some(outcome) = theirs.outcome().filter(|o| o.agreed) {
            return outcome;
        }
    }
    panic!(
        "spandsp reached no V.8 agreement, last {:?}",
        theirs.outcome()
    );
}

fn train(ours: &mut Ours, theirs: &mut V22bis) {
    with_spandsp(ours, theirs, FRAMES);
    assert!(ours.pump.connected(), "we did not train after V.8");
    assert!(theirs.trained(), "spandsp did not train after V.8");
    assert_eq!(ours.pump.bit_rate(), 2400);
    assert_eq!(theirs.bit_rate(), 2400);
}

#[test]
fn we_answer_a_spandsp_v8_caller_and_go_on_at_2400() {
    let mut ours = Ours::from_pump(automode::pump(Modulation::V22bis, Role::Answer));
    let outcome = negotiate(&mut ours, &mut V8::new(true, true, true));
    assert!(outcome.v22bis, "V.8 did not choose V.22bis: {outcome:?}");

    let mut theirs = V22bis::new(2400, true, GuardTone::Hz1800);
    train(&mut ours, &mut theirs);
    carry_data(&mut ours, &mut theirs);
}

#[test]
fn we_call_a_spandsp_v8_answerer_and_go_on_at_2400() {
    let mut ours = Ours::from_pump(automode::pump(Modulation::V22bis, Role::Originate));
    let outcome = negotiate(&mut ours, &mut V8::new(false, true, true));
    assert!(outcome.v22bis, "V.8 did not choose V.22bis: {outcome:?}");

    let mut theirs = V22bis::new(2400, false, GuardTone::Hz1800);
    train(&mut ours, &mut theirs);
    carry_data(&mut ours, &mut theirs);
}

#[test]
fn v8_with_a_v21_only_spandsp_agrees_on_v21() {
    let mut ours = Ours::from_pump(automode::pump(Modulation::V22bis, Role::Answer));
    let outcome = negotiate(&mut ours, &mut V8::new(true, false, true));
    assert!(
        outcome.v21 && !outcome.v22bis,
        "JM offered a mode the caller lacks: {outcome:?}"
    );
}
