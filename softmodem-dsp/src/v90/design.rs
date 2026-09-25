// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the analogue modem asks the digital modem for: the DIL in Ja, and
//! the constellations in CPt and CP, from the levels that DIL showed.

use super::cp::{Constellation, Cp};
use super::dil::Descriptor;
use super::encoder::FRAME;
use super::ucode::{self, COUNT, Law};

// Table 15, as RMS, for bits 33:37 of INFO0d from 0 (-0.5 dBm0) to 31 (-16 dBm0).
const POWER_LIMITS: [f64; 32] = [
    15124.0, 14276.0, 13480.0, 12724.0, 12012.0, 11340.0, 10708.0, 10108.0, 9544.0, 9008.0, 8504.0,
    8028.0, 7580.0, 7156.0, 6756.0, 6380.0, 6020.0, 5684.0, 5368.0, 5068.0, 4784.0, 4516.0, 4264.0,
    4024.0, 3800.0, 3588.0, 3388.0, 3196.0, 3020.0, 2852.0, 2692.0, 2540.0,
];
// SP, the most table 12 allows.
const DIL_SIGN_BITS: usize = 128;
/// Uchords, the eight groups of 16 Ucodes.
pub const UCHORDS: usize = 8;
// H1 to H8: segments of 120 symbols, shorter for the loud Uchords, down to 24 at full scale.
const DIL_LENGTHS: [u8; UCHORDS] = [19, 19, 19, 19, 19, 9, 3, 3];
// Uchord 8 comes once in DIL, its even Ucodes on the way up and its odd ones on the way down.
const LOUDEST: u8 = 112;
// Levels this many noise deviations apart leave symbol errors below 10⁻⁷.
const SPACING_SIGMAS: f64 = 11.0;
// Where DIL showed no noise at all, half the smallest A-law step.
const LEAST_SPACING: f64 = 8.0;
// Data mode rates, as D, from 56 000 bit/s down to 28 000.
const DATA_BITS: std::ops::RangeInclusive<usize> = 21..=42;
// CPt: 32 000 bit/s on 8 levels.
const TRAINING_BITS: usize = 24;

/// The most that the average power may be, squared, for the highest digital
/// modem power in bits 33:37 of INFO0d.
#[must_use]
pub fn power_limit(max_power: u8) -> f64 {
    POWER_LIMITS[usize::from(max_power).min(POWER_LIMITS.len() - 1)].powi(2)
}

/// § 8.5.2: the average power of the constellations `sets`, each the
/// squared levels of its labels, when K bits enter the modulus encoder.
#[must_use]
#[expect(clippy::cast_precision_loss, reason = "counts of at most 2⁴²")]
pub fn average_power(sets: &[Vec<f64>; FRAME], modulus_bits: usize) -> f64 {
    let total = 1u128 << modulus_bits;
    let mut remainder = total - 1;
    let mut weight = 1u128;
    let mut sum = 0.0;
    for set in sets {
        let modulus = set.len().max(1) as u128;
        let (label, next) = (remainder % modulus, remainder / modulus);
        for (j, &power) in (0u128..).zip(set) {
            let uses = match j.cmp(&label) {
                std::cmp::Ordering::Less => weight * (next + 1),
                std::cmp::Ordering::Equal => total - weight * (remainder - next),
                std::cmp::Ordering::Greater => weight * next,
            };
            sum += power * uses as f64;
        }
        weight *= modulus;
        remainder = next;
    }
    sum / (6.0 * total as f64)
}

// Pseudo-random from x¹⁶ + x¹⁴ + x¹³ + x¹¹ + 1, the second and fourth frames the ones before inverted.
fn dil_signs() -> Vec<bool> {
    let mut state: u16 = 0xACE1;
    let mut signs: Vec<bool> = (0..DIL_SIGN_BITS)
        .map(|_| {
            let bit = (state ^ state >> 2 ^ state >> 3 ^ state >> 5) & 1;
            state = state >> 1 | bit << 15;
            bit == 1
        })
        .collect();
    for k in (FRAME..2 * FRAME).chain(3 * FRAME..4 * FRAME) {
        signs[k] = !signs[k - FRAME];
    }
    signs
}

