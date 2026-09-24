mod common;

use common::{add_noise, mix, resample};
use softmodem_dsp::pump::Role;
use softmodem_dsp::tone::Tone;
use softmodem_dsp::v34::info::{Deframer, Info, Info0, Info1c, Probe};
use softmodem_dsp::v34::{GUARD_DBM0, GUARD_HZ, NOMINAL_DBM0, dpsk};

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
