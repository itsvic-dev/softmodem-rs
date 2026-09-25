// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Choosing a carrier and a data rate from what line probing and training
//! measured, for this modem's receiver: the 16-state trellis and minimum
//! shaping.

use super::SymbolRate;
use super::framing::Framing;
use super::info::Probe;
use super::probing::Probing;

// The decoder reaches 10⁻⁶ at 4 dB + 3.01 dB per bit per 2D symbol, in white noise.
pub(super) const DB_PER_BIT: f64 = 3.01;
pub(super) const DECODED_DB: f64 = 4.0;
// Probing only estimates the SNR, so it leaves 5 dB of margin.
const PROBED_MARGIN_DB: f64 = 5.0;
// The equaliser measures the SNR of the data path itself.
const TRAINED_MARGIN_DB: f64 = 2.0;
// The part of the band either side of the carrier that the tones must cover.
const BAND: f64 = 0.45;

/// The highest data rate, in multiples of 2400 bit/s, that `snr_db` from
/// line probing carries at `symbol_rate`, or 0 if none.
#[must_use]
pub fn max_rate(symbol_rate: SymbolRate, snr_db: f64) -> u8 {
    highest(symbol_rate, snr_db - PROBED_MARGIN_DB)
}

/// The highest data rate, in multiples of 2400 bit/s, that an equaliser
/// error of `mse`, against points of unit mean power, carries at
/// `symbol_rate`, or 0 if none.
#[must_use]
pub fn trained_rate(symbol_rate: SymbolRate, mse: f64) -> u8 {
    highest(symbol_rate, -10.0 * mse.log10() - TRAINED_MARGIN_DB)
}

/// How many dB an equaliser error of `mse` leaves above what `bit_rate`
/// needs at `symbol_rate`: below 0, the decoder loses bits.
#[must_use]
pub fn margin_db(symbol_rate: SymbolRate, bit_rate: u32, mse: f64) -> f64 {
    let bits = f64::from(bit_rate) / symbol_rate.baud();
    -10.0 * mse.log10() - DECODED_DB - DB_PER_BIT * bits
}

fn highest(symbol_rate: SymbolRate, snr_db: f64) -> u8 {
    let bits = (snr_db - DECODED_DB) / DB_PER_BIT;
    (1..=14u8)
        .rev()
        .find(|&n| {
            let bit_rate = u32::from(n) * 2400;
            Framing::new(symbol_rate, bit_rate, false).is_some()
                && f64::from(bit_rate) <= bits * symbol_rate.baud()
        })
        .unwrap_or(0)
}

/// What a linear equaliser would see across the band of one symbol rate and
/// carrier: the harmonic mean of the SNR of the probing tones in it, in dB.
#[must_use]
pub fn band_snr_db(probing: &Probing, symbol_rate: SymbolRate, high_carrier: bool) -> f64 {
    let carrier = symbol_rate.carrier_hz(high_carrier);
    let half = BAND * symbol_rate.baud();
    let (sum, count) = probing
        .tones
        .iter()
        .filter(|tone| (tone.hz - carrier).abs() <= half)
        .fold((0.0, 0.0), |(sum, count), tone| {
            (sum + 10f64.powf(-tone.snr_db / 10.0), count + 1.0)
        });
    if count == 0.0 {
        return f64::NEG_INFINITY;
    }
    -10.0 * (sum / count).log10()
}

