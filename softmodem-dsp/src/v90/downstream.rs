// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The analogue modem's receiver of the digital modem's PCM signal, from
//! phase 3 on (§ 9.3.2, § 9.4.2). The path to it is digital, so each sample
//! is one symbol.

use std::collections::VecDeque;

use super::Codeword;
use super::design::{Levels, UCHORDS};
use super::dil::{Descriptor, Dil};
use super::encoder::{Decoder, FRAME, Mapping};
use super::frames::Deframer;
use super::jd::Jd;
use super::training::{JD_PRIME_BITS, SignReader, TRN1D_SYMBOLS};
use super::ucode::{self, COUNT, Law};
use crate::scrambler::{Descrambler, Polynomial};
use crate::v34::mp::{self, Mp};

// Frames of Sd in a row before the frame alignment counts.
const SD_FRAMES: usize = 8;
// Of the large symbols of Sd, the most that its zeros and their spread may be.
const SMALL: f64 = 0.25;
const SPREAD: f64 = 1.3;
// Passes of the whole DIL to learn from.
const DIL_PASSES: usize = 1;
// Frames of Ri before R̄i counts.
const RI_FRAMES: usize = 4;
const R_BAR_SYMBOLS: usize = 24;
// Jd's fill and Jd′.
const JD_PRIME_ZEROS: usize = 4 + JD_PRIME_BITS;
const ED_FRAMES: usize = 2;
const B1D_FRAMES: usize = 48;

/// What the receiver has heard that the transmitter acts on.
#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag for each signal that § 9.3.2 and § 9.4.2 wait for"
)]
pub struct Events {
    /// The Sd to S̄d transition, which ends Ja.
    pub sd: bool,
    pub jd: Option<Jd>,
    /// Jd′, which ends S.
    pub jd_prime: bool,
    /// What DIL showed, once enough of it has come.
    pub levels: Option<Levels>,
    /// The Ri to R̄i transition, which ends CPt.
    pub r_bar: bool,
    pub mp: Option<Mp>,
    /// MP′ or Ed, which ends CP′.
    pub mp_ack: bool,
    /// Rate renegotiations the digital modem has started or answered, by
    /// the R̄d of each.
    pub renegotiations: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Sd { run: usize, last: Option<usize> },
    Trn { from: usize },
    Jd,
    Dil { from: usize },
    Ri { frames: usize },
    Training { from: usize },
    Data,
}

#[derive(Debug, Clone, Copy, Default)]
struct Stat {
    sum: f64,
    squares: f64,
    count: u32,
}

/// Hears the digital modem from its phase 3 on.
#[derive(Debug)]
pub struct Downstream {
    uinfo: u8,
    law: Law,
    dil: Dil,
    dil_symbols: usize,
    stage: Stage,
    count: usize,
    window: VecDeque<f64>,
    origin: usize,
    gain: f64,
    signs: SignReader,
    jd: Deframer<Jd>,
    zeros: Option<usize>,
    stats: Vec<[Stat; 2]>,
    training: Option<Mapping>,
    data: Option<Mapping>,
    /// The mapping that TRN2d or MP will come with.
    next: Option<Mapping>,
    /// Frames of Rd in a row, at each sample of the data frame they might start at.
    rd_frames: [usize; FRAME],
    decoder: Option<Decoder>,
    descrambler: Descrambler,
    mp: mp::Deframer,
    frame: Vec<f64>,
    zero_frames: usize,
    skip_frames: usize,
    events: Events,
}

impl Downstream {
    /// For the DIL that `descriptor` asks for, and training on `uinfo`.
    #[must_use]
    pub fn new(uinfo: u8, law: Law, descriptor: &Descriptor) -> Self {
        let pass: usize = descriptor
            .training
            .iter()
            .map(|&u| (usize::from(descriptor.lengths[usize::from(u / 16).min(7)]) + 1) * FRAME)
            .sum();
        Self {
            uinfo,
            law,
            dil: Dil::new(descriptor.clone()),
            dil_symbols: pass * DIL_PASSES,
            stage: Stage::Sd { run: 0, last: None },
            count: 0,
            window: VecDeque::from(vec![0.0; FRAME]),
            origin: 0,
            gain: 1.0,
            signs: SignReader::default(),
            jd: Deframer::default(),
            zeros: None,
            stats: vec![[Stat::default(); 2]; FRAME * usize::from(COUNT)],
            training: None,
            data: None,
            next: None,
            rd_frames: [0; FRAME],
            decoder: None,
            descrambler: Descrambler::with(Polynomial::V34_CALL),
            mp: mp::Deframer::default(),
            frame: Vec::with_capacity(FRAME),
            zero_frames: 0,
            skip_frames: 0,
            events: Events::default(),
        }
    }