/// The DIL this modem asks for: every Ucode, with each sign at least twice
/// in each data frame interval. The Ucodes go from UINFO up to 127, down to
/// 0 and back up to UINFO, so the level never jumps, and TRN1d before it and
/// Ri after it are near UINFO too. The loud Uchords have short segments.
#[must_use]
pub fn descriptor(uinfo: u8) -> Descriptor {
    let uinfo = uinfo.clamp(1, COUNT - 1);
    let up = (uinfo..COUNT).filter(|&u| u < LOUDEST || u % 2 == 0);
    let down = (0..COUNT).rev().filter(|&u| u < LOUDEST || u % 2 == 1);
    Descriptor {
        signs: dil_signs(),
        pattern: vec![true],
        lengths: DIL_LENGTHS,
        references: [uinfo; UCHORDS],
        training: up.chain(down).chain(1..uinfo).collect(),
    }
}

/// What DIL showed: the level of each Ucode in each interval, and the noise.
#[derive(Debug, Clone)]
pub struct Levels {
    /// The received magnitude of each positive Ucode, by interval.
    pub levels: [[f64; COUNT as usize]; FRAME],
    /// The RMS of the noise around those levels, by Uchord, as a path may
    /// add more to some levels than to others.
    pub noise: [f64; UCHORDS],
}

impl Levels {
    /// As table 1 gives them, with no noise: a digital path that changes
    /// nothing.
    #[must_use]
    pub fn table(law: Law) -> Self {
        let row: [f64; COUNT as usize] =
            std::array::from_fn(|u| f64::from(ucode::linear(u8::try_from(u).unwrap_or(0), law)));
        Self {
            levels: [row; FRAME],
            noise: [0.0; UCHORDS],
        }
    }

    // Half the room a level needs from its neighbour, for the noise of its Uchord.
    fn margin(&self, ucode: u8) -> f64 {
        SPACING_SIGMAS / 2.0 * self.noise[usize::from(ucode / 16).min(UCHORDS - 1)]
    }
}

// `count` levels from the bottom, as far apart as `spacing` and their noise ask, the first clear of its other sign.
fn pick(levels: &Levels, interval: usize, count: usize, spacing: f64) -> Option<Vec<u8>> {
    let row = &levels.levels[interval];
    let mut chosen: Vec<u8> = Vec::with_capacity(count);
    for ucode in 0..COUNT {
        let level = row[usize::from(ucode)];
        let room = match chosen.last() {
            None => 2.0 * level >= spacing.max(2.0 * levels.margin(ucode)),
            Some(&last) => {
                let needed = spacing.max(levels.margin(last) + levels.margin(ucode));
                level - row[usize::from(last)] >= needed
            }
        };
        if room {
            chosen.push(ucode);
            if chosen.len() == count {
                return Some(chosen);
            }
        }
    }
    None
}

fn mask(ucodes: &[u8]) -> Constellation {
    ucodes.iter().fold(0, |mask, &u| mask | 1 << u)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "at most 128 levels"
)]
fn levels_for(modulus_bits: usize) -> usize {
    (2f64.powf(modulus_bits as f64 / FRAME as f64) - 1e-9).ceil() as usize
}

