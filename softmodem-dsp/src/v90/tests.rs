// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The analogue modem against the digital modem.

use super::analogue::Analogue;
use super::digital::Digital;
use crate::pump::DataPump;

const FRAME: usize = 160;

// G.711 A-law and back, as the transport carries every call.
fn alaw(sample: i16) -> i16 {
    const ENDS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];
    let value = i32::from(sample) >> 3;
    let (negative, magnitude) = if value >= 0 {
        (false, value)
    } else {
        (true, -value - 1)
    };
    let segment = ENDS.iter().position(|&end| magnitude <= end).unwrap_or(7);
    let shift = if segment < 2 { 1 } else { segment };
    let step = ((magnitude.min(0xFFF) >> shift) & 0x0F) << 4;
    let restored = if segment == 0 {
        step + 8
    } else {
        (step + 0x108) << (segment - 1)
    };
    let restored = i16::try_from(restored).unwrap_or(i16::MAX);
    if negative { -restored } else { restored }
}

// A-law, with a digital pad of `gain` that the network codes to A-law again.
fn padded(gain: f64) -> impl Fn(i16) -> i16 {
    move |sample| {
        #[expect(clippy::cast_possible_truncation, reason = "within i16 after the pad")]
        let padded = (f64::from(alaw(sample)) * gain).round() as i16;
        alaw(padded)
    }
}

fn exchange(
    analogue: &mut Analogue,
    digital: &mut Digital,
    frames: usize,
    line: &impl Fn(i16) -> i16,
) -> (Vec<bool>, Vec<bool>) {
    let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
    let (mut at_analogue, mut at_digital) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        analogue.transmit(&mut up);
        digital.transmit(&mut down);
        digital.receive(&up.map(line), &mut at_digital);
        analogue.receive(&down.map(line), &mut at_analogue);
    }
    (at_analogue, at_digital)
}

// Connects, then checks that data crosses both ways, and gives the downstream rate.
fn connect_over(line: impl Fn(i16) -> i16) -> u32 {
    let mut analogue = Analogue::new();
    let mut digital = Digital::new();
    let mut frames = 0;
    while !(analogue.connected() && digital.connected()) && frames < 2000 {
        exchange(&mut analogue, &mut digital, 1, &line);
        frames += 1;
    }
    assert!(
        analogue.connected() && digital.connected(),
        "no V.90 connection in {} ms: analogue {analogue:?}",
        frames * 20
    );
    assert_eq!(analogue.bit_rate(), digital.bit_rate());
    let message: Vec<bool> = (0..40_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
    analogue.push_bits(&message);
    digital.push_bits(&message);
    let (at_analogue, at_digital) = exchange(&mut analogue, &mut digital, 100, &line);
    let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
    assert!(
        found(&at_analogue),
        "{} bit/s down would lose data",
        analogue.bit_rate()
    );
    assert!(found(&at_digital), "V.34 up would lose data");
    analogue.bit_rate()
}

fn connected_pair() -> (Analogue, Digital) {
    let mut analogue = Analogue::new();
    let mut digital = Digital::new();
    for _ in 0..2000 {
        if analogue.connected() && digital.connected() {
            break;
        }
        exchange(&mut analogue, &mut digital, 1, &alaw);
    }
    assert!(analogue.connected() && digital.connected());
    (analogue, digital)
}

// Whether data crosses down and up.
fn carries_data(analogue: &mut Analogue, digital: &mut Digital) -> (bool, bool) {
    let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
    analogue.push_bits(&message);
    digital.push_bits(&message);
    let (at_analogue, at_digital) = exchange(analogue, digital, 60, &alaw);
    let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
    (found(&at_analogue), found(&at_digital))
}

// As `exchange`, with the lines up and down apart.
fn exchange_apart(
    analogue: &mut Analogue,
    digital: &mut Digital,
    frames: usize,
    up_line: &impl Fn(i16) -> i16,
    down_line: &impl Fn(i16) -> i16,
) -> (Vec<bool>, Vec<bool>) {
    let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
    let (mut at_analogue, mut at_digital) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        analogue.transmit(&mut up);
        digital.transmit(&mut down);
        digital.receive(&up.map(up_line), &mut at_digital);
        analogue.receive(&down.map(down_line), &mut at_analogue);
    }
    (at_analogue, at_digital)
}

