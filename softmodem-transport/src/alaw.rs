//! G.711 A-law, the only codec a modem call may use.

const SEGMENT_ENDS: [i32; 8] = [0x1F, 0x3F, 0x7F, 0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF];

#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the segment is below 8 and the step is masked to four bits"
)]
pub fn encode(sample: i16) -> u8 {
    let mut value = i32::from(sample) >> 3;
    let mask = if value >= 0 {
        0xD5
    } else {
        value = -value - 1;
        0x55
    };
    let Some(segment) = SEGMENT_ENDS.iter().position(|&end| value <= end) else {
        return 0x7F ^ mask;
    };
    let shift = if segment < 2 { 1 } else { segment };
    let step = ((value >> shift) & 0x0F) as u8;
    ((segment as u8) << 4 | step) ^ mask
}

#[must_use]
pub fn decode(code: u8) -> i16 {
    let code = code ^ 0x55;
    let segment = (code & 0x70) >> 4;
    let step = i16::from(code & 0x0F) << 4;
    let magnitude = match segment {
        0 => step + 8,
        _ => (step + 0x108) << (segment - 1),
    };
    if code & 0x80 == 0 {
        -magnitude
    } else {
        magnitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_survives_a_round_trip() {
        for code in 0..=255 {
            assert_eq!(
                decode(encode(decode(code))),
                decode(code),
                "code {code:#04x}"
            );
        }
    }

    #[test]
    fn decodes_the_g711_extremes() {
        assert_eq!(encode(0), 0xD5);
        assert_eq!(decode(0xD5), 8);
        assert_eq!(decode(0xAA), 32256);
        assert_eq!(decode(0x2A), -32256);
        assert_eq!(encode(i16::MAX), 0xAA);
        assert_eq!(encode(i16::MIN), 0x2A);
    }

    #[test]
    fn quantisation_error_stays_within_one_step() {
        for sample in (i16::MIN..=i16::MAX).step_by(7) {
            let error = (i32::from(decode(encode(sample))) - i32::from(sample)).abs();
            let step = (i32::from(sample).abs() / 16).max(16);
            assert!(error <= step, "sample {sample} came back {error} off");
        }
    }
}
