mod common;

use common::{add_noise, mix, resample};
use softmodem_dsp::pump::Role;
use softmodem_dsp::tone::Tone;
use softmodem_dsp::v34::info::{Deframer, Info, Info0, Info1c, Probe};
use softmodem_dsp::v34::{GUARD_DBM0, GUARD_HZ, NOMINAL_DBM0, dpsk, tones};

const FRAME: usize = 160;

fn info0() -> Info0 {
    Info0 {
        supports_2743: true,
        supports_2800: true,
        supports_3429: true,
        high_carrier_3200: true,
        allows_3429: true,
        constellation_1664: true,
        ..Info0::default()
    }
}

fn info1c() -> Info1c {
    Info1c {
        md_length: 3,
        probes: [Probe {
            high_carrier: true,
            pre_emphasis: 4,
            max_rate: 12,
        }; 6],
        frequency_offset: Some(12),
        ..Info1c::default()
    }
}

fn send(role: Role, groups: &[Vec<bool>]) -> Vec<i16> {
    let mut modulator = dpsk::Modulator::new(role);
    let mut out = vec![0; 400];
    for group in groups {
        modulator.send(group.iter().copied());
    }
    while !modulator.idle() {
        let mut chunk = [0; FRAME];
        modulator.render(&mut chunk);
        out.extend_from_slice(&chunk);
    }
    out.extend_from_slice(&[0; 400]);
    if role == Role::Answer {
        let mut guard = vec![0; out.len()];
        Tone::new(GUARD_HZ, GUARD_DBM0).render(&mut guard);
        out = mix(&out, &guard);
    }
    out
}

fn receive<T: Info>(role: Role, samples: &[i16]) -> Vec<T> {
    let mut demodulator = dpsk::Demodulator::new(role);
    let mut deframer = Deframer::<T>::default();
    let mut bits = Vec::new();
    for chunk in samples.chunks(FRAME) {
        demodulator.process(chunk, &mut bits);
    }
    bits.into_iter()
        .filter_map(|bit| deframer.push(bit))
        .collect()
}

#[test]
fn each_end_hears_the_other_ends_info0() {
    for role in [Role::Answer, Role::Originate] {
        let line = send(role, &[info0().frame()]);
        assert_eq!(
            receive::<Info0>(role, &line),
            [info0()],
            "phase 2 cannot start with INFO0 from the {role:?} end"
        );
    }
}

#[test]
fn hears_a_group_of_repeated_sequences() {
    let frame = info0().frame();
    let line = send(Role::Answer, &[frame.clone(), frame.clone(), frame]);
    assert_eq!(receive::<Info0>(Role::Answer, &line).len(), 3);
}

#[test]
fn hears_info1c_through_noise_and_a_clock_offset() {
    let line = send(Role::Originate, &[info1c().frame()]);
    let line = resample(&add_noise(line, NOMINAL_DBM0, 20.0), 1.0001);
    assert_eq!(receive::<Info1c>(Role::Originate, &line), [info1c()]);
}

#[test]
fn does_not_hear_the_other_carrier() {
    let line = send(Role::Answer, &[info0().frame()]);
    assert!(receive::<Info0>(Role::Originate, &line).is_empty());
}

fn tone(role: Role, samples: usize, reversals: &[usize]) -> Vec<i16> {
    let mut sender = tones::Sender::new(role);
    let mut out = Vec::new();
    let mut at = 0;
    for &reversal in reversals {
        let mut chunk = vec![0; reversal - at];
        sender.render(&mut chunk);
        out.extend(chunk);
        sender.reverse_after(0);
        at = reversal;
    }
    let mut rest = vec![0; samples - at];
    sender.render(&mut rest);
    out.extend(rest);
    if role == Role::Answer {
        let mut guard = vec![0; out.len()];
        Tone::new(GUARD_HZ, GUARD_DBM0).render(&mut guard);
        out = mix(&out, &guard);
    }
    out
}

fn reversals(role: Role, samples: &[i16]) -> Vec<f64> {
    let mut detector = tones::Detector::new(role);
    samples
        .chunks(FRAME)
        .flat_map(|chunk| detector.process(chunk))
        .collect()
}

fn within_a_sample(heard: &[f64], sent: &[usize]) -> bool {
    heard.len() == sent.len()
        && heard
            .iter()
            .zip(sent)
            .all(|(&heard, &sent)| (heard - f64::from(u32::try_from(sent).unwrap())).abs() < 1.0)
}

#[test]
fn times_each_reversal_of_tones_a_and_b_to_within_a_sample() {
    for role in [Role::Answer, Role::Originate] {
        let sent = [1003, 1500, 4321];
        let heard = reversals(role, &tone(role, 6000, &sent));
        assert!(
            within_a_sample(&heard, &sent),
            "the round trip delay from the {role:?} tone would be off: {heard:?}"
        );
    }
}

#[test]
fn times_reversals_through_noise_and_the_echo_of_its_own_tone() {
    let sent = [2000, 5000];
    let far = add_noise(tone(Role::Answer, 8000, &sent), NOMINAL_DBM0, 25.0);
    let echo: Vec<i16> = tone(Role::Originate, 8000, &[3000])
        .into_iter()
        .map(|s| s / 4)
        .collect();
    let heard = reversals(Role::Answer, &mix(&far, &echo));
    assert!(
        within_a_sample(&heard, &sent),
        "an echo would upset the round trip delay: {heard:?}"
    );
}

#[test]
fn a_tone_that_stops_is_not_a_reversal() {
    let mut line = tone(Role::Originate, 3000, &[]);
    line.extend([0; 2000]);
    assert!(reversals(Role::Originate, &line).is_empty());
}

#[test]
fn hears_only_the_tone_it_listens_for() {
    let mut detector = tones::Detector::new(Role::Answer);
    detector.process(&tone(Role::Answer, 1000, &[]));
    assert!(detector.present());
    let mut detector = tones::Detector::new(Role::Originate);
    detector.process(&tone(Role::Answer, 1000, &[]));
    assert!(!detector.present());
}

#[test]
fn stays_present_through_a_reversal() {
    let mut detector = tones::Detector::new(Role::Originate);
    let line = tone(Role::Originate, 2000, &[1000]);
    for (n, chunk) in line.chunks(10).enumerate() {
        detector.process(chunk);
        assert!(n < 10 || detector.present(), "lost at {n}");
    }
}