// A-law, with up to `amplitude` of noise added before it, as a path that decodes and codes again.
fn noisy(amplitude: i32) -> impl Fn(i16) -> i16 {
    let state = std::cell::Cell::new(0x1234_5678_u32);
    move |sample| {
        let next = state
            .get()
            .wrapping_mul(1_664_525)
            .wrapping_add(1_013_904_223);
        state.set(next);
        let added = i32::try_from(next >> 16).unwrap_or(0) % (2 * amplitude + 1) - amplitude;
        let sum = (i32::from(alaw(sample)) + added).clamp(-32_768, 32_767);
        alaw(i16::try_from(sum).unwrap_or(0))
    }
}

#[test]
fn renegotiates_down_when_data_mode_strays_more_than_dil_showed() {
    let (mut analogue, mut digital) = connected_pair();
    assert_eq!(analogue.bit_rate(), 56_000);
    let line = noisy(100);
    exchange_apart(&mut analogue, &mut digital, 400, &alaw, &line);
    assert!(
        analogue.connected() && digital.connected(),
        "no connection after the renegotiation"
    );
    let rate = analogue.bit_rate();
    assert!(rate < 56_000, "still at {rate} bit/s on a noisy path");
    assert_eq!(rate, digital.bit_rate());
    let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
    analogue.push_bits(&message);
    digital.push_bits(&message);
    let (at_analogue, _) = exchange_apart(&mut analogue, &mut digital, 150, &alaw, &line);
    assert!(
        at_analogue.windows(message.len()).any(|w| w == message),
        "{rate} bit/s would lose data on that path"
    );
}

// A VoIP provider's path as real calls met it: see "Wire" in docs/transport.md.
struct Provider {
    up: std::collections::VecDeque<i16>,
    down: std::collections::VecDeque<i16>,
    quiet: usize,
    gateway_after: usize,
    gateway: bool,
    noise: i32,
    conceal: u32,
    state: u32,
    frames: usize,
    concealed_up: Vec<usize>,
    concealed_down: Vec<usize>,
}

impl Provider {
    fn new(
        delay: usize,
        gateway_after: usize,
        noise: i32,
        conceal_per_mille: u32,
        seed: u32,
    ) -> Self {
        Self {
            up: std::collections::VecDeque::from(vec![0; delay]),
            down: std::collections::VecDeque::from(vec![0; delay]),
            quiet: 0,
            gateway_after,
            gateway: false,
            noise,
            conceal: conceal_per_mille,
            state: seed.max(1),
            frames: 0,
            concealed_up: Vec::new(),
            concealed_down: Vec::new(),
        }
    }

    fn random(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        self.state
    }

    #[expect(clippy::cast_precision_loss, reason = "a frame of samples")]
    fn carry(&mut self, up: &[i16; FRAME], down: &[i16; FRAME]) -> ([i16; FRAME], [i16; FRAME]) {
        let power = up.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / FRAME as f64;
        self.quiet = if power.sqrt() < 30.0 {
            self.quiet + FRAME
        } else {
            0
        };
        self.gateway |= self.quiet >= self.gateway_after;
        let (mut up, mut down) = (*up, *down);
        if self.gateway {
            for sample in &mut down {
                let most = self.noise.unsigned_abs() * 2 + 1;
                let added = i32::try_from(self.random() % most).unwrap_or(0) - self.noise;
                let sum = (i32::from(alaw(*sample)) + added).clamp(-32_768, 32_767);
                *sample = i16::try_from(sum).unwrap_or(0);
            }
        }
        for (frame, concealed) in [
            (&mut up, &mut self.concealed_up),
            (&mut down, &mut self.concealed_down),
        ] {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 17;
            self.state ^= self.state << 5;
            if self.state % 1000 < self.conceal {
                frame.fill(0);
                concealed.push(self.frames);
            }
        }
        self.frames += 1;
        self.up.extend(up.map(alaw));
        self.down.extend(down.map(alaw));
        let heard_up: Vec<i16> = self.up.drain(..FRAME).collect();
        let heard_down: Vec<i16> = self.down.drain(..FRAME).collect();
        (
            heard_up.try_into().unwrap_or([0; FRAME]),
            heard_down.try_into().unwrap_or([0; FRAME]),
        )
    }
}