    #[must_use]
    pub fn events(&self) -> &Events {
        &self.events
    }

    /// The constellations of CPt, which TRN2d, MP and Ed use, and of CP,
    /// which B1d and data use.
    pub fn set_mappings(&mut self, training: Option<Mapping>, data: Option<Mapping>) {
        self.training = training;
        self.data = data;
    }

    #[must_use]
    pub fn has_mappings(&self) -> bool {
        self.training.is_some()
    }

    /// Whether B1d has passed and data is coming.
    #[must_use]
    pub fn in_data(&self) -> bool {
        self.stage == Stage::Data && self.skip_frames == 0 && self.decoder.is_some()
    }

    /// The data bits in `input`.
    pub fn receive(&mut self, input: &[i16], data: &mut Vec<bool>) {
        for &sample in input {
            let x = f64::from(sample);
            self.window.pop_front();
            self.window.push_back(x);
            self.take(x, data);
            self.count += 1;
        }
    }

    fn at_frame_end(&self) -> bool {
        (self.count + 1)
            .checked_sub(self.origin)
            .is_some_and(|n| n % FRAME == 0)
    }

    // The last six samples as a frame of four large symbols and two small, and the signs of the first.
    fn sd_like(&self) -> Option<bool> {
        let w: Vec<f64> = self.window.iter().copied().collect();
        let large = [w[0], w[2], w[3], w[5]].map(f64::abs);
        let least = large.iter().copied().fold(f64::INFINITY, f64::min);
        let most = large.iter().copied().fold(0.0, f64::max);
        if least <= 0.0 || most > SPREAD * least || w[1].abs().max(w[4].abs()) > SMALL * least {
            return None;
        }
        let positive = w[0] > 0.0;
        let pattern = w[0] > 0.0 && w[2] > 0.0 && w[3] < 0.0 && w[5] < 0.0;
        let bar = w[0] < 0.0 && w[2] < 0.0 && w[3] > 0.0 && w[5] > 0.0;
        (pattern || bar).then_some(positive)
    }

    // The last frame as R, + + + − − −, or R̄ when false, at `expected` in each interval.
    fn r_like(&self, expected: [f64; FRAME]) -> Option<bool> {
        let w: Vec<f64> = self.window.iter().copied().collect();
        if w.iter()
            .zip(expected)
            .any(|(x, level)| (x.abs() - level).abs() > 0.25 * level)
        {
            return None;
        }
        let r = w[..3].iter().all(|&x| x > 0.0) && w[3..].iter().all(|&x| x < 0.0);
        let bar = w[..3].iter().all(|&x| x < 0.0) && w[3..].iter().all(|&x| x > 0.0);
        (r || bar).then_some(r)
    }

    fn take(&mut self, x: f64, data: &mut Vec<bool>) {
        let n = self.count;
        match self.stage {
            Stage::Sd { run, last } => self.sd(n, run, last),
            Stage::Trn { from } => self.trn(n, from, x),
            Stage::Jd => self.jd(n, x),
            Stage::Dil { from } => self.dil(n, from, x),
            Stage::Ri { frames } => self.ri(n, frames),
            Stage::Training { from } => {
                if n == from {
                    self.decoder = self.next.take().map(Decoder::new);
                    self.descrambler = Descrambler::with(Polynomial::V34_CALL);
                }
                if n >= from {
                    self.symbol(x, data);
                }
            }
            Stage::Data => {
                if !self.renegotiated(n) {
                    self.symbol(x, data);
                }
            }
        }
    }

