// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! INFO sequences on the line: binary DPSK at 600 bit/s (§ 10.1.2.3.1).

use std::collections::VecDeque;

use super::{carrier_hz, info_level_dbm0};
use crate::passband::{Complex, Receiver, Transmitter, V22_PULSE};

const SPAN: usize = V22_PULSE.span;
use crate::pump::Role;

// V.22 table 3, as the carriers and the pulse are V.22's.
const CARRIER_OFF_SAMPLES: u32 = 136;

/// Sends INFO sequences from one end: a 1 turns the carrier by 180°.
#[derive(Debug)]
pub struct Modulator {
    transmitter: Transmitter,
    symbols: VecDeque<Complex>,
    point: Complex,
    silent_symbols: usize,
}

impl Modulator {
    /// For the end in `role`, at its carrier and level.
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            transmitter: Transmitter::new(carrier_hz(role), info_level_dbm0(role)),
            symbols: VecDeque::new(),
            point: (1.0, 0.0),
            silent_symbols: 2 * SPAN + 1,
        }
    }

    /// Queues sequences sent as a group, after the point at an arbitrary
    /// phase that starts the group.
    pub fn send(&mut self, bits: impl IntoIterator<Item = bool>) {
        if self.symbols.is_empty() {
            self.symbols.push_back(self.point);
        }
        for bit in bits {
            if bit {
                self.point = (-self.point.0, -self.point.1);
            }
            self.symbols.push_back(self.point);
        }
    }

    /// Fills `out`, and silence once the queue is empty.
    pub fn render(&mut self, out: &mut [i16]) {
        let symbols = &mut self.symbols;
        let silent_symbols = &mut self.silent_symbols;
        self.transmitter.render(out, || {
            if let Some(symbol) = symbols.pop_front() {
                *silent_symbols = 0;
                symbol
            } else {
                *silent_symbols += 1;
                (0.0, 0.0)
            }
        });
    }

    /// Whether everything queued has been sent, pulses and all.
    #[must_use]
    pub fn idle(&self) -> bool {
        self.symbols.is_empty() && self.silent_symbols > 2 * SPAN
    }
}

/// Turns the far end's INFO sequences back into bits.
#[derive(Debug)]
pub struct Demodulator {
    receiver: Receiver,
    last: Option<Complex>,
}

impl Demodulator {
    /// For what the end in `role` sends.
    #[must_use]
    pub fn new(role: Role) -> Self {
        Self {
            receiver: Receiver::new(carrier_hz(role), CARRIER_OFF_SAMPLES),
            last: None,
        }
    }

    /// Appends a bit for each symbol after the first while a signal is on
    /// the line.
    pub fn process(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        for &sample in input {
            let Some(strobe) = self.receiver.push(sample) else {
                continue;
            };
            if !strobe.present {
                self.last = None;
                continue;
            }
            let symbol = strobe.symbol;
            if let Some(last) = self.last {
                bits.push(symbol.0 * last.0 + symbol.1 * last.1 < 0.0);
            }
            self.last = Some(symbol);
        }
    }
}
