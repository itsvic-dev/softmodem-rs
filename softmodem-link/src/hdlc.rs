//! HDLC framing as V.42 § 8.1 uses it: flags, zero-bit insertion, aborts and
//! the 16-bit FCS.

use std::collections::VecDeque;

pub const FLAG: u8 = 0x7e;

const FCS_INIT: u16 = 0xffff;
// x^16 + x^12 + x^5 + 1, with the register shifted LSB first.
const FCS_POLY: u16 = 0x8408;
// The remainder a receiver is left with over a frame and its FCS (§ 8.1.1.6.1).
const FCS_GOOD: u16 = 0xf0b8;
const FCS_LEN: usize = 2;
// Address and one control octet, the shortest a frame can be (§ 8.1.3).
const MIN_CONTENT: usize = 2;
// Far longer than any frame LAPM sends; past this the frame is unbounded.
const MAX_BITS: usize = 8 * 4200;

fn fcs_update(mut fcs: u16, bytes: &[u8]) -> u16 {
    for &byte in bytes {
        fcs ^= u16::from(byte);
        for _ in 0..8 {
            fcs = if fcs & 1 == 1 {
                fcs >> 1 ^ FCS_POLY
            } else {
                fcs >> 1
            };
        }
    }
    fcs
}

/// The 16-bit FCS of `bytes`, in the order it is sent.
#[must_use]
pub fn fcs16(bytes: &[u8]) -> [u8; 2] {
    (!fcs_update(FCS_INIT, bytes)).to_le_bytes()
}

pub fn push_flag(out: &mut VecDeque<bool>) {
    out.extend((0..8).map(|i| FLAG >> i & 1 == 1));
}

/// Appends `frame` with its FCS and a closing flag. The flag before it,
/// opening or closing the last frame, is the caller's.
pub fn push_frame(frame: &[u8], out: &mut VecDeque<bool>) {
    let fcs = fcs16(frame);
    let mut ones = 0;
    for &byte in frame.iter().chain(&fcs) {
        for i in 0..8 {
            let bit = byte >> i & 1 == 1;
            out.push_back(bit);
            ones = if bit { ones + 1 } else { 0 };
            if ones == 5 {
                out.push_back(false);
                ones = 0;
            }
        }
    }
    push_flag(out);
}

/// Finds frames in a bit stream and checks their FCS.
#[derive(Debug, Default)]
pub struct Deframer {
    bits: Vec<bool>,
    ones: u8,
    open: bool,
    flags_in_a_row: usize,
    bad_frames: u64,
}

impl Deframer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes one bit, and returns a frame when its closing flag ends it. The
    /// frame has its address, control and information fields, without the
    /// FCS.
    pub fn push(&mut self, bit: bool) -> Option<Vec<u8>> {
        if bit {
            self.ones = self.ones.saturating_add(1);
            if self.ones == 7 {
                self.abort();
            } else if self.open && self.ones < 7 {
                self.bits.push(true);
            }
            return None;
        }
        let ones = std::mem::take(&mut self.ones);
        match ones {
            5 => None,
            6 => self.flag(),
            _ => {
                if self.open {
                    self.bits.push(false);
                    if self.bits.len() > MAX_BITS {
                        self.abort();
                    }
                }
                None
            }
        }
    }

    /// How many flags have come with nothing between them, up to the last
    /// bit.
    #[must_use]
    pub fn flags_in_a_row(&self) -> usize {
        self.flags_in_a_row
    }

    /// Frames dropped for a bad FCS or length.
    #[must_use]
    pub fn bad_frames(&self) -> u64 {
        self.bad_frames
    }

    fn abort(&mut self) {
        self.open = false;
        self.bits.clear();
        self.flags_in_a_row = 0;
    }

    fn flag(&mut self) -> Option<Vec<u8>> {
        let was_open = std::mem::replace(&mut self.open, true);
        // The flag's own 0 and six 1s went in as data.
        let content = self.bits.len().saturating_sub(7);
        self.bits.truncate(content);
        let bits = std::mem::take(&mut self.bits);
        if !was_open || content == 0 {
            self.flags_in_a_row += 1;
            return None;
        }
        self.flags_in_a_row = 0;
        let frame = checked(&bits);
        if frame.is_none() {
            self.bad_frames += 1;
        }
        frame
    }
}

