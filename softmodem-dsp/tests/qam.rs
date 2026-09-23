mod common;

use std::collections::VecDeque;

use common::{add_noise, common_subsequence, mix, random_bytes, resample, scale};
use softmodem_dsp::dpsk::{self, V22_HIGH_HZ, V22_LOW_HZ};
use softmodem_dsp::qam::{Demodulator, Modulator, Rate};
use softmodem_dsp::scrambler::{Descrambler, Scrambler};
use softmodem_dsp::tone::Tone;
use softmodem_dsp::uart::{Decoder, frame};

const FRAME: usize = 160;
const LEVEL: f64 = -13.0;
const TRAINING: usize = 6400;
const SETTLING: usize = 1600;

fn payload() -> Vec<u8> {
    (0..=255)
        .chain(*b"The quick brown fox jumps over the lazy dog.")
        .collect()
}

fn modulate(carrier_hz: f64, bytes: &[u8]) -> Vec<i16> {
    let mut modulator = Modulator::new(carrier_hz, LEVEL);
    let mut scrambler = Scrambler::new();
    scrambler.guard_against_lockup();
    let mut bits: VecDeque<bool> = VecDeque::new();
    let mut next = |bits: &mut VecDeque<bool>| scrambler.scramble(bits.pop_front().unwrap_or(true));
    let mut out = vec![0; TRAINING];
    modulator.render(&mut out, Rate::Bps1200, || next(&mut bits));
    let mut settle = vec![0; SETTLING];
    modulator.render(&mut settle, Rate::Bps2400, || next(&mut bits));
    out.extend(settle);
    bits.extend(bytes.iter().flat_map(|&b| frame(b)));
    while !bits.is_empty() {
        let mut chunk = [0; FRAME];
        modulator.render(&mut chunk, Rate::Bps2400, || next(&mut bits));
        out.extend_from_slice(&chunk);
    }
    let mut tail = [0; 800];
    modulator.render(&mut tail, Rate::Bps2400, || next(&mut bits));
    out.extend_from_slice(&tail);
    out
}

fn with_guard_tone(samples: &[i16]) -> Vec<i16> {
    let mut guard = vec![0; samples.len()];
    Tone::new(1800.0, LEVEL - 6.0).render(&mut guard);
    mix(samples, &guard)
}

struct Received {
    bytes: Vec<u8>,
    carrier_drops: usize,
}

fn demodulate(carrier_hz: f64, samples: &[i16]) -> Received {
    let mut demodulator = Demodulator::new(carrier_hz);
    let mut descrambler = Descrambler::new();
    let mut decoder = Decoder::v14();
    let mut bits = Vec::new();
    let mut bytes = Vec::new();
    let mut carrier_drops = 0;
    let mut had_carrier = false;
    demodulator.process(&samples[..TRAINING], &mut bits);
    demodulator.set_rate(Rate::Bps2400);
    demodulator.process(&samples[TRAINING..TRAINING + SETTLING], &mut bits);
    for bit in bits.drain(..) {
        descrambler.descramble(bit);
    }
    for chunk in samples[TRAINING + SETTLING..].chunks(FRAME) {
        demodulator.process(chunk, &mut bits);
        let carrier = demodulator.carrier();
        for bit in bits.drain(..) {
            let bit = descrambler.descramble(bit);
            if carrier {
                bytes.extend(decoder.push(bit));
            }
        }
        if had_carrier && !carrier {
            carrier_drops += 1;
            decoder.reset();
        }
        had_carrier = carrier;
    }
    Received {
        bytes,
        carrier_drops,
    }
}

fn link(carrier_hz: f64, impair: impl FnOnce(Vec<i16>) -> Vec<i16>) -> Received {
    demodulate(carrier_hz, &impair(modulate(carrier_hz, &payload())))
}

#[test]
fn clean_line_round_trips_both_channels_at_2400() {
    for carrier in [V22_LOW_HZ, V22_HIGH_HZ] {
        let received = link(carrier, |s| s);
        assert_eq!(received.bytes, payload(), "corrupted at {carrier} Hz");
        assert_eq!(received.carrier_drops, 0);
    }
}

#[test]
fn survives_noise_at_18_db() {
    let received = link(V22_LOW_HZ, |s| add_noise(s, LEVEL, 18.0));
    assert_eq!(received.bytes, payload());
}

