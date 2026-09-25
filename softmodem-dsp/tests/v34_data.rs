// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.34 data mode on the line: encoder, modulator, front end, equaliser and
//! decoder, after the S, S̄, PP and TRN of phase 3.

mod common;

use common::{Noise, add_noise, resample_finely};
use softmodem_dsp::pump::Role;
use softmodem_dsp::v34::decoder::Decoder;
use softmodem_dsp::v34::detect::{Heard, SDetector};
use softmodem_dsp::v34::encoder::{Encoder, Settings};
use softmodem_dsp::v34::framing::Framing;
use softmodem_dsp::v34::modulator::{Modulator, SPAN};
use softmodem_dsp::v34::mp::{Asks, Trellis};
use softmodem_dsp::v34::receiver::{DELAY, Equalizer, FrontEnd, Symbol, Tracking};
use softmodem_dsp::v34::training::{self, PP_SYMBOLS, Points};
use softmodem_dsp::v34::{NOMINAL_DBM0, SymbolRate};

const S: usize = 128;
const S_BAR: usize = 16;
const TRN: usize = 1024;
const TRAINING: usize = S + S_BAR + PP_SYMBOLS + TRN;
const FRAMES: usize = 400;
const FRAME: usize = 160;

type Complex = (f64, f64);

struct Line {
    snr_db: Option<f64>,
    clock: f64,
}

const CLEAN: Line = Line {
    snr_db: None,
    clock: 1.0,
};

fn phase_3() -> Vec<Complex> {
    let mut sender = training::Sender::new(Role::Answer);
    (0..S)
        .map(training::s)
        .chain((0..S_BAR).map(training::s_bar))
        .chain((0..PP_SYMBOLS).map(training::pp))
        .chain((0..TRN).map(|_| sender.trn(Points::Four)))
        .collect()
}

// Phase 3 and then data from random bits, on the line, with the bits sent.
fn transmitted(
    symbol_rate: SymbolRate,
    framing: Framing,
    settings: Settings,
    known: &[Complex],
    line: &Line,
) -> (Vec<bool>, Vec<i16>) {
    let mut encoder = Encoder::new(framing, settings);
    let scale = encoder.energy().sqrt();
    let mut noise = Noise(0x0123_4567_89AB_CDEF);
    let mut sent: Vec<bool> = Vec::new();
    let mut points = known.to_vec();
    for _ in 0..FRAMES {
        let bits: Vec<bool> = (0..encoder.bits()).map(|_| noise.uniform() > 0.0).collect();
        sent.extend(&bits);
        points.extend(
            encoder
                .encode(&bits)
                .map(|(re, im)| (re / scale, im / scale)),
        );
    }
    let symbols = points.len();
    let mut modulator = Modulator::new(symbol_rate, false, 0, NOMINAL_DBM0);
    let mut queue = points.into_iter();
    let mut samples = vec![0; (symbols + 2 * SPAN + 40) * 4];
    modulator.render(&mut samples, || queue.next().unwrap_or((0.0, 0.0)));
    if let Some(snr) = line.snr_db {
        samples = add_noise(samples, NOMINAL_DBM0, snr);
    }
    (sent, resample_finely(&samples, line.clock))
}

// The receiver of data mode, trained on the known points once S̄ places them.
struct Receiver {
    front_end: FrontEnd,
    detector: SDetector,
    equalizer: Equalizer,
    decoder: Decoder,
    scale: f64,
    first_s_bar: Option<usize>,
    count: usize,
    received: Vec<bool>,
}

impl Receiver {
    fn new(symbol_rate: SymbolRate, framing: Framing, settings: Settings) -> Self {
        Self {
            front_end: FrontEnd::new(symbol_rate, false),
            detector: SDetector::default(),
            equalizer: Equalizer::default(),
            decoder: Decoder::new(framing, settings),
            scale: Encoder::new(framing, Settings::default()).energy().sqrt(),
            first_s_bar: None,
            count: 0,
            received: Vec::new(),
        }
    }

