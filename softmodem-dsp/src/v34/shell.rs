//! The shell mapper of § 9.4, and its inverse for the receiver.

/// Maps K bits to eight ring indices below M, cheaper rings for smaller
/// values, and back.
#[derive(Debug, Clone)]
pub struct ShellMapper {
    rings: usize,
    g2: Vec<u64>,
    g4: Vec<u64>,
    z8: Vec<u64>,
}

fn convolve(g: &[u64]) -> Vec<u64> {
    (0..2 * g.len() - 1)
        .map(|p| {
            (0..=p)
                .filter_map(|i| Some(g.get(i)? * g.get(p - i)?))
                .sum()
        })
        .collect()
}

fn at(g: &[u64], p: usize) -> u64 {
    g.get(p).copied().unwrap_or(0)
}

/// The largest n with `sum(0..n) <= value`, and what remains of `value`.
fn largest(mut value: u64, term: impl Fn(usize) -> u64) -> (usize, u64) {
    let mut n = 0;
    while value >= term(n) {
        value -= term(n);
        n += 1;
    }
    (n, value)
}

fn pair(sum: usize, low: usize, rings: usize) -> [usize; 2] {
    if sum < rings {
        [low, sum - low]
    } else {
        [sum - (rings - 1 - low), rings - 1 - low]
    }
}

fn unpair([first, second]: [usize; 2], rings: usize) -> (usize, usize) {
    let sum = first + second;
    (
        sum,
        if sum < rings {
            first
        } else {
            rings - 1 - second
        },
    )
}

impl ShellMapper {
    /// For M rings, from 1 to 18.
    #[must_use]
    pub fn new(rings: u8) -> Self {
        let rings = usize::from(rings);
        let g2: Vec<u64> = (0..2 * rings - 1)
            .map(|p| (rings - p.abs_diff(rings - 1)) as u64)
            .collect();
        let g4 = convolve(&g2);
        let g8 = convolve(&g4);
        let z8 = std::iter::once(0)
            .chain(g8.iter().scan(0, |total, &g| {
                *total += g;
                Some(*total)
            }))
            .collect();
        Self { rings, g2, g4, z8 }
    }

    /// How many values `map` takes: M⁸.
    #[must_use]
    pub fn values(&self) -> u64 {
        self.z8.last().copied().unwrap_or(0)
    }

    /// Ring indices m(i,0,0), m(i,0,1), ..., m(i,3,1) for the K shell mapping
    /// bits as R0 of equation 9-7, below `values()`.
    #[must_use]
    #[expect(clippy::many_single_char_names, reason = "A to H of § 9.4")]
    pub fn map(&self, r0: u64) -> [u8; 8] {
        let g2 = |p| at(&self.g2, p);
        let g4 = |p| at(&self.g4, p);
        let a = self.z8.iter().rposition(|&z| z <= r0).unwrap_or(0);
        let (b, r1) = largest(r0 - self.z8[a], |p| g4(p) * g4(a - p));
        let (r2, r3) = (r1 % g4(b), r1 / g4(b));
        let (c, r4) = largest(r2, |p| g2(p) * g2(b - p));
        let (d, r5) = largest(r3, |p| g2(p) * g2(a - b - p));
        let (e, f) = (r4 % g2(c), r4 / g2(c));
        let (g, h) = (r5 % g2(d), r5 / g2(d));
        let index = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
        let pairs = [
            pair(c, index(e), self.rings),
            pair(b - c, index(f), self.rings),
            pair(d, index(g), self.rings),
            pair(a - b - d, index(h), self.rings),
        ];
        pairs
            .concat()
            .try_into()
            .map(|rings: [usize; 8]| rings.map(|ring| u8::try_from(ring).unwrap_or(u8::MAX)))
            .unwrap_or_default()
    }

    /// R0 for the eight ring indices that `map` gave.
    #[must_use]
    #[expect(clippy::many_single_char_names, reason = "A to H of § 9.4")]
    pub fn unmap(&self, rings: [u8; 8]) -> u64 {
        let g2 = |p| at(&self.g2, p);
        let g4 = |p| at(&self.g4, p);
        let ring = |n: usize| usize::from(rings[n]);
        let (c, e) = unpair([ring(0), ring(1)], self.rings);
        let (b_minus_c, f) = unpair([ring(2), ring(3)], self.rings);
        let (d, g) = unpair([ring(4), ring(5)], self.rings);
        let (rest, h) = unpair([ring(6), ring(7)], self.rings);
        let b = c + b_minus_c;
        let a = b + d + rest;
        let sum = |n: usize, term: &dyn Fn(usize) -> u64| (0..n).map(term).sum::<u64>();
        let r4 = f as u64 * g2(c) + e as u64;
        let r5 = h as u64 * g2(d) + g as u64;
        let r2 = r4 + sum(c, &|p| g2(p) * g2(b - p));
        let r3 = r5 + sum(d, &|p| g2(p) * g2(a - b - p));
        let r1 = r3 * g4(b) + r2;
        r1 + self.z8[a] + sum(b, &|p| g4(p) * g4(a - p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(n: u64) -> impl Iterator<Item = u64> {
        (0..n).map(|i| i.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 33)
    }

    #[test]
    fn counts_m_to_the_eighth_values() {
        for rings in 1..=18u8 {
            assert_eq!(ShellMapper::new(rings).values(), u64::from(rings).pow(8));
        }
    }

    #[test]
    fn unmaps_every_value_it_maps_for_small_m() {
        for rings in 1..=4 {
            let shell = ShellMapper::new(rings);
            for r0 in 0..shell.values() {
                let mapped = shell.map(r0);
                assert!(mapped.iter().all(|&ring| ring < rings));
                assert_eq!(shell.unmap(mapped), r0);
            }
        }
    }

    #[test]
    fn unmaps_31_bits_for_large_m() {
        for rings in [15, 17, 18] {
            let shell = ShellMapper::new(rings);
            for r0 in values(20_000) {
                let mapped = shell.map(r0);
                assert!(mapped.iter().all(|&ring| ring < rings));
                assert_eq!(shell.unmap(mapped), r0);
            }
        }
    }

    #[test]
    fn gives_smaller_values_cheaper_rings() {
        let shell = ShellMapper::new(5);
        let cost = |r0| {
            shell
                .map(r0)
                .iter()
                .map(|&ring| u32::from(ring))
                .sum::<u32>()
        };
        let costs: Vec<u32> = (0..shell.values()).step_by(97).map(cost).collect();
        assert!(costs.is_sorted());
    }

    #[test]
    fn maps_zero_to_the_inner_ring() {
        assert_eq!(ShellMapper::new(12).map(0), [0; 8]);
        assert_eq!(ShellMapper::new(12).map(1), [0, 0, 0, 0, 0, 0, 0, 1]);
    }
}