// Frames through `provider`, and the data each end hears.
fn through(
    provider: &mut Provider,
    analogue: &mut Analogue,
    digital: &mut Digital,
    frames: usize,
) -> (Vec<bool>, Vec<bool>) {
    let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
    let (mut at_analogue, mut at_digital) = (Vec::new(), Vec::new());
    for _ in 0..frames {
        analogue.transmit(&mut up);
        digital.transmit(&mut down);
        let (heard_up, heard_down) = provider.carry(&up, &down);
        digital.receive(&heard_up, &mut at_digital);
        analogue.receive(&heard_down, &mut at_analogue);
    }
    (at_analogue, at_digital)
}

#[test]
fn connects_and_carries_data_through_a_path_like_a_providers() {
    let mut failed = Vec::new();
    // Seeds 6, 8 and 12 lose INFO1d, Sd and Ed.
    for seed in [1, 6, 8, 12] {
        let mut provider = Provider::new(1120, 2400, 100, 2, seed);
        let mut analogue = Analogue::new();
        let mut digital = Digital::asking_sixteen_points();
        let up = (0..2500).find(|_| {
            through(&mut provider, &mut analogue, &mut digital, 1);
            analogue.connected() && digital.connected()
        });
        through(&mut provider, &mut analogue, &mut digital, 300);
        // A lost frame in the message would spoil it however well the modems recover.
        provider.conceal = 0;
        let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
        analogue.push_bits(&message);
        digital.push_bits(&message);
        let (at_analogue, at_digital) = through(&mut provider, &mut analogue, &mut digital, 150);
        let found = |bits: &[bool]| bits.windows(message.len()).any(|w| w == message);
        if up.is_none() || !found(&at_analogue) || !found(&at_digital) {
            failed.push((
                seed,
                up,
                analogue.bit_rate(),
                found(&at_analogue),
                found(&at_digital),
                provider.concealed_up.clone(),
                provider.concealed_down.clone(),
            ));
        }
    }
    assert!(
        failed.is_empty(),
        "(seed, connected at frame, bit/s, data down, data up, frames concealed up, down) that failed: \
         {failed:?}"
    );
}

#[test]
fn connects_at_the_lowest_rates_when_dil_shows_noise() {
    for (amplitude, least, most) in [
        (100, 28_000, 33_333),
        (120, 28_000, 29_333),
        (400, 28_000, 28_000),
    ] {
        let line = noisy(amplitude);
        let mut analogue = Analogue::new();
        let mut digital = Digital::new();
        let up = (0..2000).find(|_| {
            exchange_apart(&mut analogue, &mut digital, 1, &alaw, &line);
            analogue.connected() && digital.connected()
        });
        let rate = analogue.bit_rate();
        assert!(up.is_some(), "no connection with {amplitude} of noise down");
        assert!(
            (least..=most).contains(&rate),
            "{rate} bit/s with {amplitude} of noise down"
        );
        if amplitude < 400 {
            let message: Vec<bool> = (0..20_000).map(|n| n % 7 < 3 || n % 13 == 0).collect();
            analogue.push_bits(&message);
            digital.push_bits(&message);
            let (at_analogue, _) = exchange_apart(&mut analogue, &mut digital, 60, &alaw, &line);
            assert!(
                at_analogue.windows(message.len()).any(|w| w == message),
                "{rate} bit/s would lose data with {amplitude} of noise down"
            );
        }
    }
}