/// The carrier, pre-emphasis and rate for the far transmitter at one symbol
/// rate, as INFO1 carries them, choosing between the carriers it allows. It
/// asks for no pre-emphasis, as the equaliser takes out the tilt of the line.
#[must_use]
pub fn probe(probing: &Probing, symbol_rate: SymbolRate, carriers: [bool; 2]) -> Probe {
    let best = [false, true]
        .into_iter()
        .filter(|&high| carriers[usize::from(high)])
        .map(|high| (high, band_snr_db(probing, symbol_rate, high)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    match best {
        Some((high_carrier, snr)) => Probe {
            high_carrier,
            pre_emphasis: 0,
            max_rate: max_rate(symbol_rate, snr),
        },
        None => Probe::default(),
    }
}

/// The rate mask of MP: bit n for (n + 1) · 2400 bit/s, set for each rate up
/// to `max` that table 8 has at `symbol_rate`.
#[must_use]
pub fn mask(symbol_rate: SymbolRate, max: u8) -> u16 {
    (1..=max.min(14)).fold(0, |mask, n| {
        if Framing::new(symbol_rate, u32::from(n) * 2400, false).is_some() {
            mask | 1 << (n - 1)
        } else {
            mask
        }
    })
}

/// The data rate both directions run at, in multiples of 2400 bit/s, as
/// § 11.4.1.1.3 has it for symmetric rates: the highest that both masks
/// enable and no maximum exceeds.
#[must_use]
pub fn agree(maxima: [u8; 4], masks: [u16; 2]) -> u8 {
    let ceiling = maxima.into_iter().min().unwrap_or(0);
    (1..=ceiling)
        .rev()
        .find(|&n| masks.iter().all(|mask| mask >> (n - 1) & 1 == 1))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v34::probing::ToneResult;

    fn flat(snr_db: f64) -> Probing {
        Probing {
            tones: crate::v34::probing::TONES
                .iter()
                .map(|&(hz, _)| ToneResult {
                    hz,
                    gain_db: 0.0,
                    snr_db,
                })
                .collect(),
            frequency_offset_hz: Some(0.0),
        }
    }

    #[test]
    fn a_clean_line_carries_the_top_rate_of_table_8() {
        assert_eq!(max_rate(SymbolRate::S3429, 60.0), 14);
        assert_eq!(max_rate(SymbolRate::S3200, 60.0), 13);
        assert_eq!(max_rate(SymbolRate::S2400, 60.0), 9);
    }

    #[test]
    fn a_noisy_line_carries_less() {
        let rate = max_rate(SymbolRate::S3200, 30.0);
        assert!(
            (5..=10).contains(&rate),
            "a 30 dB line would be judged to carry {rate} · 2400 bit/s"
        );
        assert_eq!(max_rate(SymbolRate::S3200, 5.0), 0);
    }

    #[test]
    fn a_dead_band_edge_pulls_the_rate_down() {
        let mut probing = flat(45.0);
        for tone in &mut probing.tones {
            if tone.hz > 3400.0 {
                tone.snr_db = 3.0;
            }
        }
        let clear = probe(&flat(45.0), SymbolRate::S3429, [true, true]);
        let cut = probe(&probing, SymbolRate::S3429, [true, true]);
        assert!(cut.max_rate < clear.max_rate);
        assert!(probe(&probing, SymbolRate::S3200, [true, true]).max_rate > cut.max_rate);
    }

    #[test]
    fn training_through_a_law_carries_33600_at_3429_baud() {
        let alaw_mse = 10f64.powf(-37.6 / 10.0);
        assert_eq!(trained_rate(SymbolRate::S3429, alaw_mse), 14);
        assert_eq!(
            trained_rate(SymbolRate::S3429, 10f64.powf(-34.0 / 10.0)),
            13
        );
        assert_eq!(trained_rate(SymbolRate::S3429, 0.1), 0);
    }

    #[test]
    fn a_law_leaves_4_db_at_33600() {
        let margin = margin_db(SymbolRate::S3429, 33_600, 10f64.powf(-37.6 / 10.0));
        assert!(
            (margin - 4.1).abs() < 0.1,
            "the error monitor would misjudge an A-law line: {margin:.2} dB"
        );
    }

    #[test]
    fn agrees_on_the_highest_rate_both_ends_have() {
        let ours = mask(SymbolRate::S3200, 13);
        let theirs = mask(SymbolRate::S3200, 10);
        assert_eq!(agree([13, 13, 11, 12], [ours, theirs]), 10);
        assert_eq!(agree([13, 13, 13, 13], [ours, ours]), 13);
        assert_eq!(agree([0, 13, 13, 13], [ours, ours]), 0);
    }
}
