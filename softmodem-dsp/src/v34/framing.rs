//! The data mode framing of § 8 and the mapping parameters of § 9.2, for a
//! symbol rate and a data rate.

use super::SymbolRate;

/// The mapping of one data rate at one symbol rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Framing {
    pub symbol_rate: SymbolRate,
    /// In bit/s, a multiple of 2400.
    pub bit_rate: u32,
    /// Data frames in a superframe.
    pub j: usize,
    /// Mapping frames in a data frame.
    pub p: usize,
    /// Bits in a high mapping frame; a low one carries b − 1.
    pub b: usize,
    /// High mapping frames in a data frame.
    pub r: usize,
    /// Bits for the shell mapper.
    pub k: usize,
    /// Bits for each 2D symbol besides the ring.
    pub q: usize,
    /// Rings, M of table 10.
    pub rings: u8,
    /// Points in the 2D constellation, L of table 10.
    pub points: usize,
}

/// The lowest and highest data rates of table 8 at a symbol rate.
fn rates(symbol_rate: SymbolRate) -> (u32, u32) {
    match symbol_rate {
        SymbolRate::S2400 => (2400, 21_600),
        SymbolRate::S2743 | SymbolRate::S2800 => (4800, 26_400),
        SymbolRate::S3000 => (4800, 28_800),
        SymbolRate::S3200 => (4800, 31_200),
        SymbolRate::S3429 => (4800, 33_600),
    }
}

impl Framing {
    /// With the minimum or the expanded number of rings, if table 8 has the
    /// data rate at the symbol rate.
    #[must_use]
    #[expect(
        clippy::many_single_char_names,
        reason = "J, P, N, b, r, K and q of § 8 and § 9"
    )]
    pub fn new(symbol_rate: SymbolRate, bit_rate: u32, expanded: bool) -> Option<Self> {
        let (low, high) = rates(symbol_rate);
        if !bit_rate.is_multiple_of(2400) || !(low..=high).contains(&bit_rate) {
            return None;
        }
        let (j, p) = match symbol_rate {
            SymbolRate::S2400 => (7, 12),
            SymbolRate::S2743 => (8, 12),
            SymbolRate::S2800 => (7, 14),
            SymbolRate::S3000 => (7, 15),
            SymbolRate::S3200 => (7, 16),
            SymbolRate::S3429 => (8, 15),
        };
        let n = usize::try_from(bit_rate).ok()? * 28 / 100 / j;
        let b = n.div_ceil(p);
        let r = n - (b - 1) * p;
        let (k, q) = if b <= 12 {
            (0, 0)
        } else {
            let q = (b - 12).saturating_sub(31).div_ceil(8);
            (b - 12 - 8 * q, q)
        };
        let rings =
            ring_counts(k).map(|(minimum, larger)| if expanded { larger } else { minimum })?;
        Some(Self {
            symbol_rate,
            bit_rate,
            j,
            p,
            b,
            r,
            k,
            q,
            rings,
            points: (4 * usize::from(rings)) << q,
        })
    }

    /// Whether mapping frame `frame` of a data frame is a high one, by the
    /// counter of § 8.2.
    #[must_use]
    pub fn high(&self, frame: usize) -> bool {
        (frame + 1) * self.r / self.p > frame * self.r / self.p
    }

    /// SWP as table 8 writes it, the first mapping frame leftmost.
    #[must_use]
    pub fn swp(&self) -> u16 {
        (0..self.p).fold(0, |swp, frame| swp << 1 | u16::from(self.high(frame)))
    }

    /// The scale of the precoder's modulo, 2w of § 9.6.2.
    #[must_use]
    pub fn precoder_modulo(&self) -> i64 {
        if self.b < 56 { 2 } else { 4 }
    }
}