// Levels in each interval for D = `bits`, spaced as widely as the power limit allows.
fn constellations(levels: &Levels, bits: usize, law: Law, limit: f64) -> Option<[Vec<u8>; FRAME]> {
    let modulus_bits = bits.checked_sub(FRAME)?;
    let count = levels_for(modulus_bits);
    let least = LEAST_SPACING;
    let fits = |spacing: f64| -> Option<[Vec<u8>; FRAME]> {
        let sets: [Option<Vec<u8>>; FRAME] =
            std::array::from_fn(|i| pick(levels, i, count, spacing));
        let sets: [Vec<u8>; FRAME] = sets.map(Option::unwrap_or_default);
        if sets.iter().any(|set| set.len() < count) {
            return None;
        }
        let powers: [Vec<f64>; FRAME] = std::array::from_fn(|i| {
            sets[i]
                .iter()
                .rev()
                .map(|&u| f64::from(ucode::linear(u, law)).powi(2))
                .collect()
        });
        (average_power(&powers, modulus_bits) <= limit).then_some(sets)
    };
    let mut best = fits(least)?;
    let (mut low, mut high) = (least, 32768.0);
    for _ in 0..40 {
        let middle = f64::midpoint(low, high);
        if let Some(sets) = fits(middle) {
            best = sets;
            low = middle;
        } else {
            high = middle;
        }
    }
    Some(best)
}

fn cp(sets: &[Vec<u8>; FRAME], bits: usize, training: bool, law: Law, upstream_rates: u16) -> Cp {
    let offset = if training { 8 } else { 20 };
    let mut constellations: Vec<Constellation> = Vec::new();
    let mut intervals = [0u8; FRAME];
    for (interval, set) in sets.iter().enumerate() {
        let constellation = mask(set);
        let index = constellations
            .iter()
            .position(|&c| c == constellation)
            .unwrap_or_else(|| {
                constellations.push(constellation);
                constellations.len() - 1
            });
        intervals[interval] = u8::try_from(index).unwrap_or(0);
    }
    Cp {
        training,
        rate: u8::try_from(bits - offset).unwrap_or(0),
        silence: false,
        redundancy: 0,
        acknowledge: false,
        law,
        upstream_rates,
        lookahead: 0,
        rms_ratio: 1 << 13,
        filter: [0; 4],
        intervals,
        constellations,
        at_codec: None,
    }
}

/// CPt: 8 levels in each interval, as far apart as the power limit allows.
#[must_use]
pub fn training(levels: &Levels, law: Law, max_power: u8, upstream_rates: u16) -> Option<Cp> {
    let sets = constellations(levels, TRAINING_BITS, law, power_limit(max_power))?;
    Some(cp(&sets, TRAINING_BITS, true, law, upstream_rates))
}

