//! The encoder of the digital modem (§ 5.4), which turns each data frame
//! of D bits into six PCM codewords, and its inverse for the analogue
//! modem.

use std::collections::VecDeque;

use super::Codeword;
use super::cp::{self, Cp};
use super::ucode::Law;

/// Symbols in a data frame.
pub const FRAME: usize = 6;
const Q16: f64 = 64.0;

/// The mapping parameters of § 5.4.1.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapping {
    /// The PCM code set of each data frame interval, labelled from the
    /// largest Ucode down.
    pub sets: [Vec<u8>; FRAME],
    /// K, the bits that enter the modulus encoder.
    pub modulus_bits: usize,
    /// Sr, the sign bits of each frame that shape the spectrum.
    pub redundancy: u8,
    /// ld.
    pub lookahead: u8,
    /// a1, a2, b1 and b2 of § 5.4.5.6.
    pub filter: [f64; 4],
    pub law: Law,
}

impl Mapping {
    /// From CP or CPt, if its sets carry its rate.
    #[must_use]
    pub fn from_cp(cp: &Cp) -> Option<Self> {
        let bits = cp.frame_bits();
        let redundancy = cp.redundancy.min(3);
        let sets: [Vec<u8>; FRAME] =
            std::array::from_fn(|interval| cp::labels(cp.constellation(interval)));
        let modulus_bits = bits.checked_sub(FRAME - usize::from(redundancy))?;
        let mapping = Self {
            sets,
            modulus_bits,
            redundancy,
            lookahead: cp.lookahead,
            filter: cp.filter.map(|c| f64::from(c) / Q16),
            law: cp.law,
        };
        mapping.fits().then_some(mapping)
    }

    /// § 8.6: the mapping of MP and Ed in a rate renegotiation, the
    /// constellations and K of CPt with the spectral shaping of data mode.
    #[must_use]
    pub fn renegotiating(training: &Self, data: &Self) -> Self {
        Self {
            redundancy: data.redundancy,
            lookahead: data.lookahead,
            filter: data.filter,
            ..training.clone()
        }
    }

    /// Whether 2^K ≤ M0 · … · M5, as § 5.4.3 requires.
    #[must_use]
    pub fn fits(&self) -> bool {
        let product = self
            .sets
            .iter()
            .try_fold(1u128, |product, set| product.checked_mul(set.len() as u128));
        self.modulus_bits < 64 && product.is_some_and(|p| p >= 1 << self.modulus_bits)
    }

    /// S, the sign bits of each frame that carry data.
    #[must_use]
    pub fn sign_bits(&self) -> usize {
        FRAME - usize::from(self.redundancy)
    }

    /// D, the bits of each data frame.
    #[must_use]
    pub fn bits(&self) -> usize {
        self.sign_bits() + self.modulus_bits
    }

    // The symbols of one spectral shaping frame: 6, 3 or 2.
    fn shaping_length(&self) -> usize {
        match self.redundancy {
            0 | 1 => FRAME,
            2 => 3,
            _ => 2,
        }
    }

    // § 5.4.3: K bits to K0 to K5, and § 5.4.4: each to its Ucode.
    fn map(&self, bits: &[bool]) -> [u8; FRAME] {
        let mut value = bits
            .iter()
            .rev()
            .fold(0u64, |value, &bit| value << 1 | u64::from(bit));
        std::array::from_fn(|interval| {
            let set = &self.sets[interval];
            let modulus = set.len().max(1) as u64;
            let label = usize::try_from(value % modulus).unwrap_or(0);
            value /= modulus;
            set.get(label).copied().unwrap_or(0)
        })
    }

    // The inverse of `map`, from the Ucodes of one frame.
    fn unmap(&self, ucodes: [u8; FRAME]) -> Vec<bool> {
        let value = (0..FRAME).rev().fold(0u64, |value, interval| {
            let set = &self.sets[interval];
            let label = set.iter().position(|&u| u == ucodes[interval]).unwrap_or(0);
            value * set.len().max(1) as u64 + label as u64
        });
        (0..self.modulus_bits)
            .map(|n| value >> n & 1 == 1)
            .collect()
    }
}

