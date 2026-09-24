//! The patterns of the V.42 detection phase (§ 7.2.1): the originator's ODP
//! and the answerer's ADP, sent and heard as start-stop characters.

use std::collections::VecDeque;

use softmodem_dsp::uart::frame;

const DC1_EVEN: u8 = 0x11;
const DC1_ODD: u8 = 0x91;
const E: u8 = b'E';
const C: u8 = b'C';
const NULL: u8 = 0;
// Between the 8 and 16 that § 7.2.1.2 allows after each character.
const ONES: usize = 10;

/// What an answerer's ADP says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adp {
    Lapm,
    NoErrorCorrection,
}

pub fn push_odp(out: &mut VecDeque<bool>) {
    push_pattern(out, [DC1_EVEN, DC1_ODD]);
}

pub fn push_adp(out: &mut VecDeque<bool>, adp: Adp) {
    let second = match adp {
        Adp::Lapm => C,
        Adp::NoErrorCorrection => NULL,
    };
    push_pattern(out, [E, second]);
}

fn push_pattern(out: &mut VecDeque<bool>, characters: [u8; 2]) {
    for character in characters {
        out.extend(frame(character));
        out.extend([true; ONES]);
    }
}

/// The last characters heard, enough to tell two whole patterns.
#[derive(Debug, Default)]
pub struct Heard {
    last: VecDeque<u8>,
}

impl Heard {
    pub fn push(&mut self, byte: u8) {
        if self.last.len() == 4 {
            self.last.pop_front();
        }
        self.last.push_back(byte);
    }

    /// Four DC1s of alternating parity (§ 7.2.1.3).
    #[must_use]
    pub fn odp(&self) -> bool {
        let dc1 = |b: u8| b == DC1_EVEN || b == DC1_ODD;
        self.last.len() == 4
            && self.last.iter().all(|&b| dc1(b))
            && self
                .last
                .iter()
                .zip(self.last.iter().skip(1))
                .all(|(a, b)| a != b)
    }

    /// Two adjacent ADPs of the same kind (§ 7.2.1.2).
    #[must_use]
    pub fn adp(&self) -> Option<Adp> {
        match self.last.iter().copied().collect::<Vec<_>>()[..] {
            [E, C, E, C] => Some(Adp::Lapm),
            [E, NULL, E, NULL] => Some(Adp::NoErrorCorrection),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use softmodem_dsp::uart::Decoder;

    use super::*;

    fn hear(bits: &VecDeque<bool>) -> Heard {
        let mut decoder = Decoder::v14();
        let mut heard = Heard::default();
        for &bit in bits {
            if let Some(byte) = decoder.push(bit) {
                heard.push(byte);
            }
        }
        heard
    }

    #[test]
    fn the_odp_is_the_bit_pattern_of_section_7_2_1_2() {
        let mut bits = VecDeque::new();
        push_odp(&mut bits);
        let text: String = bits.iter().map(|&b| if b { '1' } else { '0' }).collect();
        let ones = "1".repeat(ONES);
        assert_eq!(text, format!("0100010001{ones}0100010011{ones}"));
    }

    #[test]
    fn the_adp_is_the_bit_pattern_of_table_3() {
        let mut bits = VecDeque::new();
        push_adp(&mut bits, Adp::Lapm);
        let text: String = bits.iter().map(|&b| if b { '1' } else { '0' }).collect();
        let ones = "1".repeat(ONES);
        assert_eq!(text, format!("0101000101{ones}0110000101{ones}"));
    }

    #[test]
    fn two_odps_are_heard_and_one_is_not() {
        let mut bits = VecDeque::new();
        push_odp(&mut bits);
        assert!(!hear(&bits).odp());
        push_odp(&mut bits);
        assert!(hear(&bits).odp());
    }

    #[test]
    fn two_adps_are_heard_by_kind() {
        let mut bits = VecDeque::new();
        push_adp(&mut bits, Adp::Lapm);
        assert_eq!(hear(&bits).adp(), None);
        push_adp(&mut bits, Adp::Lapm);
        assert_eq!(hear(&bits).adp(), Some(Adp::Lapm));
        let mut bits = VecDeque::new();
        push_adp(&mut bits, Adp::NoErrorCorrection);
        push_adp(&mut bits, Adp::NoErrorCorrection);
        assert_eq!(hear(&bits).adp(), Some(Adp::NoErrorCorrection));
    }

    #[test]
    fn dte_text_is_not_a_pattern() {
        let mut heard = Heard::default();
        for &byte in b"ECECho" {
            heard.push(byte);
        }
        assert_eq!(heard.adp(), None);
        for byte in [DC1_EVEN, DC1_EVEN, DC1_ODD, DC1_ODD] {
            heard.push(byte);
        }
        assert!(!heard.odp());
    }
}