// Table 10 rounds 2.5, at K = 8, up.
fn ring_counts(k: usize) -> Option<(u8, u8)> {
    let root = 2f64.powf(f64::from(u8::try_from(k).ok()?) / 8.0);
    let minimum = (1..=18u8).find(|&m| u64::from(m).pow(8) >= 1 << k)?;
    let larger = (1..=18u8)
        .find(|&m| f64::from(m) >= (1.25 * root).round())?
        .max(minimum);
    Some((minimum, larger))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn framing(symbol_rate: SymbolRate, bit_rate: u32) -> Framing {
        Framing::new(symbol_rate, bit_rate, false).unwrap()
    }

    #[test]
    fn gives_b_and_swp_as_table_8() {
        let rows: [(SymbolRate, u32, usize, u16); 12] = [
            (SymbolRate::S2400, 2400, 8, 0xFFF),
            (SymbolRate::S2400, 21_600, 72, 0xFFF),
            (SymbolRate::S2743, 26_400, 77, 0xFFF),
            (SymbolRate::S2800, 4800, 14, 0x1BB7),
            (SymbolRate::S2800, 14_400, 42, 0x0081),
            (SymbolRate::S3000, 9600, 26, 0x2D6B),
            (SymbolRate::S3000, 19_200, 52, 0x0421),
            (SymbolRate::S3000, 28_800, 77, 0x3DEF),
            (SymbolRate::S3200, 31_200, 78, 0xFFFF),
            (SymbolRate::S3429, 4800, 12, 0x0421),
            (SymbolRate::S3429, 12_000, 28, 0x7FFF),
            (SymbolRate::S3429, 33_600, 79, 0x14A5),
        ];
        for (symbol_rate, bit_rate, b, swp) in rows {
            let framing = framing(symbol_rate, bit_rate);
            assert_eq!(
                (framing.b, framing.swp()),
                (b, swp),
                "{symbol_rate:?} at {bit_rate} would not frame as the far end does"
            );
        }
    }

    #[test]
    fn gives_k_m_and_l_as_table_10() {
        let rows: [(SymbolRate, u32, usize, u8, u8, usize, usize); 10] = [
            (SymbolRate::S2400, 2400, 0, 1, 1, 4, 4),
            (SymbolRate::S2400, 7200, 12, 3, 4, 12, 16),
            (SymbolRate::S2400, 14_400, 28, 12, 14, 96, 112),
            (SymbolRate::S2743, 14_400, 30, 14, 17, 56, 68),
            (SymbolRate::S2743, 26_400, 25, 9, 11, 1152, 1408),
            (SymbolRate::S2800, 26_400, 24, 8, 10, 1024, 1280),
            (SymbolRate::S3000, 7200, 8, 2, 3, 8, 12),
            (SymbolRate::S3200, 31_200, 26, 10, 12, 1280, 1536),
            (SymbolRate::S3429, 7200, 5, 2, 2, 8, 8),
            (SymbolRate::S3429, 33_600, 27, 11, 13, 1408, 1664),
        ];
        for (symbol_rate, bit_rate, k, minimum, expanded, l_minimum, l_expanded) in rows {
            let small = framing(symbol_rate, bit_rate);
            let large = Framing::new(symbol_rate, bit_rate, true).unwrap();
            assert_eq!(
                (
                    small.k,
                    small.rings,
                    large.rings,
                    small.points,
                    large.points
                ),
                (k, minimum, expanded, l_minimum, l_expanded),
                "{symbol_rate:?} at {bit_rate} would map to other points than the far end"
            );
        }
    }

    #[test]
    fn has_no_rates_that_table_8_leaves_out() {
        assert!(Framing::new(SymbolRate::S2400, 24_000, false).is_none());
        assert!(Framing::new(SymbolRate::S3000, 2400, false).is_none());
        assert!(Framing::new(SymbolRate::S3200, 33_600, false).is_none());
        assert!(Framing::new(SymbolRate::S3429, 5000, false).is_none());
    }

    #[test]
    fn carries_n_bits_in_each_data_frame() {
        for symbol_rate in SymbolRate::ALL {
            for bit_rate in (2400..=33_600).step_by(2400) {
                let Some(framing) = Framing::new(symbol_rate, bit_rate, false) else {
                    continue;
                };
                let bits: usize = (0..framing.p)
                    .map(|frame| framing.b - usize::from(!framing.high(frame)))
                    .sum();
                assert_eq!(
                    bits * framing.j * 100,
                    usize::try_from(bit_rate).unwrap() * 28
                );
                assert!(framing.high(framing.p - 1));
            }
        }
    }
}
