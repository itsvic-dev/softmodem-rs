// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

mod common;

use std::collections::VecDeque;

use common::{add_noise, common_subsequence, mix, random_bytes, resample, scale};
use softmodem_dsp::dpsk::{Demodulator, Modulator, V22_HIGH_HZ, V22_LOW_HZ};
use softmodem_dsp::scrambler::{Descrambler, Scrambler};
use softmodem_dsp::tone::Tone;
use softmodem_dsp::uart::{Decoder, frame};

const FRAME: usize = 160;
const LEVEL: f64 = -13.0;
const LEAD_IN: usize = 4000;

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
    let mut out = vec![0; LEAD_IN];
    let mut next = |bits: &mut VecDeque<bool>| scrambler.scramble(bits.pop_front().unwrap_or(true));
    modulator.render(&mut out, || next(&mut bits));
    bits.extend(bytes.iter().flat_map(|&b| frame(b)));
    while !bits.is_empty() {
        let mut chunk = [0; FRAME];
        modulator.render(&mut chunk, || next(&mut bits));
        out.extend_from_slice(&chunk);
    }
    let mut tail = [0; 800];
    modulator.render(&mut tail, || next(&mut bits));
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
    let mut decoder = Decoder::new();
    let mut bits = Vec::new();
    let mut bytes = Vec::new();
    let mut carrier_drops = 0;
    let mut had_carrier = false;
    for chunk in samples.chunks(FRAME) {
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
fn clean_line_round_trips_both_channels() {
    for carrier in [V22_LOW_HZ, V22_HIGH_HZ] {
        let received = link(carrier, |s| s);
        assert_eq!(received.bytes, payload(), "corrupted at {carrier} Hz");
        assert_eq!(received.carrier_drops, 0);
    }
}

#[test]
fn survives_noise_at_12_db() {
    let received = link(V22_LOW_HZ, |s| add_noise(s, LEVEL, 12.0));
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
fn tolerates_the_7_hz_carrier_error_v22_requires() {
    for (carrier, error) in [(V22_LOW_HZ, -7.0), (V22_LOW_HZ, 7.0), (V22_HIGH_HZ, 7.0)] {
        let received = demodulate(carrier, &modulate(carrier + error, &payload()));
        assert_eq!(
            received.bytes,
            payload(),
            "corrupted at {carrier} Hz, {error} Hz off"
        );
    }
}

#[test]
fn hears_the_high_channel_through_its_guard_tone() {
    let received = link(V22_HIGH_HZ, |s| with_guard_tone(&s));
    assert_eq!(received.bytes, payload());
}

#[test]
fn rejects_the_other_channel_in_full_duplex() {
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
    let expected = random_bytes(600);
    let mut samples = modulate(V22_LOW_HZ, &expected);
    for n in 1..=losses {
        let start = n * samples.len() / (losses + 1) / FRAME * FRAME;
        samples[start..start + FRAME].fill(0);
    }
    let received = demodulate(V22_LOW_HZ, &samples);
    let kept = common_subsequence(&received.bytes, &expected);
    assert!(
        kept >= expected.len() - 10 * losses,
        "packet loss spread past the characters it hit: {kept} of {} survived",
        expected.len()
    );
    assert_eq!(received.carrier_drops, 0);
}

#[test]
fn silence_and_idle_mark_yield_nothing() {
    assert!(demodulate(V22_LOW_HZ, &[0; 8000]).bytes.is_empty());
    assert!(
        demodulate(V22_LOW_HZ, &modulate(V22_LOW_HZ, &[]))
            .bytes
            .is_empty()
    );
}

fn carrier_edges(samples: &[i16]) -> Vec<usize> {
    let mut demodulator = Demodulator::new(V22_LOW_HZ);
    let mut bits = Vec::new();
    let mut edges = Vec::new();
    let mut carrier = false;
    for (n, sample) in samples.iter().enumerate() {
        demodulator.process(std::slice::from_ref(sample), &mut bits);
        if demodulator.carrier() != carrier {
            carrier = demodulator.carrier();
            edges.push(n);
        }
    }
    edges
}

fn carrier_at(level_dbm0: f64, samples: usize) -> Vec<i16> {
    let mut modulator = Modulator::new(V22_LOW_HZ, level_dbm0);
    let mut out = vec![0; samples];
    let mut scrambler = Scrambler::new();
    modulator.render(&mut out, || scrambler.scramble(true));
    out
}

#[test]
fn carrier_detect_meets_the_v22_response_times() {
    let mut samples = carrier_at(-40.0, 8000);
    samples.extend([0; 8000]);

    let edges = carrier_edges(&samples);
    let ms = |n: usize| n / 8;
    assert_eq!(edges.len(), 2);
    assert!(
        (105..=205).contains(&ms(edges[0])),
        "on after {} ms",
        ms(edges[0])
    );
    let off = ms(edges[1]) - 1000;
    assert!((10..=24).contains(&off), "off after {off} ms");
}

#[test]
fn carrier_detect_ignores_signals_below_the_threshold() {
    assert!(carrier_edges(&carrier_at(-44.0, 16000)).is_empty());
}