    fn receive(&mut self, samples: &[i16], known: &[Complex]) {
        for chunk in samples.chunks(FRAME) {
            for symbol in self.front_end.process(chunk) {
                self.symbol(&symbol, known);
            }
        }
    }

    fn symbol(&mut self, symbol: &Symbol, known: &[Complex]) {
        let z = self.equalizer.output(symbol);
        if self.first_s_bar.is_none()
            && let Some(Heard::SBar(at)) = self.detector.push(symbol)
        {
            self.first_s_bar = Some(at);
            self.front_end.track(Tracking::Train);
        }
        self.count += 1;
        let Some(index) = self
            .first_s_bar
            .and_then(|first| (self.count - 1 + S).checked_sub(first + DELAY))
        else {
            return;
        };
        if index < S + S_BAR {
            return;
        }
        if index < TRAINING {
            self.equalizer.adapt(known[index]);
            if index == TRAINING - 1 {
                self.front_end.track(Tracking::Data);
            }
            return;
        }
        self.data(z);
    }

    fn data(&mut self, z: Complex) {
        let scale = self.scale;
        let grid = (z.0 * scale, z.1 * scale);
        let target = self.decoder.push(grid, &mut self.received);
        self.equalizer.adapt((target.0 / scale, target.1 / scale));
    }
}

fn call(symbol_rate: SymbolRate, bit_rate: u32, asks: Asks, line: &Line) -> (usize, usize) {
    let framing = Framing::new(symbol_rate, bit_rate, asks.expanded_shaping).unwrap();
    let known = phase_3();
    let settings = asks.settings();
    let (sent, samples) = transmitted(symbol_rate, framing, settings, &known, line);
    let mut far = Receiver::new(symbol_rate, framing, settings);
    far.receive(&samples, &known);
    let wrong = sent
        .iter()
        .zip(&far.received)
        .filter(|(a, b)| a != b)
        .count();
    (wrong, far.received.len())
}

fn assert_clean(symbol_rate: SymbolRate, bit_rate: u32, line: &Line) {
    assert_clean_asking(symbol_rate, bit_rate, Asks::default(), line);
}

fn assert_clean_asking(symbol_rate: SymbolRate, bit_rate: u32, asks: Asks, line: &Line) {
    let (wrong, decided) = call(symbol_rate, bit_rate, asks, line);
    assert!(
        decided > 1000 && wrong == 0,
        "{symbol_rate:?} at {bit_rate} with {asks:?} would corrupt data: {wrong} of {decided} bits wrong"
    );
}

// What the ISP's modem asked of this one's V.90 upstream, with coefficients of its size.
const ALL_OF_IT: Asks = Asks {
    trellis: Trellis::States16,
    nonlinear: true,
    expanded_shaping: true,
    precoding: [(6000, -1500), (-2500, 800), (700, 300)],
};

#[test]
fn carries_precoded_data_with_expanded_shaping_and_theta() {
    let noisy = Line {
        snr_db: Some(40.0),
        clock: 1.0,
    };
    assert_clean_asking(SymbolRate::S3200, 28_800, ALL_OF_IT, &CLEAN);
    assert_clean_asking(SymbolRate::S3200, 24_000, ALL_OF_IT, &noisy);
    assert_clean_asking(SymbolRate::S3429, 33_600, ALL_OF_IT, &CLEAN);
}

#[test]
fn carries_data_on_a_clean_line() {
    assert_clean(SymbolRate::S2400, 9600, &CLEAN);
    assert_clean(SymbolRate::S3200, 24_000, &CLEAN);
    assert_clean(SymbolRate::S3429, 33_600, &CLEAN);
}

#[test]
fn carries_data_through_noise() {
    let noisy = Line {
        snr_db: Some(38.0),
        clock: 1.0,
    };
    assert_clean(SymbolRate::S3000, 14_400, &noisy);
    assert_clean(SymbolRate::S3200, 24_000, &noisy);
}

#[test]
fn follows_the_far_clock() {
    let drifting = Line {
        snr_db: Some(40.0),
        clock: 1.00005,
    };
    assert_clean(SymbolRate::S3200, 19_200, &drifting);
}
