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
    assert!(found(&at_analogue), "{} bit/s down would lose data", analogue.bit_rate());
    assert!(found(&at_digital), "V.34 up would lose data");
    analogue.bit_rate()
}

#[test]
fn an_analogue_and_a_digital_modem_carry_data_at_56000_bit_s() {
    assert_eq!(connect_over(alaw), 56_000);
}

#[test]
fn learns_the_levels_behind_a_digital_pad() {
    assert_eq!(connect_over(padded(10f64.powf(-3.0 / 20.0))), 56_000);
}