#[test]
fn keeps_its_rate_while_data_mode_stays_within_dil() {
    let (mut analogue, mut digital) = connected_pair();
    exchange_apart(&mut analogue, &mut digital, 400, &alaw, &alaw);
    assert_eq!((analogue.bit_rate(), digital.bit_rate()), (56_000, 56_000));
    assert!(analogue.connected() && digital.connected());
}

#[derive(Debug, Clone, Copy)]
enum Recovery {
    Renegotiation,
    Retrain,
}

// `recovery` from either end, after a downstream slip if `slip`, back to 56 000 bit/s with DCD held.
fn recovers_from_either_end(recovery: Recovery, slip: bool) {
    for from_analogue in [true, false] {
        let end = if from_analogue { "analogue" } else { "digital" };
        let (mut analogue, mut digital) = connected_pair();
        if slip {
            let mut lost = [0; FRAME];
            for _ in 0..2 {
                digital.transmit(&mut lost);
            }
            assert_eq!(carries_data(&mut analogue, &mut digital), (false, true));
        }
        let from: &mut dyn DataPump = if from_analogue {
            &mut analogue
        } else {
            &mut digital
        };
        match recovery {
            Recovery::Renegotiation => from.renegotiate(),
            Recovery::Retrain => from.retrain(),
        }
        assert!(!(analogue.connected() && digital.connected()));
        let back = (0..1500).find(|_| {
            exchange(&mut analogue, &mut digital, 1, &alaw);
            assert!(
                analogue.carrier() && digital.carrier(),
                "DCD would drop during a {recovery:?} from the {end} modem"
            );
            analogue.connected() && digital.connected()
        });
        assert!(
            back.is_some(),
            "a {recovery:?} from the {end} modem would not end"
        );
        assert_eq!((analogue.bit_rate(), digital.bit_rate()), (56_000, 56_000));
        assert_eq!(
            carries_data(&mut analogue, &mut digital),
            (true, true),
            "data down and up after a {recovery:?} from the {end} modem"
        );
    }
}

#[test]
fn renegotiates_from_either_end_and_carries_data_after() {
    recovers_from_either_end(Recovery::Renegotiation, false);
}

#[test]
fn a_renegotiation_finds_the_data_frames_again_after_a_slip() {
    recovers_from_either_end(Recovery::Renegotiation, true);
}

#[test]
fn retrains_from_either_end_after_a_slip_and_carries_data_after() {
    recovers_from_either_end(Recovery::Retrain, true);
}

#[test]
fn clears_down_from_either_end() {
    for from_analogue in [true, false] {
        let (mut analogue, mut digital) = connected_pair();
        if from_analogue {
            analogue.clear_down();
        } else {
            digital.clear_down();
        }
        let cleared = (0..500).find(|_| {
            exchange(&mut analogue, &mut digital, 1, &alaw);
            analogue.cleared() && digital.cleared()
        });
        assert!(
            cleared.is_some(),
            "a cleardown from the {} modem would leave the call up",
            if from_analogue { "analogue" } else { "digital" }
        );
        assert!(
            !analogue.carrier() && !digital.carrier(),
            "DCD would stay on after a cleardown"
        );
    }
}

