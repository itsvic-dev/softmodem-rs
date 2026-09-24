//! V.34: the sequences of its start-up and the encoding of its data mode.

use crate::pump::Role;

mod bits;
pub mod constellation;
pub mod dpsk;
pub mod encoder;
pub mod framing;
pub mod info;
pub mod modulator;
pub mod mp;
pub mod probing;
pub mod shell;
pub mod tones;
pub mod trellis;

/// The nominal transmit power, the mean that V.2 allows.
pub const NOMINAL_DBM0: f64 = -13.0;
/// The guard tone the answer modem sends with INFO and tone A.
pub const GUARD_HZ: f64 = 1800.0;
/// 7 dB below nominal, as § 10.1.2.3.1 has it for INFO.
pub const GUARD_DBM0: f64 = NOMINAL_DBM0 - 7.0;

/// The phase 2 carrier of INFO and of tone A or B, from the end in `role`.
#[must_use]
pub fn carrier_hz(role: Role) -> f64 {
    match role {
        Role::Answer => 2400.0,
        Role::Originate => 1200.0,
    }
}

/// The level of INFO and of tone A or B, from the end in `role`: 1 dB below
/// nominal from the answer modem, to leave room for its guard tone.
#[must_use]
pub fn info_level_dbm0(role: Role) -> f64 {
    match role {
        Role::Answer => NOMINAL_DBM0 - 1.0,
        Role::Originate => NOMINAL_DBM0,
    }
}

/// Table 1, in the order INFO sequences number them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SymbolRate {
    S2400,
    S2743,
    S2800,
    S3000,
    S3200,
    S3429,
}

impl SymbolRate {
    pub const ALL: [Self; 6] = [
        Self::S2400,
        Self::S2743,
        Self::S2800,
        Self::S3000,
        Self::S3200,
        Self::S3429,
    ];

    /// a and c of S = (a/c) · 2400.
    fn ratio(self) -> (u32, u32) {
        match self {
            Self::S2400 => (1, 1),
            Self::S2743 => (8, 7),
            Self::S2800 => (7, 6),
            Self::S3000 => (5, 4),
            Self::S3200 => (4, 3),
            Self::S3429 => (10, 7),
        }
    }

    #[must_use]
    pub fn baud(self) -> f64 {
        let (a, c) = self.ratio();
        2400.0 * f64::from(a) / f64::from(c)
    }

    /// Table 2: d and e of the carrier (d/e) · S.
    fn carrier_ratio(self, high: bool) -> (u32, u32) {
        match (self, high) {
            (Self::S2400, false) | (Self::S2743 | Self::S2800 | Self::S3000, true) => (2, 3),
            (Self::S2400, true) => (3, 4),
            (Self::S2743 | Self::S2800 | Self::S3000, false) | (Self::S3200, true) => (3, 5),
            (Self::S3200, false) | (Self::S3429, _) => (4, 7),
        }
    }

    #[must_use]
    pub fn carrier_hz(self, high: bool) -> f64 {
        let (d, e) = self.carrier_ratio(high);
        self.baud() * f64::from(d) / f64::from(e)
    }

    #[must_use]
    pub fn index(self) -> u8 {
        match self {
            Self::S2400 => 0,
            Self::S2743 => 1,
            Self::S2800 => 2,
            Self::S3000 => 3,
            Self::S3200 => 4,
            Self::S3429 => 5,
        }
    }

    #[must_use]
    pub fn from_index(index: u32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|index| Self::ALL.get(index))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rounded(hz: impl Fn(SymbolRate) -> f64) -> Vec<String> {
        SymbolRate::ALL
            .into_iter()
            .map(|rate| format!("{:.0}", hz(rate)))
            .collect()
    }

    #[test]
    fn rounds_to_table_1() {
        assert_eq!(
            rounded(SymbolRate::baud),
            ["2400", "2743", "2800", "3000", "3200", "3429"]
        );
    }

    #[test]
    fn rounds_to_table_2() {
        assert_eq!(
            rounded(|rate| rate.carrier_hz(false)),
            ["1600", "1646", "1680", "1800", "1829", "1959"]
        );
        assert_eq!(
            rounded(|rate| rate.carrier_hz(true)),
            ["1800", "1829", "1867", "2000", "1920", "1959"]
        );
    }

    #[test]
    fn numbers_them_in_order() {
        for rate in SymbolRate::ALL {
            assert_eq!(SymbolRate::from_index(rate.index().into()), Some(rate));
        }
        assert_eq!(SymbolRate::from_index(6), None);
    }
}
