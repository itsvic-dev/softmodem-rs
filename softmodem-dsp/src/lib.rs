//! Modulators and demodulators over linear 8 kHz samples. No IO.

mod correlator;
pub mod dpsk;
pub mod fsk;
pub mod pump;
pub mod scrambler;
pub mod tone;
pub mod uart;
mod v21;
mod v22;

pub const SAMPLE_RATE: f64 = 8000.0;

/// Peak amplitude on the i16 scale of a sine at `dbm0`. G.711 puts the A-law
/// overload point, 32256 after expansion, at +3.14 dBm0.
#[must_use]
#[expect(clippy::approx_constant, reason = "G.711's +3.14 dBm0, not pi")]
pub fn sine_peak(dbm0: f64) -> f64 {
    32256.0 * 10f64.powf((dbm0 - 3.14) / 20.0)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the i16 range first"
)]
fn to_sample(value: f64) -> i16 {
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}
