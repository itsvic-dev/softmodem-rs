//! The information field of an XID frame, which negotiates LAPM's parameters
//! and optional procedures (V.42 § 12.2).

const FORMAT: u8 = 0x82;
const PARAMETERS: u8 = 0x80;
const PRIVATE: u8 = 0xf0;
const USER_DATA: u8 = 0xff;

const PARAMETER_SET: u8 = 0;
const P0: u8 = 1;
const P1: u8 = 2;
const P2: u8 = 3;
// V.42 bis Annex A, Table A-1.
const V42: [u8; 3] = *b"V42";

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
    /// V.42 bis, when P0 is present to ask for it.
    pub compression: Option<Compression>,
}

/// V.42 bis's parameters, in the private parameter group (V.42 Table 11b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compression {
    /// P0: bit 0 for the direction from the negotiation initiator, bit 1 for
    /// the direction to it.
    pub directions: u8,
    /// P1, the number of codewords. Left out, it is 512.
    pub codewords: Option<u16>,
    /// P2, the longest string. Left out, it is 6.
    pub max_string: Option<u8>,
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
        let mut bytes = vec![FORMAT];
        push_group(&mut bytes, PARAMETERS, &group);
        if let Some(compression) = self.compression {
            let mut group = Vec::new();
            push_parameter(&mut group, PARAMETER_SET, &V42);
            push_parameter(&mut group, P0, &[compression.directions]);
            if let Some(codewords) = compression.codewords {
                push_parameter(&mut group, P1, &codewords.to_be_bytes());
            }
            if let Some(max_string) = compression.max_string {
                push_parameter(&mut group, P2, &[max_string]);
            }
            push_group(&mut bytes, PRIVATE, &group);
        }
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
            match *id {
                PARAMETERS => xid.read_parameters(group)?,
                PRIVATE => xid.read_private(group)?,
                _ => {}
            }
        }
        Some(xid)
    }

    fn read_parameters(&mut self, group: &[u8]) -> Option<()> {
        for (id, value) in parameters(group)? {
            let number = number(value);
            match id {
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

    fn read_private(&mut self, group: &[u8]) -> Option<()> {
        let mut directions = None;
        let mut codewords = None;
        let mut max_string = None;
        for (id, value) in parameters(group)? {
            let number = number(value);
            match id {
                P0 => directions = number.map(|n| n.to_le_bytes()[0] & 3),
                P1 => codewords = number.map(|n| u16::try_from(n).unwrap_or(u16::MAX)),
                P2 => max_string = number.map(window),
                _ => {}
            }
        }
        self.compression = directions.map(|directions| Compression {
            directions,
            codewords,
            max_string,
        });
        Some(())
    }
}

/// The identifier and value of each parameter in a group.
fn parameters(mut group: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut found = Vec::new();
    while let [id, length, tail @ ..] = group {
        let value = tail.get(..usize::from(*length))?;
        group = &tail[usize::from(*length)..];
        found.push((*id, value));
    }
    Some(found)
}

fn number(value: &[u8]) -> Option<u32> {
    value
        .iter()
        .try_fold(0u32, |n, &b| n.checked_mul(256).map(|n| n | u32::from(b)))
}

fn push_group(bytes: &mut Vec<u8>, id: u8, group: &[u8]) {
    bytes.push(id);
    bytes.extend(u16::try_from(group.len()).unwrap_or(u16::MAX).to_be_bytes());
    bytes.extend_from_slice(group);
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
    fn carries_v42bis_in_the_private_group_as_annex_a_shows() {
        let xid = Xid {
            compression: Some(Compression {
                directions: 3,
                codewords: Some(2048),
                max_string: Some(32),
            }),
            ..Xid::default()
        };
        let bytes = xid.bytes();
        assert_eq!(
            bytes[10..],
            [
                0xf0, 0x00, 0x0f, 0x00, 0x03, b'V', b'4', b'2', 0x01, 0x01, 0x03, 0x02, 0x02, 0x08,
                0x00, 0x03, 0x01, 0x20
            ]
        );
        assert_eq!(Xid::parse(&bytes), Some(xid));
    }

    #[test]
    fn a_truncated_field_does_not_parse() {
        assert_eq!(Xid::parse(&[0x82, 0x80, 0x00, 0x05, 0x07, 0x01]), None);
        assert_eq!(Xid::parse(&[0x83, 0x80, 0x00, 0x00]), None);
    }
}
