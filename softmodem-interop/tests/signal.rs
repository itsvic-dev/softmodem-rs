// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use softmodem_dsp::ansam::{self, AnswerToneDetector, AnswerToneKind};
use softmodem_dsp::dtmf::DtmfSender;
use softmodem_dsp::fsk::{
    Channel, Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE,
};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_interop::{AnswerTone, DtmfRx, FskChannel, FskRx, FskTx, ToneRx, ToneTx};

const FRAME: usize = 160;

fn payload() -> Vec<u8> {
    (0..=255).collect()
}

fn pairs() -> [(Channel, FskChannel); 2] {
    [
        (V21_ORIGINATE, FskChannel::V21Originate),
        (V21_ANSWER, FskChannel::V21Answer),
    ]
}

#[test]
fn we_demodulate_what_spandsp_sends() {
    for (ours, theirs) in pairs() {
        let mut spandsp = FskTx::new(theirs);
        let mut demodulator = Demodulator::new(ours);
        let mut decoder = Decoder::new();
        let mut bits = Vec::new();
        let mut received = Vec::new();
        let mut samples = [0; FRAME];

        for _ in 0..50 {
            spandsp.render(&mut samples);
            demodulator.process(&samples, &mut bits);
        }
        spandsp.send(&payload());
        for _ in 0..600 {
            spandsp.render(&mut samples);
            demodulator.process(&samples, &mut bits);
            received.extend(bits.drain(..).filter_map(|b| decoder.push(b)));
        }
        assert_eq!(received, payload(), "{theirs:?}");
    }
}

#[test]
fn spandsp_demodulates_what_we_send() {
    for (ours, theirs) in pairs() {
        let mut modulator = Modulator::new(ours, V21_MAX_LEVEL_DBM0);
        let mut spandsp = FskRx::new(theirs);
        let mut samples = [0; FRAME];

        for _ in 0..50 {
            modulator.render(&mut samples);
            spandsp.process(&samples);
        }
        assert!(spandsp.carrier(), "spandsp heard no carrier on {theirs:?}");
        modulator.push_bits(payload().into_iter().flat_map(frame));
        for _ in 0..600 {
            modulator.render(&mut samples);
            spandsp.process(&samples);
        }
        assert_eq!(spandsp.bytes(), payload(), "{theirs:?}");
    }
}

#[test]
fn spandsp_hears_our_answer_tone_as_v25_ans() {
    let mut tone = Tone::new(ANSWER_TONE_HZ, V21_MAX_LEVEL_DBM0);
    let mut detector = ToneRx::new(AnswerTone::Ans);
    let mut samples = [0; FRAME];
    for _ in 0..100 {
        tone.render(&mut samples);
        detector.process(&samples);
    }
    assert_eq!(detector.detected(), Some(AnswerTone::Ans));
}

#[test]
fn spandsp_hears_our_ansam_with_and_without_reversals() {
    for (reversals, expected) in [(false, AnswerTone::Ansam), (true, AnswerTone::AnsamPr)] {
        let mut tone = ansam::AnswerTone::new(AnswerToneKind::Ansam, reversals, V21_MAX_LEVEL_DBM0);
        let mut detector = ToneRx::new(expected);
        let mut samples = [0; FRAME];
        for _ in 0..150 {
            tone.render(&mut samples);
            detector.process(&samples);
        }
        assert_eq!(detector.detected(), Some(expected), "reversals {reversals}");
    }
}

#[test]
fn spandsp_hears_our_dtmf_digits() {
    let mut sender = DtmfSender::new("123A456B,789C*0#D", 0.2);
    let mut detector = DtmfRx::new();
    let mut samples = [0; FRAME];
    while !sender.done() {
        sender.render(&mut samples);
        detector.process(&samples);
    }
    assert_eq!(detector.digits(), "123A456B789C*0#D");
}

#[test]
fn we_tell_spandsp_answer_tones_apart() {
    for (theirs, ours) in [
        (AnswerTone::Ans, AnswerToneKind::Ans),
        (AnswerTone::AnsPr, AnswerToneKind::Ans),
        (AnswerTone::Ansam, AnswerToneKind::Ansam),
        (AnswerTone::AnsamPr, AnswerToneKind::Ansam),
    ] {
        let mut tone = ToneTx::new(theirs);
        let mut detector = AnswerToneDetector::new();
        let mut samples = [0; FRAME];
        let mut heard = None;
        for _ in 0..100 {
            tone.render(&mut samples);
            heard = detector.process(&samples).or(heard);
        }
        assert_eq!(heard, Some(ours), "{theirs:?}");
    }
}
