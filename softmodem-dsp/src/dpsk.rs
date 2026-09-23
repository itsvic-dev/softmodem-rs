//! Differential 4-phase PSK at 600 baud, as V.22 uses it.

use std::collections::VecDeque;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI, SQRT_2, TAU};

use crate::{SAMPLE_RATE, sine_peak, to_sample};

/// The V.22 low channel, sent by the calling modem.
pub const V22_LOW_HZ: f64 = 1200.0;
/// The V.22 high channel, sent by the answering modem.
pub const V22_HIGH_HZ: f64 = 2400.0;
pub const V22_BAUD: f64 = 600.0;
/// The V.22 roll-off, from its § 2.4.
pub const ROLL_OFF: f64 = 0.75;

// Symbols each side of the one being sent that still shape the output.
const SPAN: usize = 3;

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

// `t` in symbols; the energy of the pulse is one symbol.
fn root_raised_cosine(t: f64) -> f64 {
    let b = ROLL_OFF;
    if t.abs() < 1e-9 {
        return 1.0 - b + 4.0 * b / PI;
    }
    if ((4.0 * b * t).abs() - 1.0).abs() < 1e-9 {
        let angle = PI / (4.0 * b);
        return b / SQRT_2 * ((1.0 + 2.0 / PI) * angle.sin() + (1.0 - 2.0 / PI) * angle.cos());
    }
    let numerator = (PI * t * (1.0 - b)).sin() + 4.0 * b * t * (PI * t * (1.0 + b)).cos();
    numerator / (PI * t * (1.0 - (4.0 * b * t).powi(2)))
}

/// Turns bits into shaped DPSK on one carrier. Its mean power is that of a
/// sine at the level it is given.
#[derive(Debug)]
pub struct Modulator {
    carrier_step: f64,
    carrier_phase: f64,
    symbol_step: f64,
    amplitude: f64,
    quadrant: u8,
    symbols: VecDeque<(f64, f64)>,
    elapsed: f64,
}

impl Modulator {
    #[must_use]
    pub fn new(carrier_hz: f64, level_dbm0: f64) -> Self {
        Self {
            carrier_step: carrier_hz / SAMPLE_RATE,
            carrier_phase: 0.0,
            symbol_step: V22_BAUD / SAMPLE_RATE,
            amplitude: sine_peak(level_dbm0),
            quadrant: 0,
            symbols: VecDeque::from(vec![(0.0, 0.0); 2 * SPAN + 1]),
            elapsed: 0.0,
        }
    }

    /// Fills `out`, taking two bits from `next_bit` for each symbol.
    pub fn render(&mut self, out: &mut [i16], mut next_bit: impl FnMut() -> bool) {
        for sample in out {
            if self.elapsed >= 1.0 {
                self.elapsed -= 1.0;
                let first = next_bit();
                let second = next_bit();
                self.quadrant = (self.quadrant + quarter_turns(first, second)) % 4;
                let angle = FRAC_PI_4 + FRAC_PI_2 * f64::from(self.quadrant);
                self.symbols.pop_front();
                self.symbols.push_back((angle.cos(), angle.sin()));
            }

            #[expect(clippy::cast_precision_loss, reason = "the window is 7 symbols")]
            let (re, im) =
                self.symbols
                    .iter()
                    .enumerate()
                    .fold((0.0, 0.0), |(re, im), (i, &(a, b))| {
                        let g = root_raised_cosine(self.elapsed + SPAN as f64 - i as f64);
                        (re + a * g, im + b * g)
                    });
            let angle = TAU * self.carrier_phase;
            *sample = to_sample(self.amplitude * (re * angle.cos() - im * angle.sin()));

            self.carrier_phase = (self.carrier_phase + self.carrier_step).fract();
            self.elapsed += self.symbol_step;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_pulse_has_the_energy_of_one_symbol() {
        let step = 1e-3;
        let energy: f64 = (-8000..=8000)
            .map(|i| root_raised_cosine(f64::from(i) * step).powi(2) * step)
            .sum();
        assert!((energy - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_pulse_is_continuous_where_the_formula_divides_by_zero() {
        let t = 1.0 / (4.0 * ROLL_OFF);
        assert!((root_raised_cosine(t + 1e-6) - root_raised_cosine(t)).abs() < 1e-4);
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
    }
}
