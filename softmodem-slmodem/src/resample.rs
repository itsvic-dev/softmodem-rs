//! Between 8000 and 9600 samples/s, through one windowed-sinc prototype at
//! 48 000 samples/s, the rate both reach.

use std::collections::VecDeque;
use std::f64::consts::PI;

const PROTOTYPE_RATE: f64 = 48_000.0;
// The V.34 band at 3429 baud reaches 3845 Hz, and 8000 samples/s folds at 4000 Hz.
const CUTOFF_HZ: f64 = 3900.0;
const PROTOTYPE_TAPS: usize = 1200;
const KAISER_BETA: f64 = 8.0;

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..30 {
        term *= (x / 2.0 / f64::from(k)).powi(2);
        sum += term;
    }
    sum
}

/// Takes samples at one rate and gives them at `up / down` times that rate.
#[derive(Debug)]
pub struct Resampler {
    up: u64,
    down: u64,
    phases: Vec<Vec<f64>>,
    history: VecDeque<f64>,
    consumed: u64,
    next: u64,
}

impl Resampler {
    /// `up` times the input rate must be 48 000 samples/s.
    #[must_use]
    #[expect(
        clippy::cast_precision_loss,
        reason = "the prototype has a few thousand taps"
    )]
    pub fn new(up: usize, down: usize) -> Self {
        let taps = PROTOTYPE_TAPS / up;
        let length = up * taps;
        let centre = (length - 1) as f64 / 2.0;
        let fraction = 2.0 * CUTOFF_HZ / PROTOTYPE_RATE;
        let prototype: Vec<f64> = (0..length)
            .map(|k| {
                let t = k as f64 - centre;
                let sinc = if t == 0.0 {
                    1.0
                } else {
                    (PI * fraction * t).sin() / (PI * fraction * t)
                };
                let x = t / centre;
                let window = bessel_i0(KAISER_BETA * (1.0 - x * x).max(0.0).sqrt())
                    / bessel_i0(KAISER_BETA);
                up as f64 * fraction * sinc * window
            })
            .collect();
        let phases = (0..up)
            .map(|phase| (0..taps).map(|j| prototype[phase + j * up]).collect())
            .collect();
        Self {
            up: up as u64,
            down: down as u64,
            phases,
            history: VecDeque::from(vec![0.0; taps]),
            consumed: 0,
            next: 0,
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the phase is below `up`, and samples are clamped first"
    )]
    pub fn process(&mut self, input: &[i16], output: &mut Vec<i16>) {
        for &sample in input {
            self.history.pop_front();
            self.history.push_back(f64::from(sample));
            self.consumed += 1;
            while self.next < self.consumed * self.up {
                let phase = &self.phases[(self.next % self.up) as usize];
                let value: f64 = phase
                    .iter()
                    .zip(self.history.iter().rev())
                    .map(|(h, x)| h * x)
                    .sum();
                output.push(value.round().clamp(-32_768.0, 32_767.0) as i16);
                self.next += self.down;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f64, rate: f64, samples: usize, amplitude: f64) -> Vec<i16> {
        #[expect(clippy::cast_possible_truncation, reason = "within i16")]
        (0..samples)
            .map(|n| (amplitude * (2.0 * PI * hz * f64::from(u32::try_from(n).unwrap()) / rate).sin()) as i16)
            .collect()
    }

    // The amplitude of `hz` in `samples`, skipping the filter's start.
    fn amplitude(samples: &[i16], hz: f64, rate: f64) -> f64 {
        let settled = &samples[samples.len() / 4..];
        let (re, im) = settled.iter().zip(0u32..).fold((0.0, 0.0), |(re, im), (&s, n)| {
            let angle = 2.0 * PI * hz * f64::from(n) / rate;
            (re + f64::from(s) * angle.cos(), im + f64::from(s) * angle.sin())
        });
        #[expect(clippy::cast_precision_loss, reason = "a few thousand samples")]
        let count = settled.len() as f64;
        2.0 * (re * re + im * im).sqrt() / count
    }

    #[test]
    fn keeps_the_modem_band_through_both_directions() {
        for hz in [300.0, 1200.0, 1959.0, 3700.0] {
            let mut up = Resampler::new(6, 5);
            let mut down = Resampler::new(5, 6);
            let mut high = Vec::new();
            up.process(&tone(hz, 8000.0, 16_000, 10_000.0), &mut high);
            assert_eq!(high.len(), 19_200, "9600 samples/s would drift from 8000");
            let mut back = Vec::new();
            down.process(&high, &mut back);
            assert_eq!(back.len(), 16_000);
            let level = amplitude(&back, hz, 8000.0);
            assert!(
                (level / 10_000.0 - 1.0).abs() < 0.01,
                "{hz} Hz would reach the far modem at {level:.0} of 10000"
            );
        }
    }

    #[test]
    fn keeps_what_would_fold_out_of_8000_samples_per_second() {
        let mut down = Resampler::new(5, 6);
        let mut low = Vec::new();
        down.process(&tone(4400.0, 9600.0, 19_200, 10_000.0), &mut low);
        let folded = amplitude(&low, 8000.0 - 4400.0, 8000.0);
        assert!(
            folded < 10.0,
            "4400 Hz from slmodemd would fold onto 3600 Hz at {folded:.1} of 10000"
        );
    }
}
