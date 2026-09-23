use softmodem_dsp::fsk::{
    Channel, Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE,
};
use softmodem_dsp::tone::{ANSWER_TONE_HZ, Tone};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_interop::{AnswerTone, FskChannel, FskRx, FskTx, ToneRx};

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
