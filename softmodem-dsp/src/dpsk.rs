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

fn dibit(quarter_turns: u8) -> (bool, bool) {
    match quarter_turns {
        0 => (false, true),
        1 => (false, false),
        2 => (true, false),
        _ => (true, true),
    }
}

// V.22 § 3.3 and table 3.
const CARRIER_ON_DBM0: f64 = -43.0;
const CARRIER_OFF_DBM0: f64 = -48.0;
const CARRIER_ON_SAMPLES: u32 = 1200;
const CARRIER_OFF_SAMPLES: u32 = 136;
const LEVEL_WINDOW: usize = 40;
const TIMING_GAIN: f64 = 0.05;

type Complex = (f64, f64);

fn lerp(a: Complex, b: Complex, t: f64) -> Complex {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Turns DPSK on one carrier back into bits. It recovers symbol timing from
/// the signal, so the sender's clock may differ from ours, and it decodes each
/// symbol against the one before, so the carrier may be a few hertz off.
#[derive(Debug)]
pub struct Demodulator {
    carrier_step: f64,
    carrier_phase: f64,
    taps: Vec<f64>,
    mixed: VecDeque<Complex>,
    powers: VecDeque<f64>,
    power_sum: f64,
    symbol_step: f64,
    symbol_phase: f64,
    previous: Complex,
    middle: Complex,
    last_symbol: Complex,
    present: bool,
    carrier: bool,
    carrier_count: u32,
    carrier_on: f64,
    carrier_off: f64,
}

impl Demodulator {
    #[must_use]
    pub fn new(carrier_hz: f64) -> Self {
        let samples_per_symbol = SAMPLE_RATE / V22_BAUD;
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_precision_loss,
            reason = "the filter is a few dozen taps"
        )]
        let half = (SPAN as f64 * samples_per_symbol).round() as i32;
        let taps: Vec<f64> = (-half..=half)
            .map(|n| root_raised_cosine(f64::from(n) / samples_per_symbol))
            .collect();
        let gain: f64 = taps.iter().sum();
        let taps: Vec<f64> = taps.into_iter().map(|g| g / gain).collect();
        Self {
            carrier_step: carrier_hz / SAMPLE_RATE,
            carrier_phase: 0.0,
            mixed: VecDeque::from(vec![(0.0, 0.0); taps.len()]),
            taps,
            powers: VecDeque::from(vec![0.0; LEVEL_WINDOW]),
            power_sum: 0.0,
            symbol_step: V22_BAUD / SAMPLE_RATE,
            symbol_phase: 0.0,
            previous: (0.0, 0.0),
            middle: (0.0, 0.0),
            last_symbol: (0.0, 0.0),
            present: false,
            carrier: false,
            carrier_count: 0,
            carrier_on: sine_peak(CARRIER_ON_DBM0),
            carrier_off: sine_peak(CARRIER_OFF_DBM0),
        }
    }

    /// Whether carrier has been on the line long enough to count, with the
    /// V.22 thresholds and response times.
    #[must_use]
    pub fn carrier(&self) -> bool {
        self.carrier
    }

    /// Appends the bits found in `input` to `bits`, two for each symbol while
    /// any signal is on the line, before carrier detect agrees that it is.
    pub fn process(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        for &sample in input {
            let x = f64::from(sample);
            let angle = TAU * self.carrier_phase;
            self.carrier_phase = (self.carrier_phase + self.carrier_step).fract();
            self.mixed.pop_front();
            self.mixed.push_back((x * angle.cos(), -x * angle.sin()));
            let y = self
                .taps
                .iter()
                .zip(&self.mixed)
                .fold((0.0, 0.0), |(re, im), (g, (a, b))| (re + g * a, im + g * b));

            let power = y.0 * y.0 + y.1 * y.1;
            self.power_sum += power - self.powers.pop_front().unwrap_or_default();
            self.powers.push_back(power);
            #[expect(clippy::cast_precision_loss, reason = "the window is 40")]
            let level = 2.0 * (self.power_sum.max(0.0) / LEVEL_WINDOW as f64).sqrt();
            self.present = level >= self.carrier_off;
            self.track_carrier(level);

            self.clock(y, bits);
            self.previous = y;
        }
    }

    fn clock(&mut self, y: Complex, bits: &mut Vec<bool>) {
        let before = self.symbol_phase;
        let after = before + self.symbol_step;
        if before < 0.5 && after >= 0.5 {
            self.middle = lerp(self.previous, y, (0.5 - before) / self.symbol_step);
        }
        if after < 1.0 {
            self.symbol_phase = after;
            return;
        }
        let symbol = lerp(self.previous, y, (1.0 - before) / self.symbol_step);
        self.symbol_phase = after - 1.0;
        self.symbol(symbol, bits);
    }

    fn symbol(&mut self, symbol: Complex, bits: &mut Vec<bool>) {
        let last = self.last_symbol;
        self.last_symbol = symbol;

        // Gardner timing error.
        let step = (symbol.0 - last.0, symbol.1 - last.1);
        let energy = symbol.0 * symbol.0 + symbol.1 * symbol.1 + last.0 * last.0 + last.1 * last.1;
        if energy > 0.0 {
            let error = (self.middle.0 * step.0 + self.middle.1 * step.1) / energy;
            self.symbol_phase += TIMING_GAIN * error;
        }

        if !self.present {
            return;
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

    fn track_carrier(&mut self, level: f64) {
        let flipping = if self.carrier {
            level < self.carrier_off
        } else {
            level > self.carrier_on
        };
        if !flipping {
            self.carrier_count = 0;
            return;
        }
        self.carrier_count += 1;
        let needed = if self.carrier {
            CARRIER_OFF_SAMPLES
        } else {
            CARRIER_ON_SAMPLES
        };
        if self.carrier_count >= needed {
            self.carrier = !self.carrier;
            self.carrier_count = 0;
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