    fn sd(&mut self, n: usize, run: usize, last: Option<usize>) {
        let Some(positive) = self.sd_like() else {
            return;
        };
        let start = n + 1 - FRAME;
        let follows = last.is_some_and(|last| last + FRAME == n);
        if positive && (follows || run == 0) {
            let run = if follows { run + 1 } else { 1 };
            self.stage = Stage::Sd { run, last: Some(n) };
        } else if !positive && follows && run >= SD_FRAMES {
            self.origin = start;
            self.events.sd = true;
            self.gain = 0.0;
            self.stage = Stage::Trn {
                from: start + super::training::SD_BAR_SYMBOLS,
            };
        } else if positive {
            self.stage = Stage::Sd {
                run: 1,
                last: Some(n),
            };
        }
    }

    fn trn(&mut self, n: usize, from: usize, x: f64) {
        if n < from {
            return;
        }
        self.gain += x.abs();
        self.signs.trn(x > 0.0);
        if n + 1 == from + TRN1D_SYMBOLS {
            #[expect(clippy::cast_precision_loss, reason = "2040 symbols")]
            let mean = self.gain / TRN1D_SYMBOLS as f64;
            self.gain = mean / f64::from(ucode::linear(self.uinfo, self.law));
            self.stage = Stage::Jd;
        }
    }

    fn ri(&mut self, n: usize, frames: usize) {
        if !self.at_frame_end() {
            return;
        }
        let uinfo = self.gain * f64::from(ucode::linear(self.uinfo, self.law));
        match self.r_like([uinfo; FRAME]) {
            Some(true) => self.stage = Stage::Ri { frames: frames + 1 },
            Some(false) if frames >= RI_FRAMES => {
                self.events.r_bar = true;
                self.next = self.training.clone();
                self.stage = Stage::Training {
                    from: n + 1 - FRAME + R_BAR_SYMBOLS,
                };
            }
            _ => {}
        }
    }

    fn jd(&mut self, n: usize, x: f64) {
        let bit = self.signs.push(x > 0.0);
        if let Some(jd) = self.jd.push(bit) {
            self.events.jd = Some(jd);
            self.zeros = Some(0);
            return;
        }
        self.zeros = self.zeros.filter(|_| !bit).map(|zeros| zeros + 1);
        if self.zeros == Some(JD_PRIME_ZEROS) {
            self.events.jd_prime = true;
            self.stage = if self.dil.is_empty() {
                Stage::Ri { frames: 0 }
            } else {
                Stage::Dil { from: n + 1 }
            };
        }
    }

    fn dil(&mut self, n: usize, from: usize, x: f64) {
        if n < from {
            return;
        }
        let Some(expected) = self.dil.next() else {
            return;
        };
        let interval = (n - self.origin) % FRAME;
        let stat = &mut self.stats[interval * usize::from(COUNT) + usize::from(expected.ucode)]
            [usize::from(expected.positive)];
        stat.sum += x;
        stat.squares += x * x;
        stat.count += 1;
        if n + 1 == from + self.dil_symbols {
            self.events.levels = Some(self.levels());
            self.stage = Stage::Ri { frames: 0 };
        }
    }

    fn levels(&self) -> Levels {
        let mut levels = Levels::table(self.law);
        let mut spread = [0.0; UCHORDS];
        let mut freedom = [0.0; UCHORDS];
        for (index, [negative, positive]) in self.stats.iter().enumerate() {
            if negative.count == 0 || positive.count == 0 {
                continue;
            }
            let ucode = index % usize::from(COUNT);
            let mean = |s: &Stat| s.sum / f64::from(s.count);
            levels.levels[index / usize::from(COUNT)][ucode] =
                (mean(positive) - mean(negative)) / 2.0;
            for s in [negative, positive] {
                spread[ucode / 16] += (s.squares - s.sum * mean(s)).max(0.0);
                freedom[ucode / 16] += f64::from(s.count - 1);
            }
        }
        let noise: [Option<f64>; UCHORDS] =
            std::array::from_fn(|c| (freedom[c] > 0.0).then(|| (spread[c] / freedom[c]).sqrt()));
        let worst = noise.iter().flatten().copied().fold(0.0, f64::max);
        levels.noise = noise.map(|n| n.unwrap_or(worst));
        levels
    }

