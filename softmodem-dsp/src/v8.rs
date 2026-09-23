//! The V.8 menus: CM from the caller, JM from the answering modem, and CJ, as
//! bit sequences for V.21 at 300 bit/s.

// Table 1, the preamble and the CM and JM synchronisation.
const PREAMBLE_ONES: usize = 10;
const SYNC: [bool; 10] = [
    false, false, false, false, false, false, true, true, true, true,
];

// Tables 2 to 4, with bit i of each byte being b_i.
const CALL_FUNCTION_DATA: u8 = 0xC1;
const MODULATION_TAG: u8 = 0x05;
const CALL_FUNCTION_TAG: u8 = 0x01;
const EXTENSION: u8 = 0x10;
const EXTENSION_MASK: u8 = 0x38;
const V22BIS_BIT: u8 = 0x02;
const V21_BIT: u8 = 0x80;

/// The modulations a menu offers, of those this modem has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modes {
    /// V.22bis or V.22, item 4 of table 4.
    pub v22bis: bool,
    /// V.21, item 12 of table 4.
    pub v21: bool,
}

impl Modes {
    #[must_use]
    pub fn common(self, other: Self) -> Self {
        Self {
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

    fn octets(self) -> [u8; 4] {
        let call = if self.data { CALL_FUNCTION_DATA } else { 0x01 };
        let modn1 = EXTENSION | if self.modes.v22bis { V22BIS_BIT } else { 0 };
        let modn2 = EXTENSION | if self.modes.v21 { V21_BIT } else { 0 };
        [call, MODULATION_TAG, modn1, modn2]
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
    zeros: usize,
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
            zeros: 0,
        }
    }

    pub fn push(&mut self, bit: bool) -> Option<Heard> {
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
                self.zeros = if value == 0 { self.zeros + 1 } else { 0 };
                if self.zeros == 3 {
                    self.zeros = 0;
                    self.octets.clear();
                    (State::Hunt { ones: 0 }, Some(Heard::Cj))
                } else {
                    self.octets.push(value);
                    (State::Octets, None)
                }
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
            self.zeros = 0;
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
        v22bis: true,
        v21: true,
    };

    #[test]
    fn encodes_the_octets_of_tables_3_and_4() {
        assert_eq!(Menu::data(BOTH).octets(), [0xC1, 0x05, 0x12, 0x90]);
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
            v21: true,
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
            v21: true,
        };
        assert_eq!(ours.common(theirs), theirs);
    }
}
