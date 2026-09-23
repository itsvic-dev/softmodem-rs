//! Data pumps: one modulation's handshake, modulator and demodulator, chosen
//! per call.

use std::fmt::Debug;

use crate::uart::Decoder;
use crate::v21::V21;
use crate::v22::V22;
use crate::v22bis::V22bis;

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
}

impl Modulation {
    #[must_use]
    pub fn pump(self, role: Role) -> Box<dyn DataPump> {
        match self {
            Self::V21 => Box::new(V21::new(role)),
            Self::V22 => Box::new(V22::new(role)),
            Self::V22bis => Box::new(V22bis::new(role)),
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

    /// A decoder for the start-stop characters this modulation carries.
    fn decoder(&self) -> Decoder;

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
}
