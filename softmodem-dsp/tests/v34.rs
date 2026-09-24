mod common;

use common::{add_noise, mix, resample};
use softmodem_dsp::pump::Role;
use softmodem_dsp::tone::Tone;
use softmodem_dsp::v34::info::{Deframer, Info, Info0, Info1c, Probe};
use softmodem_dsp::v34::probing::{self, Analyser, L1_DBM0, L2_DBM0, Probing};
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

fn probe(level_dbm0: f64, samples: usize) -> Vec<i16> {
    let mut out = vec![0; samples];
    probing::Sender::new(level_dbm0).render(&mut out);
    out
}

fn analyse(samples: &[i16]) -> Probing {
    let mut analyser = Analyser::new(L2_DBM0);
    analyser.push(samples);
    analyser.result().unwrap()
}

#[test]
fn l1_has_its_level_and_does_not_clip() {
    let l1 = probe(L1_DBM0, 1600);
    let mean_square = l1.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / 1600.0;
    let db = 10.0 * (mean_square / (softmodem_dsp::sine_peak(L1_DBM0).powi(2) / 2.0)).log10();
    assert!(
        db.abs() < 0.1,
        "the far end would misjudge the line by {db:.2} dB"
    );
    assert!(l1.iter().all(|&s| s.unsigned_abs() < 32000));
}

#[test]
fn a_clean_line_passes_every_tone_at_its_level() {
    let result = analyse(&probe(L2_DBM0, 4000));
    for tone in &result.tones {
        assert!(
            tone.gain_db.abs() < 0.05,
            "rate selection would see a loss that is not there: {tone:?}"
        );
        assert!(
            tone.snr_db > 50.0,
            "rate selection would see noise that is not there: {tone:?}"
        );
    }
    assert!(result.frequency_offset_hz.unwrap().abs() < 0.01);
}

#[test]
fn measures_the_snr_the_data_would_meet() {
    let noisy = add_noise(probe(L2_DBM0, 8000), L2_DBM0, 30.0);
    for tone in &analyse(&noisy).tones {
        assert!(
            (tone.snr_db - 31.0).abs() < 2.0,
            "rate selection would misjudge the noise: {tone:?}"
        );
    }
}

#[test]
fn measures_the_shape_of_the_line() {
    let line = probe(L2_DBM0, 4001);
    let filtered: Vec<i16> = line
        .windows(2)
        .map(|pair| i16::try_from(i32::midpoint(pair[0].into(), pair[1].into())).unwrap())
        .collect();
    for tone in &analyse(&filtered).tones {
        let expected = 20.0 * (std::f64::consts::PI * tone.hz / 8000.0).cos().log10();
        assert!(
            (tone.gain_db - expected).abs() < 0.2,
            "pre-emphasis would suit another line: {tone:?}, not {expected:.2} dB"
        );
    }
}

#[test]
fn measures_the_frequency_offset_of_1050_hz() {
    let fast = resample(&probe(L2_DBM0, 8000), 1.0002);
    let offset = analyse(&fast).frequency_offset_hz.unwrap();
    assert!(
        (offset - 1050.0 * 0.0002).abs() < 0.02,
        "INFO1 would report {offset:.3} Hz"
    );
}
