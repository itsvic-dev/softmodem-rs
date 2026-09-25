// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Differential 4-phase PSK at 600 baud, as V.22 uses it.

use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

use crate::passband::{Receiver, Transmitter};

/// The V.22 low channel, sent by the calling modem.
pub const V22_LOW_HZ: f64 = 1200.0;
/// The V.22 high channel, sent by the answering modem.
pub const V22_HIGH_HZ: f64 = 2400.0;

// V.22 table 3: off in 10 to 24 ms.
const CARRIER_OFF_SAMPLES: u32 = 136;

/// The quarter turns a dibit advances the phase by, from table 1 of V.22.
/// The first bit of the pair is the one sent first.
#[must_use]
pub fn quarter_turns(first: bool, second: bool) -> u8 {
    match (first, second) {
        (false, false) => 1,
        (false, true) => 0,
        (true, true) => 3,
        (true, false) => 2,
    }
}

pub(crate) fn dibit(quarter_turns: u8) -> (bool, bool) {
    match quarter_turns % 4 {
        0 => (false, true),
        1 => (false, false),
        2 => (true, false),
        _ => (true, true),
    }
}

/// Turns bits into shaped DPSK on one carrier. Its mean power is that of a
/// sine at the level it is given.
#[derive(Debug)]
pub struct Modulator {
    transmitter: Transmitter,
    quadrant: u8,
}

impl Modulator {
    #[must_use]
    pub fn new(carrier_hz: f64, level_dbm0: f64) -> Self {
        Self {
            transmitter: Transmitter::new(carrier_hz, level_dbm0),
            quadrant: 0,
        }
    }

    /// Fills `out`, taking two bits from `next_bit` for each symbol.
    pub fn render(&mut self, out: &mut [i16], mut next_bit: impl FnMut() -> bool) {
        let quadrant = &mut self.quadrant;
        self.transmitter.render(out, || {
            let first = next_bit();
            let second = next_bit();
            *quadrant = (*quadrant + quarter_turns(first, second)) % 4;
            let angle = FRAC_PI_4 + FRAC_PI_2 * f64::from(*quadrant);
            (angle.cos(), angle.sin())
        });
    }
}

/// Turns DPSK on one carrier back into bits. It recovers symbol timing from
/// the signal, so the sender's clock may differ from ours, and it decodes each
/// symbol against the one before, so the carrier may be a few hertz off.
#[derive(Debug)]
pub struct Demodulator {
    receiver: Receiver,
    last_symbol: (f64, f64),
}

impl Demodulator {
    #[must_use]
    pub fn new(carrier_hz: f64) -> Self {
        Self {
            receiver: Receiver::new(carrier_hz, CARRIER_OFF_SAMPLES),
            last_symbol: (0.0, 0.0),
        }
    }

    /// Whether carrier has been on the line long enough to count, with the
    /// V.22 thresholds and response times.
    #[must_use]
    pub fn carrier(&self) -> bool {
        self.receiver.carrier()
    }

    /// Appends the bits found in `input` to `bits`, two for each symbol while
    /// any signal is on the line, before carrier detect agrees that it is.
    pub fn process(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        for &sample in input {
            let Some(strobe) = self.receiver.push(sample) else {
                continue;
            };
            let (symbol, last) = (strobe.symbol, self.last_symbol);
            self.last_symbol = symbol;
            if !strobe.present {
                continue;
            }
            let turn = (
                symbol.0 * last.0 + symbol.1 * last.1,
                symbol.1 * last.0 - symbol.0 * last.1,
            );
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "rounded and wrapped into 0 to 3 first"
            )]
            let quarter = ((turn.1.atan2(turn.0) / FRAC_PI_2).round().rem_euclid(4.0)) as u8;
            let (first, second) = dibit(quarter);
            bits.push(first);
            bits.push(second);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::TAU;

    use super::*;
    use crate::{SAMPLE_RATE, sine_peak};

    const LEVEL: f64 = -13.0;

    fn random_bits() -> impl FnMut() -> bool {
        let mut state = 1u32;
        move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state & 1 == 1
        }
    }

    fn signal(carrier_hz: f64) -> Vec<i16> {
        let mut modulator = Modulator::new(carrier_hz, LEVEL);
        let mut out = vec![0; 16_000];
        modulator.render(&mut out, random_bits());
        out
    }

    #[expect(clippy::cast_precision_loss, reason = "a short test signal")]
    fn mean_power(samples: &[i16]) -> f64 {
        samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64
    }

    #[expect(clippy::cast_precision_loss, reason = "a short test signal")]
    fn power_between(samples: &[i16], low_hz: f64, high_hz: f64) -> f64 {
        let n = samples.len() as f64;
        let bin_hz = SAMPLE_RATE / n;
        let mut total = 0.0;
        let mut hz = low_hz;
        while hz < high_hz {
            let (re, im) = samples
                .iter()
                .enumerate()
                .fold((0.0, 0.0), |(re, im), (i, &s)| {
                    let angle = TAU * hz * i as f64 / SAMPLE_RATE;
                    let x = f64::from(s);
                    (re + x * angle.cos(), im - x * angle.sin())
                });
            total += 2.0 * (re * re + im * im) / (n * n);
            hz += bin_hz;
        }
        total
    }

    #[test]
    fn the_mean_power_is_that_of_a_sine_at_the_level() {
        let expected = sine_peak(LEVEL).powi(2) / 2.0;
        for carrier in [V22_LOW_HZ, V22_HIGH_HZ] {
            let db = 10.0 * (mean_power(&signal(carrier)[800..]) / expected).log10();
            assert!(db.abs() < 0.5, "{carrier} Hz is {db:.2} dB off its level");
        }
    }

    #[test]
    fn each_channel_stays_in_its_band() {
        let low = signal(V22_LOW_HZ);
        let high = signal(V22_HIGH_HZ);
        let low_band = (675.0, 1725.0);
        let high_band = (1875.0, 2925.0);
        let ratio = |s: &[i16], inside: (f64, f64), outside: (f64, f64)| {
            10.0 * (power_between(s, inside.0, inside.1) / power_between(s, outside.0, outside.1))
                .log10()
        };
        let low_db = ratio(&low[..4000], low_band, high_band);
        let high_db = ratio(&high[..4000], high_band, low_band);
        assert!(
            low_db > 30.0,
            "the low channel leaks into the high: {low_db:.1} dB"
        );
        assert!(
            high_db > 30.0,
            "the high channel leaks into the low: {high_db:.1} dB"
        );
    }

    #[test]
    fn each_dibit_turns_the_phase_as_table_1_says() {
        assert_eq!(quarter_turns(false, false), 1);
        assert_eq!(quarter_turns(false, true), 0);
        assert_eq!(quarter_turns(true, true), 3);
        assert_eq!(quarter_turns(true, false), 2);
        for (first, second) in [(false, false), (false, true), (true, true), (true, false)] {
            assert_eq!(dibit(quarter_turns(first, second)), (first, second));
        }
    }
}
