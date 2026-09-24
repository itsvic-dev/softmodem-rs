//! The V.8 menus: CM from the caller, JM from the answering modem, and CJ, as
//! bit sequences for V.21 at 300 bit/s.

// Table 1, the preamble and the CM and JM synchronisation.
const PREAMBLE_ONES: usize = 10;
const SYNC: [bool; 10] = [
    false, false, false, false, false, false, true, true, true, true,
];

// Tables 2 to 7, with bit i of each byte being b_i.
const CALL_FUNCTION_DATA: u8 = 0xC1;
const MODULATION_TAG: u8 = 0x05;
const PCM_PRESENT_BIT: u8 = 0x20;
const V34_BIT: u8 = 0x40;
const CALL_FUNCTION_TAG: u8 = 0x01;
const EXTENSION: u8 = 0x10;
const EXTENSION_MASK: u8 = 0x38;
const V22BIS_BIT: u8 = 0x02;
const V21_BIT: u8 = 0x80;
const CATEGORY_MASK: u8 = 0x1F;
const ACCESS_TAG: u8 = 0x0D;
const DIGITAL_ACCESS_BIT: u8 = 0x80;
const PCM_TAG: u8 = 0x07;
const PCM_ANALOGUE_BIT: u8 = 0x20;
const PCM_DIGITAL_BIT: u8 = 0x40;

/// The sides of V.90 a menu offers, from table 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pcm {
    /// The analogue modem, b5.
    pub analogue: bool,
    /// The digital modem, b6.
    pub digital: bool,
}

impl Pcm {
    pub const NONE: Self = Self {
        analogue: false,
        digital: false,
    };

    #[must_use]
    pub fn any(self) -> bool {
        self.analogue || self.digital
    }

    /// V.90 needs an analogue and a digital modem, one at each end (V.90 § 9.1.1).
    fn common(self, other: Self) -> Self {
        Self {
            analogue: self.analogue && other.digital,
            digital: self.digital && other.analogue,
        }
    }
}

/// The modulations a menu offers, of those this modem has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    pub v90: Pcm,
    /// V.34 duplex, item 2 of table 4.
    pub v34: bool,
    /// V.22bis or V.22, item 4 of table 4.
    pub v22bis: bool,
    /// V.21, item 12 of table 4.
    pub v21: bool,
}

impl Modes {
    #[must_use]
    pub fn common(self, other: Self) -> Self {
        Self {
            v90: self.v90.common(other.v90),
            v34: self.v34 && other.v34,
            v22bis: self.v22bis && other.v22bis,
            v21: self.v21 && other.v21,
        }
    }
}

/// A CM or JM, told apart by the channel it came on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Menu {
    pub data: bool,
    pub modes: Modes,
}

impl Menu {
    #[must_use]
    pub fn data(modes: Modes) -> Self {
        Self { data: true, modes }
    }

    // This modem is always on a digital network connection, as it speaks RTP.
    fn octets(self) -> Vec<u8> {
        let modes = self.modes;
        let call = if self.data { CALL_FUNCTION_DATA } else { 0x01 };
        let modn0 = MODULATION_TAG
            | if modes.v90.any() { PCM_PRESENT_BIT } else { 0 }
            | if modes.v34 { V34_BIT } else { 0 };
        let modn1 = EXTENSION | if modes.v22bis { V22BIS_BIT } else { 0 };
        let modn2 = EXTENSION | if modes.v21 { V21_BIT } else { 0 };
        let mut octets = vec![call, modn0, modn1, modn2];
        if modes.v90.any() {
            let pcm0 = PCM_TAG
                | if modes.v90.analogue { PCM_ANALOGUE_BIT } else { 0 }
                | if modes.v90.digital { PCM_DIGITAL_BIT } else { 0 };
            octets.extend([ACCESS_TAG | DIGITAL_ACCESS_BIT, pcm0]);
        }
        octets
    }

    /// One sequence of the menu, to be sent over and over.
    #[must_use]
    pub fn sequence(self) -> Vec<bool> {
        let mut bits = vec![true; PREAMBLE_ONES];
        bits.extend(SYNC);
        for octet in self.octets() {
            bits.extend(framed(octet));
        }
        bits
    }

