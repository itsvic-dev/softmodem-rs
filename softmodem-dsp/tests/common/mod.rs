//! Line impairments shared by the modulation tests.

#![allow(dead_code)]

use softmodem_dsp::sine_peak;

pub struct Noise(pub u64);

impl Noise {
    pub fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        f64::from(u32::try_from(self.0 >> 32).unwrap()) / f64::from(u32::MAX) - 0.5
    }

    pub fn gaussian(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum()
    }
}

pub fn random_bytes(count: usize) -> Vec<u8> {
    let mut noise = Noise(0x2545_F491_4F6C_DD1D);
    (0..count)
        .map(|_| {
            noise.uniform();
            noise.0.to_le_bytes()[7]
        })
        .collect()
}

pub fn common_subsequence(a: &[u8], b: &[u8]) -> usize {
    let mut row = vec![0; b.len() + 1];
    for &x in a {
        let mut diagonal = 0;
        for (j, &y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if x == y {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

#[expect(clippy::cast_possible_truncation, reason = "clamped first")]
pub fn clamp(x: f64) -> i16 {
    x.round().clamp(-32768.0, 32767.0) as i16
}

/// Adds white noise `snr_db` below a sine at `level_dbm0`.
pub fn add_noise(samples: Vec<i16>, level_dbm0: f64, snr_db: f64) -> Vec<i16> {
    let sigma = sine_peak(level_dbm0) / 2f64.sqrt() / 10f64.powf(snr_db / 20.0);
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    samples
        .into_iter()
        .map(|s| clamp(f64::from(s) + sigma * noise.gaussian()))
        .collect()
}

pub fn scale(samples: Vec<i16>, gain: f64) -> Vec<i16> {
    samples
        .into_iter()
        .map(|s| clamp(f64::from(s) * gain))
        .collect()
}

pub fn mix(a: &[i16], b: &[i16]) -> Vec<i16> {
    a.iter()
        .zip(b.iter().chain(std::iter::repeat(&0)))
        .map(|(&a, &b)| clamp(f64::from(a) + f64::from(b)))
        .collect()
}

/// The samples as a sender whose clock runs `ratio` times ours would make them.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "sample positions are small and positive"
)]
pub fn resample(samples: &[i16], ratio: f64) -> Vec<i16> {
    let count = ((samples.len() - 1) as f64 / ratio) as usize;
    (0..count)
        .map(|n| {
            let position = n as f64 * ratio;
            let i = position as usize;
            let t = position - i as f64;
            let a = f64::from(samples[i]);
            let b = f64::from(samples[i + 1]);
            clamp(a + (b - a) * t)
        })
        .collect()
}