#[test]
fn sends_cp_on_16_points_when_jd_asks() {
    for from_analogue in [true, false] {
        let mut analogue = Analogue::new();
        let mut digital = Digital::asking_sixteen_points();
        let up = (0..2000).find(|_| {
            exchange(&mut analogue, &mut digital, 1, &alaw);
            analogue.connected() && digital.connected()
        });
        assert!(up.is_some(), "no V.90 connection with CP on 16 points");
        assert_eq!(carries_data(&mut analogue, &mut digital), (true, true));
        if from_analogue {
            analogue.renegotiate();
        } else {
            digital.renegotiate();
        }
        let back = (0..1500).find(|_| {
            exchange(&mut analogue, &mut digital, 1, &alaw);
            analogue.connected() && digital.connected()
        });
        assert!(
            back.is_some(),
            "a renegotiation with CP on 16 points would not end"
        );
        assert_eq!(carries_data(&mut analogue, &mut digital), (true, true));
    }
}

// Connects with frame `lost` of the upstream lost, as over RTP, in at most 2000 frames.
fn connects_losing_upstream_frame(lost: usize) -> bool {
    let mut analogue = Analogue::new();
    let mut digital = Digital::asking_sixteen_points();
    let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
    for frame in 0..2000 {
        if analogue.connected() && digital.connected() {
            return true;
        }
        analogue.transmit(&mut up);
        digital.transmit(&mut down);
        if frame == lost {
            up = [0; FRAME];
        }
        digital.receive(&up.map(alaw), &mut Vec::new());
        analogue.receive(&down.map(alaw), &mut Vec::new());
    }
    false
}

// E cannot come twice, as B1 follows it at once, so only a retrain recovers it.
#[test]
fn connects_whichever_upstream_frame_of_phase_4_before_e_is_lost() {
    let mut analogue = Analogue::new();
    let mut digital = Digital::asking_sixteen_points();
    let mut phase_4 = Vec::new();
    for frame in 0..2000_usize {
        exchange(&mut analogue, &mut digital, 1, &alaw);
        if analogue.in_phase_4() {
            phase_4.push(frame);
        }
        if analogue.connected() && digital.connected() {
            break;
        }
    }
    let (Some(&from), Some(&to)) = (phase_4.first(), phase_4.last()) else {
        panic!("no V.90 phase 4 to lose frames from");
    };
    let failed: Vec<usize> = (from..=to)
        .filter(|&lost| !connects_losing_upstream_frame(lost))
        .collect();
    assert!(
        failed.is_empty(),
        "losing upstream frame {failed:?} of phase 4, frames {from} to {to}, leaves no connection"
    );
}

#[test]
fn an_analogue_and_a_digital_modem_carry_data_at_56000_bit_s() {
    assert_eq!(connect_over(alaw), 56_000);
}

#[test]
fn learns_the_levels_behind_a_digital_pad() {
    assert_eq!(connect_over(padded(10f64.powf(-3.0 / 20.0))), 56_000);
}

#[test]
fn connects_whatever_the_delay_each_way() {
    let mut failed = Vec::new();
    for (up_delay, down_delay) in [(1, 0), (0, 37), (160, 161), (555, 3), (1600, 1600)] {
        let mut analogue = Analogue::new();
        let mut digital = Digital::new();
        let mut lines = [
            std::collections::VecDeque::from(vec![0; up_delay]),
            std::collections::VecDeque::from(vec![0; down_delay]),
        ];
        let (mut up, mut down) = ([0; FRAME], [0; FRAME]);
        let mut frames = 0;
        while !(analogue.connected() && digital.connected()) && frames < 2000 {
            analogue.transmit(&mut up);
            digital.transmit(&mut down);
            lines[0].extend(up.map(alaw));
            lines[1].extend(down.map(alaw));
            let heard: Vec<i16> = lines[0].drain(..FRAME).collect();
            digital.receive(&heard, &mut Vec::new());
            let heard: Vec<i16> = lines[1].drain(..FRAME).collect();
            analogue.receive(&heard, &mut Vec::new());
            frames += 1;
        }
        if !(analogue.connected() && digital.connected() && analogue.bit_rate() == 56_000) {
            failed.push((up_delay, down_delay));
        }
    }
    assert!(
        failed.is_empty(),
        "no 56 000 bit/s over lines delayed, up and down, by {failed:?} samples"
    );
}
