// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

mod common;

use common::{FRAMES, exchange, payload, reversed, round_trip};
use softmodem_dsp::pump::{Modulation, Role};
use softmodem_interop::{GuardTone, V22bis};

#[test]
fn we_answer_a_spandsp_caller_at_2400() {
    let mut theirs = V22bis::new(2400, true, GuardTone::Hz1800);
    round_trip(Modulation::V22bis, Role::Answer, &mut theirs, 2400);
}

#[test]
fn we_call_a_spandsp_answerer_at_2400() {
    let mut theirs = V22bis::new(2400, false, GuardTone::Hz1800);
    round_trip(Modulation::V22bis, Role::Originate, &mut theirs, 2400);
}

#[test]
fn we_fall_back_to_1200_with_spandsp_held_there() {
    let mut theirs = V22bis::new(1200, true, GuardTone::Hz1800);
    round_trip(Modulation::V22bis, Role::Answer, &mut theirs, 1200);
    let mut theirs = V22bis::new(1200, false, GuardTone::Hz1800);
    round_trip(Modulation::V22bis, Role::Originate, &mut theirs, 1200);
}

#[test]
fn spandsp_trains_at_2400_and_carries_data_with_itself() {
    let mut caller = V22bis::new(2400, true, GuardTone::Hz1800);
    let mut answerer = V22bis::new(2400, false, GuardTone::Hz1800);

    exchange(&mut caller, &mut answerer, FRAMES);
    assert!(caller.trained() && answerer.trained());
    assert_eq!((caller.bit_rate(), answerer.bit_rate()), (2400, 2400));

    caller.send(&payload());
    answerer.send(&reversed());
    exchange(&mut caller, &mut answerer, FRAMES);
    assert_eq!(answerer.bytes(), payload());
    assert_eq!(caller.bytes(), reversed());
}
