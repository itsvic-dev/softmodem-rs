// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! LAPM frames between the flags: the address, control and information
//! fields (V.42 § 8.2).

/// The only DLCI in use, for DTE-to-DTE data (§ 9.2.7).
const DLCI: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supervisory {
    Rr,
    Rnr,
    Rej,
    Srej,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unnumbered {
    Sabme,
    Dm,
    Ui,
    Disc,
    Ua,
    Frmr,
    Xid,
    Test,
}

impl Unnumbered {
    const ALL: [Self; 8] = [
        Self::Sabme,
        Self::Dm,
        Self::Ui,
        Self::Disc,
        Self::Ua,
        Self::Frmr,
        Self::Xid,
        Self::Test,
    ];

    // Table 8, with the P/F bit clear.
    fn code(self) -> u8 {
        match self {
            Self::Sabme => 0x6f,
            Self::Dm => 0x0f,
            Self::Ui => 0x03,
            Self::Disc => 0x43,
            Self::Ua => 0x63,
            Self::Frmr => 0x87,
            Self::Xid => 0xaf,
            Self::Test => 0xe3,
        }
    }

    fn takes_info(self) -> bool {
        matches!(self, Self::Ui | Self::Frmr | Self::Xid | Self::Test)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    I { ns: u8, nr: u8, poll: bool },
    S { kind: Supervisory, nr: u8, pf: bool },
    U { kind: Unnumbered, pf: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The C/R bit, whose meaning depends on which end sent the frame.
    pub cr: bool,
    pub control: Control,
    pub info: Vec<u8>,
}

/// Why a frame was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    /// Too short, or for another DLCI: discarded without a word (§ 8.1.3).
    Invalid,
    /// A control field that Table 8 does not define, or an information field
    /// where none is allowed: a frame-rejection condition (§ 8.5.5).
    Undefined,
}

impl Frame {
    #[must_use]
    pub fn new(cr: bool, control: Control) -> Self {
        Self {
            cr,
            control,
            info: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_info(mut self, info: Vec<u8>) -> Self {
        self.info = info;
        self
    }

    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut bytes = vec![DLCI << 2 | u8::from(self.cr) << 1 | 1];
        match self.control {
            Control::I { ns, nr, poll } => {
                bytes.extend([ns << 1, nr << 1 | u8::from(poll)]);
            }
            Control::S { kind, nr, pf } => {
                bytes.extend([(kind as u8) << 2 | 1, nr << 1 | u8::from(pf)]);
            }
            Control::U { kind, pf } => bytes.push(kind.code() | u8::from(pf) << 4),
        }
        bytes.extend_from_slice(&self.info);
        bytes
    }

    /// Reads a frame from the octets between its flags, less the FCS.
    ///
    /// # Errors
    ///
    /// Fails on a frame that § 8.1.3 calls invalid or § 8.5.5 rejects.
    pub fn parse(bytes: &[u8]) -> Result<Self, Rejected> {
        let [address, first, rest @ ..] = bytes else {
            return Err(Rejected::Invalid);
        };
        if address & 1 == 0 || address >> 2 != DLCI {
            return Err(Rejected::Invalid);
        }
        let cr = address & 2 != 0;
        let (control, info) = if first & 1 == 0 || first & 3 == 1 {
            numbered(*first, rest)?
        } else {
            unnumbered(*first, rest)?
        };
        Ok(Self {
            cr,
            control,
            info: info.to_vec(),
        })
    }
}

// An I or S frame, whose control field takes two octets, and its information field.
fn numbered(first: u8, rest: &[u8]) -> Result<(Control, &[u8]), Rejected> {
    let [second, info @ ..] = rest else {
        return Err(Rejected::Invalid);
    };
    let nr = second >> 1;
    let pf = second & 1 == 1;
    if first & 1 == 0 {
        let control = Control::I {
            ns: first >> 1,
            nr,
            poll: pf,
        };
        return Ok((control, info));
    }
    let kind = match first {
        0x01 => Supervisory::Rr,
        0x05 => Supervisory::Rnr,
        0x09 => Supervisory::Rej,
        0x0d => Supervisory::Srej,
        _ => return Err(Rejected::Undefined),
    };
    if !info.is_empty() && kind != Supervisory::Srej {
        return Err(Rejected::Undefined);
    }
    Ok((Control::S { kind, nr, pf }, info))
}

fn unnumbered(first: u8, rest: &[u8]) -> Result<(Control, &[u8]), Rejected> {
    let kind = Unnumbered::ALL
        .into_iter()
        .find(|k| k.code() == first & !0x10)
        .ok_or(Rejected::Undefined)?;
    if !rest.is_empty() && !kind.takes_info() {
        return Err(Rejected::Undefined);
    }
    let pf = first & 0x10 != 0;
    Ok((Control::U { kind, pf }, rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(kind: Unnumbered, pf: bool) -> Frame {
        Frame::new(true, Control::U { kind, pf })
    }

    #[test]
    fn encodes_the_table_8_control_fields() {
        assert_eq!(u(Unnumbered::Sabme, true).bytes(), [0x03, 0x7f]);
        assert_eq!(u(Unnumbered::Ua, true).bytes(), [0x03, 0x73]);
        assert_eq!(u(Unnumbered::Dm, false).bytes(), [0x03, 0x0f]);
        assert_eq!(u(Unnumbered::Disc, true).bytes(), [0x03, 0x53]);
        assert_eq!(u(Unnumbered::Xid, false).bytes(), [0x03, 0xaf]);
        let rr = Control::S {
            kind: Supervisory::Rr,
            nr: 5,
            pf: true,
        };
        assert_eq!(Frame::new(false, rr).bytes(), [0x01, 0x01, 0x0b]);
        let i = Control::I {
            ns: 127,
            nr: 1,
            poll: false,
        };
        assert_eq!(
            Frame::new(true, i).with_info(b"hi".to_vec()).bytes(),
            [0x03, 0xfe, 0x02, b'h', b'i']
        );
    }

    #[test]
    fn every_frame_round_trips() {
        let mut frames: Vec<Frame> = Unnumbered::ALL
            .into_iter()
            .flat_map(|kind| [u(kind, false), u(kind, true)])
            .collect();
        for kind in [
            Supervisory::Rr,
            Supervisory::Rnr,
            Supervisory::Rej,
            Supervisory::Srej,
        ] {
            frames.push(Frame::new(
                false,
                Control::S {
                    kind,
                    nr: 100,
                    pf: true,
                },
            ));
        }
        frames.push(
            Frame::new(
                true,
                Control::I {
                    ns: 3,
                    nr: 64,
                    poll: true,
                },
            )
            .with_info(vec![0; 128]),
        );
        frames.push(u(Unnumbered::Xid, false).with_info(vec![0x82]));
        for frame in frames {
            assert_eq!(Frame::parse(&frame.bytes()), Ok(frame));
        }
    }

    #[test]
    fn rejects_what_table_8_does_not_define() {
        assert_eq!(Frame::parse(&[0x03, 0x2f]), Err(Rejected::Undefined));
        assert_eq!(Frame::parse(&[0x03, 0x11, 0x00]), Err(Rejected::Undefined));
        assert_eq!(Frame::parse(&[0x03, 0x7f, 0x00]), Err(Rejected::Undefined));
        assert_eq!(
            Frame::parse(&[0x03, 0x01, 0x00, 0x00]),
            Err(Rejected::Undefined)
        );
    }

    #[test]
    fn short_frames_and_other_dlcis_are_invalid() {
        assert_eq!(Frame::parse(&[0x03]), Err(Rejected::Invalid));
        assert_eq!(Frame::parse(&[0x03, 0x00]), Err(Rejected::Invalid));
        assert_eq!(Frame::parse(&[0x07, 0x7f]), Err(Rejected::Invalid));
        assert_eq!(Frame::parse(&[0x02, 0x7f]), Err(Rejected::Invalid));
    }
}