fn checked(bits: &[bool]) -> Option<Vec<u8>> {
    if !bits.len().is_multiple_of(8) || bits.len() / 8 < MIN_CONTENT + FCS_LEN {
        return None;
    }
    let mut bytes: Vec<u8> = bits
        .chunks(8)
        .map(|c| c.iter().rev().fold(0, |b, &bit| b << 1 | u8::from(bit)))
        .collect();
    if fcs_update(FCS_INIT, &bytes) != FCS_GOOD {
        return None;
    }
    bytes.truncate(bytes.len() - FCS_LEN);
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deframe(bits: impl IntoIterator<Item = bool>) -> (Vec<Vec<u8>>, Deframer) {
        let mut deframer = Deframer::new();
        let frames = bits.into_iter().filter_map(|b| deframer.push(b)).collect();
        (frames, deframer)
    }

    fn line(frames: &[&[u8]]) -> VecDeque<bool> {
        let mut bits = VecDeque::new();
        push_flag(&mut bits);
        for frame in frames {
            push_frame(frame, &mut bits);
        }
        bits
    }

    #[test]
    fn the_fcs_is_the_x25_crc() {
        assert_eq!(fcs16(b"123456789"), 0x906e_u16.to_le_bytes());
    }

    #[test]
    fn frames_round_trip_with_their_fcs_checked() {
        let frames: [&[u8]; 3] = [b"\x03\x3f", &[0x7e, 0xff, 0x1f, 0x7d], &[0; 130]];
        let (found, deframer) = deframe(line(&frames));
        assert_eq!(found, frames);
        assert_eq!(deframer.bad_frames(), 0);
    }

    #[test]
    fn no_six_ones_in_a_row_inside_a_frame() {
        let mut bits = VecDeque::new();
        push_frame(&[0xff; 16], &mut bits);
        let body: Vec<bool> = bits.iter().copied().take(bits.len() - 8).collect();
        assert!(!body.windows(6).any(|w| w.iter().all(|&b| b)));
    }

    #[test]
    fn a_corrupt_frame_is_dropped() {
        let mut bits = line(&[b"\x01\x73", b"\x03\x01\x00"]);
        bits[12] = !bits[12];
        let (found, deframer) = deframe(bits);
        assert_eq!(found, [b"\x03\x01\x00".to_vec()]);
        assert_eq!(deframer.bad_frames(), 1);
    }

    #[test]
    fn seven_ones_abort_the_frame() {
        let mut bits = line(&[]);
        let mut frame = VecDeque::new();
        push_frame(b"\x03\x7f", &mut frame);
        bits.extend(frame.iter().take(12));
        bits.extend([true; 7]);
        bits.extend(line(&[b"\x01\x73"]));
        let (found, _) = deframe(bits);
        assert_eq!(found, [b"\x01\x73".to_vec()]);
    }

    #[test]
    fn counts_flags_that_follow_each_other() {
        let mut bits = VecDeque::from(vec![true; 20]);
        for _ in 0..4 {
            push_flag(&mut bits);
        }
        let (_, deframer) = deframe(bits);
        assert_eq!(deframer.flags_in_a_row(), 4);
    }

    #[test]
    fn a_start_stop_tilde_is_not_a_run_of_flags() {
        let bits = (0..8).flat_map(|_| softmodem_dsp::uart::frame(FLAG));
        let (_, deframer) = deframe(bits);
        assert!(deframer.flags_in_a_row() <= 1);
    }
}