    fn parse(octets: &[u8]) -> Option<Self> {
        let (&call, rest) = octets.split_first()?;
        if call & 0x0F != CALL_FUNCTION_TAG || call & EXTENSION != 0 {
            return None;
        }
        let mut modes = Modes::default();
        let mut in_modulation = false;
        let mut extension = 0;
        for &octet in rest {
            if octet & EXTENSION_MASK == EXTENSION {
                if in_modulation {
                    extension += 1;
                    match extension {
                        1 => modes.v22bis = octet & V22BIS_BIT != 0,
                        2 => modes.v21 = octet & V21_BIT != 0,
                        _ => {}
                    }
                }
            } else if octet & EXTENSION == 0 {
                in_modulation = octet & 0x0F == MODULATION_TAG;
                if in_modulation {
                    modes.v34 = octet & V34_BIT != 0;
                }
                if octet & CATEGORY_MASK == PCM_TAG {
                    modes.v90 = Pcm {
                        analogue: octet & PCM_ANALOGUE_BIT != 0,
                        digital: octet & PCM_DIGITAL_BIT != 0,
                    };
                }
                extension = 0;
            }
        }
        Some(Self {
            data: call == CALL_FUNCTION_DATA,
            modes,
        })
    }
}

fn framed(octet: u8) -> impl Iterator<Item = bool> {
    std::iter::once(false)
        .chain((0..8).map(move |i| octet >> i & 1 == 1))
        .chain(std::iter::once(true))
}

/// CJ, which ends CM once the caller has heard JM.
#[must_use]
pub fn cj() -> Vec<bool> {
    (0..3).flat_map(|_| framed(0)).collect()
}

/// What the reader found in the bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    Menu(Menu),
    Cj,
}

#[derive(Debug, Clone, Copy)]
enum State {
    Hunt { ones: usize },
    Sync { at: usize },
    Octets,
    Octet { value: u8, count: u8 },
    Stop { value: u8 },
}

/// Finds CM, JM and CJ in a stream of V.21 bits.
#[derive(Debug)]
pub struct Reader {
    state: State,
    octets: Vec<u8>,
    // The last 30 bits, newest lowest, as CJ may follow a menu or the preamble of the next.
    recent: u32,
}

const CJ_BITS: usize = 30;

fn cj_pattern() -> u32 {
    cj().into_iter().fold(0, |recent, bit| recent << 1 | u32::from(bit))
}