// Rules A to D of § 5.4.5.5 as masks over a shaping frame, and the trellis of figure 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    A,
    B,
    C,
    D,
}

impl Rule {
    fn allowed(state: bool) -> [Self; 2] {
        if state {
            [Self::C, Self::D]
        } else {
            [Self::A, Self::B]
        }
    }

    fn next_state(self) -> bool {
        matches!(self, Self::B | Self::D)
    }

    fn inverts(self, position: usize) -> bool {
        match self {
            Self::A => false,
            Self::B => true,
            Self::C => position.is_multiple_of(2),
            Self::D => !position.is_multiple_of(2),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Filter {
    x: f64,
    y: f64,
    v: f64,
}

impl Filter {
    // § 5.4.5.6: one sample through F(z), and what it adds to w[n].
    fn push(&mut self, x: f64, [a1, a2, b1, b2]: [f64; 4]) -> f64 {
        let y = x - b1 * self.x + a1 * self.y;
        let v = y - b2 * self.y + a2 * self.v;
        *self = Self { x, y, v };
        v * v
    }
}

#[derive(Debug, Clone)]
struct Pending {
    ucodes: Vec<u8>,
    /// t of § 5.4.5.2, the signs before shaping.
    signs: Vec<bool>,
}

/// Maps data frames to codewords, with the spectral shaping that the
/// mapping asks for.
#[derive(Debug)]
pub struct Encoder {
    mapping: Mapping,
    pending: VecDeque<Pending>,
    out: VecDeque<Codeword>,
    last_sign: bool,
    last_odd: bool,
    last_t: Vec<bool>,
    state: bool,
    filter: Filter,
}

impl Encoder {
    /// With its differential encoders and filter at zero, as TRN2d and B1d
    /// start them.
    #[must_use]
    pub fn new(mapping: Mapping) -> Self {
        let length = mapping.shaping_length();
        Self {
            mapping,
            pending: VecDeque::new(),
            out: VecDeque::new(),
            last_sign: false,
            last_odd: false,
            last_t: vec![false; length],
            state: false,
            filter: Filter::default(),
        }
    }

    #[must_use]
    pub fn mapping(&self) -> &Mapping {
        &self.mapping
    }

    /// The next data frame of codewords, taking D bits from `bits` for each
    /// frame the shaper needs, which may be frames ahead.
    pub fn frame(&mut self, mut bits: impl FnMut() -> bool) -> [Codeword; FRAME] {
        while self.out.len() < FRAME {
            if self.mapping.redundancy == 0 {
                let frame: Vec<bool> = (0..self.mapping.bits()).map(|_| bits()).collect();
                self.unshaped(&frame);
                continue;
            }
            let lookahead = usize::from(self.mapping.lookahead);
            while self.pending.len() <= lookahead {
                let frame: Vec<bool> = (0..self.mapping.bits()).map(|_| bits()).collect();
                self.parse(&frame);
            }
            self.shape();
        }
        std::array::from_fn(|_| self.out.pop_front().unwrap_or(Codeword::SILENCE))
    }

    // § 5.4.5.1: Sr = 0, S = 6.
    fn unshaped(&mut self, frame: &[bool]) {
        let (signs, modulus) = frame.split_at(FRAME);
        let ucodes = self.mapping.map(modulus);
        for (&ucode, &bit) in ucodes.iter().zip(signs) {
            self.last_sign ^= bit;
            self.out.push_back(Codeword {
                ucode,
                positive: self.last_sign,
            });
        }
    }

    // Tables 3 and 4 and the second differential encoding, for each shaping frame of a data frame.
    fn parse(&mut self, frame: &[bool]) {
        let (signs, modulus) = frame.split_at(self.mapping.sign_bits());
        let ucodes = self.mapping.map(modulus);
        let length = self.mapping.shaping_length();
        let mut signs = signs.iter().copied();
        for part in ucodes.chunks(length) {
            let mut p = vec![false; length];
            for bit in p.iter_mut().skip(1) {
                *bit = signs.next().unwrap_or(false);
            }
            let mut previous_odd = self.last_odd;
            for k in (1..length).step_by(2) {
                p[k] ^= previous_odd;
                previous_odd = p[k];
            }
            self.last_odd = previous_odd;
            let t: Vec<bool> = p.iter().zip(&self.last_t).map(|(&p, &t)| p ^ t).collect();
            self.last_t.clone_from(&t);
            self.pending.push_back(Pending {
                ucodes: part.to_vec(),
                signs: t,
            });
        }
    }

    // What `frame` adds to w[n] under `rule`, from the state of `filter`.
    fn weigh(&self, frame: &Pending, rule: Rule, filter: &mut Filter) -> f64 {
        frame
            .signed(rule)
            .map(|codeword| filter.push(f64::from(codeword.linear(self.mapping.law)), self.mapping.filter))
            .sum()
    }

    // The least that `frames` can add to w[n] from `state`, and the rule for the first.
    fn best(&self, frames: &[&Pending], state: bool, filter: Filter) -> (f64, Rule) {
        let Some((first, rest)) = frames.split_first() else {
            return (0.0, Rule::A);
        };
        Rule::allowed(state)
            .into_iter()
            .map(|rule| {
                let mut filter = filter;
                let here = self.weigh(first, rule, &mut filter);
                (here + self.best(rest, rule.next_state(), filter).0, rule)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .unwrap_or((0.0, Rule::A))
    }

    // § 5.4.5.5: the rule for the oldest frame that minimises w[n] to the end of the look-ahead.
    fn shape(&mut self) {
        let frames: Vec<&Pending> = self.pending.iter().collect();
        let (_, rule) = self.best(&frames, self.state, self.filter);
        let frame = self.pending.pop_front().expect("the look-ahead is queued");
        let mut filter = self.filter;
        self.weigh(&frame, rule, &mut filter);
        self.filter = filter;
        self.out.extend(frame.signed(rule));
        self.state = rule.next_state();
    }
}

impl Pending {
    fn signed(&self, rule: Rule) -> impl Iterator<Item = Codeword> {
        self.ucodes
            .iter()
            .zip(&self.signs)
            .enumerate()
            .map(move |(k, (&ucode, &t))| Codeword {
                ucode,
                positive: t ^ rule.inverts(k),
            })
    }
}

/// Turns the codewords of each data frame back into its D bits.
#[derive(Debug)]
pub struct Decoder {
    mapping: Mapping,
    last_sign: bool,
    last_signs: Vec<bool>,
    last_odd: bool,
}

impl Decoder {
    #[must_use]
    pub fn new(mapping: Mapping) -> Self {
        let length = mapping.shaping_length();
        Self {
            mapping,
            last_sign: false,
            last_signs: vec![false; length],
            last_odd: false,
        }
    }

    #[must_use]
    pub fn mapping(&self) -> &Mapping {
        &self.mapping
    }

    /// The D bits of one data frame.
    pub fn frame(&mut self, codewords: [Codeword; FRAME]) -> Vec<bool> {
        let ucodes = codewords.map(|c| c.ucode);
        let mut bits = Vec::with_capacity(self.mapping.bits());
        if self.mapping.redundancy == 0 {
            for codeword in codewords {
                bits.push(codeword.positive ^ self.last_sign);
                self.last_sign = codeword.positive;
            }
        } else {
            let length = self.mapping.shaping_length();
            for part in codewords.chunks(length) {
                let u: Vec<bool> = part
                    .iter()
                    .zip(&self.last_signs)
                    .map(|(c, &last)| c.positive ^ last)
                    .collect();
                for k in 1..length {
                    bits.push(if k.is_multiple_of(2) {
                        u[k] ^ u[0]
                    } else if k == 1 {
                        u[1] ^ self.last_odd ^ u[0]
                    } else {
                        u[k] ^ u[k - 2]
                    });
                }
                self.last_odd = u[length - 1 - usize::from(length % 2 == 1)];
                self.last_signs = part.iter().map(|c| c.positive).collect();
            }
        }
        bits.extend(self.mapping.unmap(ucodes));
        bits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v90::ucode;

    fn mapping(redundancy: u8, lookahead: u8) -> Mapping {
        let sets = std::array::from_fn(|interval| {
            (0..ucode::COUNT)
                .rev()
                .filter(|&u| (usize::from(u) + interval).is_multiple_of(3))
                .take(20 + interval * 3)
                .collect()
        });
        Mapping {
            sets,
            modulus_bits: 26,
            redundancy,
            lookahead,
            filter: [0.9, -0.3, 0.2, 0.5],
            law: Law::A,
        }
    }

    fn data(count: usize) -> Vec<bool> {
        (0u64..count as u64)
            .map(|n| n.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 63 == 1)
            .collect()
    }

    #[test]
    fn maps_and_unmaps_the_modulus_bits() {
        let mapping = mapping(0, 0);
        assert!(mapping.fits());
        for chunk in data(26 * 40).chunks(26) {
            assert_eq!(mapping.unmap(mapping.map(chunk)), chunk);
        }
    }

    #[test]
    fn labels_0_as_the_largest_code() {
        let mapping = mapping(0, 0);
        let zeros = [false; 26];
        assert_eq!(mapping.map(&zeros)[0], mapping.sets[0][0]);
        assert!(mapping.sets[0][0] > mapping.sets[0][1]);
    }

    #[test]
    fn decodes_what_it_encodes_for_every_redundancy_and_lookahead() {
        for redundancy in 0..=3 {
            for lookahead in 0..=3 {
                let mapping = mapping(redundancy, lookahead);
                let bits = data(mapping.bits() * 60);
                let mut source = bits.iter().copied().chain(std::iter::repeat(false));
                let mut encoder = Encoder::new(mapping.clone());
                let mut decoder = Decoder::new(mapping.clone());
                let received: Vec<bool> = (0..60)
                    .flat_map(|_| decoder.frame(encoder.frame(|| source.next().unwrap_or(false))))
                    .collect();
                assert_eq!(received, bits, "Sr {redundancy}, ld {lookahead}");
            }
        }
    }

    #[test]
    fn shaping_keeps_down_what_the_metric_weighs() {
        let filter = [63.0 / 64.0, 0.0, 0.0, 0.0];
        let metric = |redundancy: u8, lookahead: u8| {
            let mut mapping = mapping(redundancy, lookahead);
            mapping.filter = filter;
            let bits = data(mapping.bits() * 400);
            let mut source = bits.into_iter();
            let mut encoder = Encoder::new(mapping);
            let mut weight = Filter::default();
            let mut w = 0.0;
            for _ in 0..400 {
                for codeword in encoder.frame(|| source.next().unwrap_or(false)) {
                    w += weight.push(f64::from(codeword.linear(Law::A)), filter);
                }
            }
            w
        };
        let unshaped = metric(0, 0);
        let shaped: Vec<f64> = [(1, 0), (1, 1), (2, 2), (3, 3)]
            .into_iter()
            .map(|(redundancy, lookahead)| metric(redundancy, lookahead) / unshaped)
            .collect();
        assert!(
            shaped[0] < 0.6 && shaped.windows(2).all(|pair| pair[1] < pair[0]),
            "more sign bits and look-ahead would not bring the spectrum closer to the far end's filter: {shaped:?}"
        );
    }
}
