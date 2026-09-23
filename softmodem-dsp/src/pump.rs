//! Data pumps: one modulation's handshake, modulator and demodulator, chosen
//! per call.

use std::fmt::Debug;

use crate::v21::V21;

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
}

impl Modulation {
    #[must_use]
    pub fn pump(self, role: Role) -> Box<dyn DataPump> {
        match self {
            Self::V21 => Box::new(V21::new(role)),
        }
    }
}

/// Carries bits over one modulation, from the end of the V.25 answer tone.
pub trait DataPump: Debug + Send {
    fn bit_rate(&self) -> u32;

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
