//! The information field of an XID frame, which negotiates LAPM's parameters
//! and optional procedures (V.42 § 12.2).

const FORMAT: u8 = 0x82;
const PARAMETERS: u8 = 0x80;
const USER_DATA: u8 = 0xff;

const OPTIONAL_FUNCTIONS: u8 = 3;
const N401_TX: u8 = 5;
const N401_RX: u8 = 6;
const K_TX: u8 = 7;
const K_RX: u8 = 8;

// Bits 2, 4, 8, 9, 12 and 16, which every XID sets (Table 11a, note 1).
const REQUIRED_FUNCTIONS: u32 = 1 << 1 | 1 << 3 | 1 << 7 | 1 << 8 | 1 << 11 | 1 << 15;

/// Parameters as the sender of the XID frame states them. `tx` is the
/// direction from the sender, `rx` the direction to it (Table 11a, note 2).
/// A value left out is the default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Xid {
    /// The HDLC optional functions beyond those every XID sets.
    pub functions: u32,
    /// N401 in octets.
    pub n401_tx: Option<u16>,
    pub n401_rx: Option<u16>,
    pub k_tx: Option<u8>,
    pub k_rx: Option<u8>,
}

impl Xid {
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut group = Vec::new();
        let functions = self.functions | REQUIRED_FUNCTIONS;
        push_parameter(&mut group, OPTIONAL_FUNCTIONS, &functions.to_le_bytes());
        for (id, octets) in [(N401_TX, self.n401_tx), (N401_RX, self.n401_rx)] {
            if let Some(octets) = octets {
                let bits = u32::from(octets) * 8;
                push_parameter(&mut group, id, minimal(&bits.to_be_bytes()));
            }
        }
        for (id, k) in [(K_TX, self.k_tx), (K_RX, self.k_rx)] {
            if let Some(k) = k {
                push_parameter(&mut group, id, &[k]);
            }
        }
        #[expect(clippy::cast_possible_truncation, reason = "at most 20 octets")]
        let length = group.len() as u16;
        let mut bytes = vec![FORMAT, PARAMETERS];
        bytes.extend(length.to_be_bytes());
        bytes.extend(group);
        bytes
    }

    /// Reads the parameters it knows, and skips those it does not.
    #[must_use]
    pub fn parse(info: &[u8]) -> Option<Self> {
        let Some((&FORMAT, mut rest)) = info.split_first() else {
            return None;
        };
        let mut xid = Self::default();
        while let [id, tail @ ..] = rest {
            if *id == USER_DATA {
                break;
            }
            let [high, low, tail @ ..] = tail else {
                return None;
            };
            let length = usize::from(u16::from_be_bytes([*high, *low]));
            let group = tail.get(..length)?;
            rest = &tail[length..];
            if *id == PARAMETERS {
                xid.read_parameters(group)?;
            }
        }
        Some(xid)
    }

    fn read_parameters(&mut self, mut group: &[u8]) -> Option<()> {
        while let [id, length, tail @ ..] = group {
            let value = tail.get(..usize::from(*length))?;
            group = &tail[usize::from(*length)..];
            let number = value
                .iter()
                .try_fold(0u32, |n, &b| n.checked_mul(256).map(|n| n | u32::from(b)));
            match *id {
                OPTIONAL_FUNCTIONS => {
                    let mut bytes = [0; 4];
                    let n = value.len().min(4);
                    bytes[..n].copy_from_slice(&value[..n]);
                    self.functions = u32::from_le_bytes(bytes) & !REQUIRED_FUNCTIONS;
                }
                N401_TX => self.n401_tx = number.map(octets),
                N401_RX => self.n401_rx = number.map(octets),
                K_TX => self.k_tx = number.map(window),
                K_RX => self.k_rx = number.map(window),
                _ => {}
            }
        }
        Some(())
    }
}

fn push_parameter(group: &mut Vec<u8>, id: u8, value: &[u8]) {
    group.push(id);
    group.push(u8::try_from(value.len()).expect("a short value"));
    group.extend_from_slice(value);
}

fn minimal(bytes: &[u8]) -> &[u8] {
    let zeros = bytes.iter().take_while(|&&b| b == 0).count();
    &bytes[zeros.min(bytes.len() - 1)..]
}

fn octets(bits: u32) -> u16 {
    u16::try_from(bits / 8).unwrap_or(u16::MAX)
}

fn window(k: u32) -> u8 {
    u8::try_from(k).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_defaults_as_iso_8885_asks() {
        let xid = Xid {
            n401_tx: Some(128),
            n401_rx: Some(128),
            k_tx: Some(15),
            k_rx: Some(15),
            ..Xid::default()
        };
        assert_eq!(
            xid.bytes(),
            [
                0x82, 0x80, 0x00, 0x14, 0x03, 0x04, 0x8a, 0x89, 0x00, 0x00, 0x05, 0x02, 0x04, 0x00,
                0x06, 0x02, 0x04, 0x00, 0x07, 0x01, 0x0f, 0x08, 0x01, 0x0f
            ]
        );
        assert_eq!(Xid::parse(&xid.bytes()), Some(xid));
    }

    #[test]
    fn a_value_left_out_stays_the_default() {
        let xid = Xid {
            k_rx: Some(7),
            ..Xid::default()
        };
        assert_eq!(Xid::parse(&xid.bytes()), Some(xid));
    }

    #[test]
    fn skips_groups_and_parameters_it_does_not_know() {
        let info = [
            0x82, 0x80, 0x00, 0x07, 0x07, 0x01, 0x04, 0x63, 0x02, 0xaa, 0xbb, 0xf0, 0x00, 0x05,
            0x00, 0x03, b'V', b'.', b'4', 0xff, 0x40, 0x03, b'V', b'.', b'4',
        ];
        assert_eq!(
            Xid::parse(&info),
            Some(Xid {
                k_tx: Some(4),
                ..Xid::default()
            })
        );
    }

    #[test]
    fn a_truncated_field_does_not_parse() {
        assert_eq!(Xid::parse(&[0x82, 0x80, 0x00, 0x05, 0x07, 0x01]), None);
        assert_eq!(Xid::parse(&[0x83, 0x80, 0x00, 0x00]), None);
    }
}