/// CP: the highest rate whose levels are far enough apart for the noise,
/// within the power limit.
#[must_use]
pub fn data(levels: &Levels, law: Law, max_power: u8, upstream_rates: u16) -> Option<Cp> {
    let limit = power_limit(max_power);
    DATA_BITS.rev().find_map(|bits| {
        let sets = constellations(levels, bits, law, limit)?;
        Some(cp(&sets, bits, false, law, upstream_rates))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::encoder::Mapping;

    #[test]
    fn gives_the_limits_of_table_15() {
        assert!((power_limit(0).sqrt() - 15124.0).abs() < 1e-9);
        assert!((power_limit(23).sqrt() - 4024.0).abs() < 1e-9);
        assert!((power_limit(31).sqrt() - 2540.0).abs() < 1e-9);
    }

    #[test]
    fn averages_equally_used_levels() {
        let sets: [Vec<f64>; FRAME] = std::array::from_fn(|_| vec![4.0, 1.0]);
        assert!((average_power(&sets, 6) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn counts_the_labels_as_the_modulus_encoder_reaches_them() {
        let three = |values: [f64; 3]| -> [Vec<f64>; FRAME] {
            std::array::from_fn(|i| if i < 2 { values.to_vec() } else { vec![0.0] })
        };
        assert!((average_power(&three([1.0, 0.0, 0.0]), 3) - 6.0 / 48.0).abs() < 1e-12);
        assert!((average_power(&three([0.0, 0.0, 1.0]), 3) - 4.0 / 48.0).abs() < 1e-12);
    }

    #[test]
    fn reaches_56000_bit_s_on_a_path_that_changes_nothing() {
        let levels = Levels::table(Law::A);
        let cp = data(&levels, Law::A, 23, 0x1FFF).expect("a CP");
        assert_eq!(cp.bit_rate(), 56_000);
        let mapping = Mapping::from_cp(&cp).expect("CP carries its rate");
        assert!(mapping.sets.iter().all(|set| set.len() == 64));
        let powers: [Vec<f64>; FRAME] = std::array::from_fn(|i| {
            mapping.sets[i]
                .iter()
                .map(|&u| f64::from(ucode::linear(u, Law::A)).powi(2))
                .collect()
        });
        assert!(average_power(&powers, mapping.modulus_bits) <= power_limit(23));
    }

    #[test]
    fn falls_back_as_the_noise_grows() {
        let mut levels = Levels::table(Law::A);
        levels.noise = [40.0; UCHORDS];
        let noisy = data(&levels, Law::A, 23, 0x1FFF).expect("a CP");
        assert!((28_000..56_000).contains(&noisy.bit_rate()));
        levels.noise = [5000.0; UCHORDS];
        assert!(data(&levels, Law::A, 23, 0x1FFF).is_none());
    }

    #[test]
    fn spaces_each_level_for_the_noise_of_its_uchord() {
        let mut levels = Levels::table(Law::A);
        levels.noise = [80.0; UCHORDS];
        let everywhere = data(&levels, Law::A, 23, 0x1FFF).expect("a CP");
        levels.noise = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 80.0, 80.0];
        let loud = data(&levels, Law::A, 23, 0x1FFF).expect("a CP");
        assert!(loud.bit_rate() > everywhere.bit_rate());
        let mapping = Mapping::from_cp(&loud).expect("CP carries its rate");
        for set in &mapping.sets {
            let mut ucodes = set.clone();
            ucodes.sort_unstable();
            for pair in ucodes.windows(2).filter(|pair| pair[0] >= 96) {
                let gap = ucode::linear(pair[1], Law::A) - ucode::linear(pair[0], Law::A);
                assert!(f64::from(gap) >= SPACING_SIGMAS * 80.0);
            }
        }
    }

    #[test]
    fn trains_on_8_levels_within_the_limit() {
        let cpt = training(&Levels::table(Law::A), Law::A, 23, 0x1FFF).expect("a CPt");
        assert_eq!(cpt.bit_rate(), 32_000);
        assert!(Mapping::from_cp(&cpt).is_some_and(|m| m.sets.iter().all(|s| s.len() == 8)));
    }

    #[test]
    fn asks_for_each_ucode_with_each_sign_twice_in_each_interval() {
        let descriptor = descriptor(75);
        assert!(descriptor.training.len() <= 255);
        let mut ucodes = descriptor.training.clone();
        ucodes.sort_unstable();
        ucodes.dedup();
        assert_eq!(ucodes.len(), usize::from(COUNT));
        for h in DIL_LENGTHS {
            let length = (usize::from(h) + 1) * FRAME;
            for interval in 0..FRAME {
                let signs: Vec<bool> = (interval..length)
                    .step_by(FRAME)
                    .map(|k| descriptor.signs[k % descriptor.signs.len()])
                    .collect();
                let positive = signs.iter().filter(|&&sign| sign).count();
                assert!(positive >= 2 && signs.len() - positive >= 2);
            }
        }
    }

    #[test]
    fn moves_through_the_ucodes_in_small_steps() {
        let training = descriptor(75).training;
        assert_eq!((training[0], *training.last().unwrap_or(&0)), (75, 74));
        assert!(training.windows(2).all(|w| w[0].abs_diff(w[1]) <= 2));
        assert_eq!(training.iter().filter(|&&u| u >= LOUDEST).count(), 16);
    }

    #[test]
    fn keeps_the_loud_part_short() {
        let descriptor = descriptor(75);
        let length = |u: u8| (usize::from(descriptor.lengths[usize::from(u / 16)]) + 1) * FRAME;
        let loud: usize = descriptor
            .training
            .iter()
            .filter(|&&u| u >= 112)
            .map(|&u| length(u))
            .sum();
        assert!(loud <= 400, "{loud} symbols of DIL near full scale");
    }
}
