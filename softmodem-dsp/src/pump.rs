// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Data pumps: one modulation's handshake, modulator and demodulator, chosen
//! per call.

use std::fmt::Debug;

use crate::uart::Decoder;
use crate::v21::V21;
use crate::v22::V22;
use crate::v22bis::V22bis;
use crate::v34::pump::V34;
use crate::v90::analogue::Analogue;
use crate::v90::digital::Digital;
use crate::v90::fallback::Fallback;

/// Which end of the link this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Originate,
    Answer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Modulation {
    #[default]
    V21,
    V22,
    V22bis,
    /// V.34 duplex, whose phase 1 is V.8.
    V34,
    /// V.90, whose phase 1 is V.8. The answer modem is the digital modem.
    V90,
}

impl Modulation {
    /// The pump from the end of its start-up: the answer tone, or for V.34
    /// and V.90 the silence after CJ.
    #[must_use]
    pub fn pump(self, role: Role) -> Box<dyn DataPump> {
        self.pump_up_to(role, None)
    }

    /// As [`Modulation::pump`], transmitting at `max_transmit` bit/s at most.
    /// The V.90 analogue modem enables no faster upstream rate; the others
    /// take no limit yet.
    #[must_use]
    pub fn pump_up_to(self, role: Role, max_transmit: Option<u32>) -> Box<dyn DataPump> {
        match (self, role) {
            (Self::V21, _) => Box::new(V21::new(role)),
            (Self::V22, _) => Box::new(V22::new(role)),
            (Self::V22bis, _) => Box::new(V22bis::new(role)),
            (Self::V34, _) => Box::new(V34::new(role)),
            (Self::V90, Role::Answer) => Box::new(Fallback::new(Digital::new())),
            (Self::V90, Role::Originate) => Box::new(Fallback::new(Analogue::up_to(max_transmit))),
        }
    }
}

/// The modulation for a call, whether automode may fall back from it to the
/// best one the far end also has, and the highest rate to transmit at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub top: Modulation,
    pub automode: bool,
    pub max_transmit: Option<u32>,
}

impl Offer {
    #[must_use]
    pub fn pump(self, role: Role) -> Box<dyn DataPump> {
        if self.automode {
            crate::automode::pump_up_to(self.top, role, self.max_transmit)
        } else if matches!(self.top, Modulation::V34 | Modulation::V90) {
            crate::automode::only_up_to(self.top, role, self.max_transmit)
        } else {
            self.top.pump_up_to(role, self.max_transmit)
        }
    }
}

/// Carries bits over one modulation, from the end of the V.25 answer tone,
/// or from the start of the call for a pump that sends its own answer tone.
pub trait DataPump: Debug + Send {
    /// Whether this pump sends and hears the answer tone itself, so the line
    /// must not.
    fn sends_own_answer_tone(&self) -> bool {
        false
    }

    /// Whether the far end has answered this modulation's first signal, so
    /// that it is the one to go on with.
    fn engaged(&self) -> bool {
        self.connected()
    }

    fn bit_rate(&self) -> u32;

    /// The rate this end transmits at, where it differs from the one it
    /// receives at, as in V.90.
    fn transmit_rate(&self) -> u32 {
        self.bit_rate()
    }

    /// A decoder for the start-stop characters this modulation carries.
    fn decoder(&self) -> Decoder;

    /// Asks the far end to train again, for a modulation that can.
    fn retrain(&mut self) {}

    /// Starts a rate renegotiation from data mode, for a modulation that has
    /// one. Data stops until both ends are in data mode again.
    fn renegotiate(&mut self) {}

    /// Starts ending the call with the far end, for a modulation that can.
    /// [`DataPump::cleared`] then says when it has.
    fn clear_down(&mut self) {}

    /// Whether the two ends have agreed to end the call, from either side.
    fn cleared(&self) -> bool {
        false
    }

    /// Queues bits to send. The line idles on mark when none are queued.
    fn push_bits(&mut self, bits: &[bool]);

    /// Bits queued but not yet started.
    fn pending(&self) -> usize;

    /// Fills all of `out`.
    fn transmit(&mut self, out: &mut [i16]);

    /// Appends the bits found in `input` to `bits`.
    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>);

    fn carrier(&self) -> bool;

    /// Whether the handshake is done, so that the modem can report CONNECT.
    fn connected(&self) -> bool;

    /// What the pump sends and listens for now, in words that change only
    /// as the handshake moves on, for a log or a replay to follow.
    fn stage(&self) -> String;
}
