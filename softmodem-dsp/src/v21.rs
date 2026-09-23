//! V.21: 300 bit/s FSK, full duplex on two channels.

use crate::fsk::{Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE};
use crate::pump::{DataPump, Role};
use crate::uart::Decoder;

// Longer than the far end's carrier detect, so its first bytes are not lost.
const ORIGINATE_CARRIER_BEFORE_CONNECT: usize = 4_800;

/// The answering end sends carrier at once and connects when it hears the
/// caller's. The caller sends carrier when it hears the answering end's, and
/// connects a little later.
#[derive(Debug)]
pub(crate) struct V21 {
    role: Role,
    modulator: Modulator,
    demodulator: Demodulator,
    sent: usize,
    carrier_since: Option<usize>,
    heard_carrier: bool,
}

impl V21 {
    pub(crate) fn new(role: Role) -> Self {
        let (transmit, receive) = match role {
            Role::Originate => (V21_ORIGINATE, V21_ANSWER),
            Role::Answer => (V21_ANSWER, V21_ORIGINATE),
        };
        Self {
            role,
            modulator: Modulator::new(transmit, V21_MAX_LEVEL_DBM0),
            demodulator: Demodulator::new(receive),
            sent: 0,
            carrier_since: None,
            heard_carrier: false,
        }
    }
}

impl DataPump for V21 {
    fn bit_rate(&self) -> u32 {
        300
    }

    fn decoder(&self) -> Decoder {
        Decoder::new()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        self.modulator.push_bits(bits.iter().copied());
    }

    fn pending(&self) -> usize {
        self.modulator.pending()
    }

    fn transmit(&mut self, out: &mut [i16]) {
        match self.role {
            Role::Answer => self.modulator.render(out),
            Role::Originate if self.heard_carrier => {
                self.carrier_since.get_or_insert(self.sent);
                self.modulator.render(out);
            }
            Role::Originate => out.fill(0),
        }
        self.sent += out.len();
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        self.demodulator.process(input, bits);
        self.heard_carrier |= self.demodulator.carrier();
    }

    fn carrier(&self) -> bool {
        self.demodulator.carrier()
    }

    fn engaged(&self) -> bool {
        self.heard_carrier
    }

    fn connected(&self) -> bool {
        match self.role {
            Role::Answer => self.heard_carrier,
            Role::Originate => self
                .carrier_since
                .is_some_and(|since| self.sent - since >= ORIGINATE_CARRIER_BEFORE_CONNECT),
        }
    }
}
