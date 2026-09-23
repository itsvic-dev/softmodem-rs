//! Modulators and demodulators over linear 8 kHz samples. No IO.

pub mod fsk;
pub mod uart;

pub const SAMPLE_RATE: f64 = 8000.0;

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the i16 range first"
)]
fn to_sample(value: f64) -> i16 {
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}