    // The nearest point of the interval's constellation, at the levels DIL showed.
    fn slice(&self, x: f64, interval: usize, mapping: &Mapping) -> Codeword {
        let levels = self.events.levels.as_ref();
        let level = |u: u8| {
            levels.map_or(f64::from(ucode::linear(u, self.law)) * self.gain, |l| {
                l.levels[interval][usize::from(u)]
            })
        };
        let ucode = mapping.sets[interval]
            .iter()
            .copied()
            .min_by(|&a, &b| {
                (x.abs() - level(a))
                    .abs()
                    .total_cmp(&(x.abs() - level(b)).abs())
            })
            .unwrap_or(0);
        Codeword {
            ucode,
            positive: x > 0.0,
        }
    }

    fn symbol(&mut self, x: f64, data: &mut Vec<bool>) {
        self.frame.push(x);
        if self.frame.len() < FRAME {
            return;
        }
        let samples = std::mem::take(&mut self.frame);
        let Some(mapping) = self.decoder.as_ref().map(|d| d.mapping().clone()) else {
            return;
        };
        if self.stage == Stage::Data && self.rd_frames[self.count % FRAME] > 0 {
            return;
        }
        let codewords: [Codeword; FRAME] =
            std::array::from_fn(|interval| self.slice(samples[interval], interval, &mapping));
        let Some(decoder) = &mut self.decoder else {
            return;
        };
        let bits: Vec<bool> = decoder
            .frame(codewords)
            .into_iter()
            .map(|bit| self.descrambler.descramble(bit))
            .collect();
        if self.stage == Stage::Data {
            if self.skip_frames > 0 {
                self.skip_frames -= 1;
            } else {
                data.extend(bits);
            }
            return;
        }
        for &bit in &bits {
            if let Some(mp) = self.mp.push(bit) {
                self.events.mp_ack |= mp.acknowledge;
                self.events.mp = Some(mp);
            }
        }
        let zero = bits.iter().all(|&bit| !bit);
        self.zero_frames = if zero { self.zero_frames + 1 } else { 0 };
        if self.events.mp.is_some() && self.zero_frames == ED_FRAMES {
            self.events.mp_ack = true;
            self.stage = Stage::Data;
            self.decoder = self.data.clone().map(Decoder::new);
            self.descrambler = Descrambler::with(Polynomial::V34_CALL);
            self.skip_frames = B1D_FRAMES;
        }
    }

    // § 9.6.2.2.1: Rd holds data mode and R̄d starts MP, at any sample so that R̄d undoes a slip.
    fn renegotiated(&mut self, n: usize) -> bool {
        let Some(data) = self.decoder.as_ref().map(|d| d.mapping().clone()) else {
            return false;
        };
        let phase = n % FRAME;
        let largest: [f64; FRAME] = std::array::from_fn(|i| {
            let ucode = data.sets[i].first().copied().unwrap_or(0);
            self.events
                .levels
                .as_ref()
                .map_or(f64::from(ucode::linear(ucode, self.law)), |l| {
                    l.levels[i][usize::from(ucode)]
                })
        });
        match self.r_like(largest) {
            Some(true) => self.rd_frames[phase] += 1,
            Some(false) if self.rd_frames[phase] >= RI_FRAMES => {
                self.rd_frames = [0; FRAME];
                self.expect_renegotiation();
                self.events.renegotiations += 1;
                self.next = self
                    .training
                    .as_ref()
                    .map(|training| Mapping::renegotiating(training, &data));
                self.origin = n + 1 - FRAME;
                self.frame.clear();
                self.stage = Stage::Training {
                    from: self.origin + R_BAR_SYMBOLS,
                };
                return true;
            }
            _ => self.rd_frames[phase] = 0,
        }
        false
    }

    /// Forgets the MP of data mode and listens for a new one, as § 9.6 has
    /// the digital modem send after R̄d.
    pub fn expect_renegotiation(&mut self) {
        self.events.mp = None;
        self.events.mp_ack = false;
        self.mp = mp::Deframer::default();
        self.zero_frames = 0;
    }
}
