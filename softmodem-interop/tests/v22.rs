// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

mod common;

use common::{FRAMES, exchange, payload, reversed, round_trip};
use softmodem_dsp::pump::{Modulation, Role};
use softmodem_interop::{GuardTone, V22bis};

#[test]
fn spandsp_trains_and_carries_data_with_itself() {
    let mut caller = V22bis::new(1200, true, GuardTone::Hz1800);
    let mut answerer = V22bis::new(1200, false, GuardTone::Hz1800);

    exchange(&mut caller, &mut answerer, FRAMES);
    assert!(caller.trained() && answerer.trained());
    assert_eq!((caller.bit_rate(), answerer.bit_rate()), (1200, 1200));

    caller.send(&payload());
    answerer.send(&reversed());
    exchange(&mut caller, &mut answerer, FRAMES);
    assert_eq!(answerer.bytes(), payload());
    assert_eq!(caller.bytes(), reversed());
}

#[test]
fn we_answer_a_spandsp_caller() {
    let mut theirs = V22bis::new(1200, true, GuardTone::Hz1800);
    round_trip(Modulation::V22, Role::Answer, &mut theirs, 1200);
}

#[test]
fn we_call_a_spandsp_answerer() {
    let mut theirs = V22bis::new(1200, false, GuardTone::Hz1800);
    round_trip(Modulation::V22, Role::Originate, &mut theirs, 1200);
}

#[test]
fn we_call_a_spandsp_answerer_without_a_guard_tone() {
    let mut theirs = V22bis::new(1200, false, GuardTone::None);
    round_trip(Modulation::V22, Role::Originate, &mut theirs, 1200);
}

#[test]
fn a_spandsp_v22bis_modem_falls_back_to_our_v22() {
    let mut theirs = V22bis::new(2400, true, GuardTone::Hz1800);
    round_trip(Modulation::V22, Role::Answer, &mut theirs, 1200);
    let mut theirs = V22bis::new(2400, false, GuardTone::Hz1800);
    round_trip(Modulation::V22, Role::Originate, &mut theirs, 1200);
}