impl Default for Reader {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Hunt { ones: 0 },
            octets: Vec::new(),
            recent: u32::MAX,
        }
    }

    pub fn push(&mut self, bit: bool) -> Option<Heard> {
        self.recent = (self.recent << 1 | u32::from(bit)) & ((1 << CJ_BITS) - 1);
        if self.recent == cj_pattern() {
            self.recent = u32::MAX;
            self.octets.clear();
            self.state = State::Hunt { ones: 0 };
            return Some(Heard::Cj);
        }
        let (next, heard) = match self.state {
            State::Hunt { ones } if bit => (State::Hunt { ones: ones + 1 }, None),
            State::Hunt { ones } if ones >= PREAMBLE_ONES => (self.sync(0, bit), None),
            State::Hunt { .. } => (State::Hunt { ones: 0 }, None),
            State::Sync { at } => (self.sync(at, bit), None),
            State::Octets if bit => {
                let heard = Menu::parse(&self.octets).map(Heard::Menu);
                self.octets.clear();
                (State::Hunt { ones: 1 }, heard)
            }
            State::Octets => (State::Octet { value: 0, count: 0 }, None),
            State::Octet { value, count } => {
                let value = value | u8::from(bit) << count;
                if count == 7 {
                    (State::Stop { value }, None)
                } else {
                    (
                        State::Octet {
                            value,
                            count: count + 1,
                        },
                        None,
                    )
                }
            }
            State::Stop { value } if bit => {
                self.octets.push(value);
                (State::Octets, None)
            }
            State::Stop { .. } => {
                self.octets.clear();
                (State::Hunt { ones: 0 }, None)
            }
        };
        self.state = next;
        heard
    }

    fn sync(&mut self, at: usize, bit: bool) -> State {
        if bit != SYNC[at] {
            return State::Hunt {
                ones: usize::from(bit),
            };
        }
        if at + 1 == SYNC.len() {
            self.octets.clear();
            State::Octets
        } else {
            State::Sync { at: at + 1 }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(bits: impl IntoIterator<Item = bool>) -> Vec<Heard> {
        let mut reader = Reader::new();
        bits.into_iter().filter_map(|b| reader.push(b)).collect()
    }

    const BOTH: Modes = Modes {
        v90: Pcm::NONE,
        v34: false,
        v22bis: true,
        v21: true,
    };

    const ANALOGUE: Modes = Modes {
        v90: Pcm {
            analogue: true,
            digital: false,
        },
        v34: true,
        ..BOTH
    };

    const DIGITAL: Modes = Modes {
        v90: Pcm {
            analogue: false,
            digital: true,
        },
        v34: true,
        ..BOTH
    };

    #[test]
    fn encodes_the_octets_of_tables_3_and_4() {
        assert_eq!(Menu::data(BOTH).octets(), [0xC1, 0x05, 0x12, 0x90]);
        let v34 = Modes { v34: true, ..BOTH };
        assert_eq!(Menu::data(v34).octets(), [0xC1, 0x45, 0x12, 0x90]);
    }

    #[test]
    fn encodes_v90_with_the_pstn_access_and_pcm_octets_of_tables_5_and_7() {
        assert_eq!(
            Menu::data(ANALOGUE).octets(),
            [0xC1, 0x65, 0x12, 0x90, 0x8D, 0x27]
        );
        assert_eq!(
            Menu::data(DIGITAL).octets(),
            [0xC1, 0x65, 0x12, 0x90, 0x8D, 0x47]
        );
    }

    #[test]
    fn reads_back_v34() {
        let menu = Menu::data(Modes { v34: true, ..BOTH });
        let heard = read(menu.sequence().repeat(2).into_iter().chain([true; 10]));
        assert_eq!(heard, [Heard::Menu(menu); 2]);
    }

    #[test]
    fn reads_back_v90() {
        for modes in [ANALOGUE, DIGITAL] {
            let menu = Menu::data(modes);
            let heard = read(menu.sequence().repeat(2).into_iter().chain([true; 10]));
            assert_eq!(heard, [Heard::Menu(menu); 2]);
        }
    }

    #[test]
    fn reads_pcm0_past_a_protocol_octet() {
        let prot0 = 0x8A;
        let octets = [0xC1, 0x65, 0x12, 0x90, prot0, 0x0D, 0x27];
        let menu = Menu::parse(&octets).expect("a data CM");
        assert_eq!(menu.modes, ANALOGUE);
    }

    #[test]
    fn pairs_an_analogue_modem_only_with_a_digital_one() {
        assert!(DIGITAL.common(ANALOGUE).v90.digital);
        assert!(ANALOGUE.common(DIGITAL).v90.analogue);
        assert!(!ANALOGUE.common(ANALOGUE).v90.any());
        assert!(!DIGITAL.common(DIGITAL).v90.any());
        assert!(DIGITAL.common(ANALOGUE).v34);
    }

    #[test]
    fn never_sends_an_hdlc_flag() {
        let bits: Vec<bool> = Menu::data(BOTH).sequence().repeat(3);
        let flag = [false, true, true, true, true, true, true, false];
        assert!(!bits.windows(8).any(|w| w == flag));
    }

    #[test]
    fn reads_back_repeated_menus() {
        let menu = Menu::data(Modes {
            v22bis: false,
            ..BOTH
        });
        let heard = read(menu.sequence().repeat(3).into_iter().chain([true; 10]));
        assert_eq!(heard, [Heard::Menu(menu); 3]);
    }

    #[test]
    fn hears_cj_after_the_menus() {
        let menu = Menu::data(BOTH);
        let mut bits = menu.sequence().repeat(2);
        bits.extend(cj());
        let heard = read(bits);
        assert_eq!(heard.last(), Some(&Heard::Cj));
        assert!(heard.contains(&Heard::Menu(menu)));
    }

    #[test]
    fn hears_cj_that_follows_the_preamble_of_a_new_menu() {
        let mut bits = Menu::data(BOTH).sequence().repeat(2);
        bits.extend([true; 10]);
        bits.extend(cj());
        bits.extend([true; 20]);
        assert_eq!(
            read(bits).last(),
            Some(&Heard::Cj),
            "a caller that ends CM after its preamble would hold JM on until its carrier drops"
        );
    }

    #[test]
    fn ignores_idle_mark_and_noise() {
        assert!(read([true; 200]).is_empty());
        let noise = (0u32..2000).map(|n| n.wrapping_mul(2_654_435_761) >> 31 == 1);
        assert!(!read(noise).iter().any(|h| matches!(h, Heard::Cj)));
    }

    #[test]
    fn keeps_only_the_modes_in_common() {
        let ours = BOTH;
        let theirs = Modes {
            v22bis: false,
            ..BOTH
        };
        assert_eq!(ours.common(theirs), theirs);
    }
}