#[test]
fn ignores_gain() {
    for gain in [0.05, 0.5, 2.0] {
        let received = link(V22_LOW_HZ, |s| scale(s, gain));
        assert_eq!(received.bytes, payload(), "corrupted at line gain {gain}");
    }
}

#[test]
fn tracks_a_sender_clock_that_is_off_by_a_tenth_of_a_percent() {
    for ratio in [0.999, 1.001] {
        let received = link(V22_LOW_HZ, |s| resample(&s, ratio));
        assert_eq!(
            received.bytes,
            payload(),
            "lost symbol sync at clock ratio {ratio}"
        );
    }
}

#[test]
fn locks_to_the_7_hz_carrier_error_v22bis_allows() {
    for (carrier, error) in [(V22_LOW_HZ, -7.0), (V22_LOW_HZ, 7.0), (V22_HIGH_HZ, -7.0)] {
        let received = demodulate(carrier, &modulate(carrier + error, &payload()));
        assert_eq!(
            received.bytes,
            payload(),
            "corrupted at {carrier} Hz, {error} Hz off"
        );
    }
}

#[test]
fn equalises_a_line_that_smears_the_symbols() {
    let smear = |s: Vec<i16>| -> Vec<i16> {
        let mut taps = [0.0; 14];
        taps[0] = 1.0;
        taps[13] = 0.5;
        (0..s.len())
            .map(|n| {
                let x: f64 = taps
                    .iter()
                    .enumerate()
                    .filter(|&(k, _)| k <= n)
                    .map(|(k, &h)| h * f64::from(s[n - k]))
                    .sum();
                common::clamp(x)
            })
            .collect()
    };
    let received = link(V22_LOW_HZ, smear);
    assert_eq!(received.bytes, payload());
}

#[test]
fn rejects_the_other_channel_and_the_guard_tone() {
    let reversed: Vec<u8> = payload().into_iter().rev().collect();
    let high = with_guard_tone(&modulate(V22_HIGH_HZ, &reversed));
    let received = link(V22_LOW_HZ, |s| mix(&s, &high));
    assert_eq!(
        received.bytes,
        payload(),
        "the high channel leaked into the low"
    );

    let low = modulate(V22_LOW_HZ, &reversed);
    let received = link(V22_HIGH_HZ, |s| mix(&with_guard_tone(&s), &low));
    assert_eq!(
        received.bytes,
        payload(),
        "the low channel leaked into the high"
    );
}

#[test]
fn a_lost_packet_costs_a_few_characters_and_not_the_carrier() {
    let losses = 8;
    let expected = random_bytes(1200);
    let mut samples = modulate(V22_LOW_HZ, &expected);
    let data = TRAINING + SETTLING;
    for n in 1..=losses {
        let start = data + n * (samples.len() - data) / (losses + 1) / FRAME * FRAME;
        samples[start..start + FRAME].fill(0);
    }
    let received = demodulate(V22_LOW_HZ, &samples);
    let kept = common_subsequence(&received.bytes, &expected);
    assert!(
        kept >= expected.len() - 20 * losses,
        "packet loss spread past the characters it hit: {kept} of {} survived",
        expected.len()
    );
    assert_eq!(received.carrier_drops, 0);
}

#[test]
fn its_1200_bit_s_form_is_v22() {
    let mut modulator = Modulator::new(V22_LOW_HZ, LEVEL);
    let mut scrambler = Scrambler::new();
    let mut bits: VecDeque<bool> = VecDeque::new();
    let mut samples = vec![0; TRAINING];
    let mut next = |bits: &mut VecDeque<bool>| scrambler.scramble(bits.pop_front().unwrap_or(true));
    modulator.render(&mut samples, Rate::Bps1200, || next(&mut bits));
    bits.extend(payload().iter().flat_map(|&b| frame(b)));
    let mut rest = vec![0; 30_000];
    modulator.render(&mut rest, Rate::Bps1200, || next(&mut bits));
    samples.extend(rest);

    let mut demodulator = dpsk::Demodulator::new(V22_LOW_HZ);
    let mut descrambler = Descrambler::new();
    let mut decoder = Decoder::v14();
    let mut line = Vec::new();
    let mut bytes = Vec::new();
    for chunk in samples.chunks(FRAME) {
        demodulator.process(chunk, &mut line);
        for bit in line.drain(..) {
            let bit = descrambler.descramble(bit);
            if demodulator.carrier() {
                bytes.extend(decoder.push(bit));
            }
        }
    }
    assert_eq!(bytes, payload());
}
