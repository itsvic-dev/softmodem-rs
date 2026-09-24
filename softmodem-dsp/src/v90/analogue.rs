//! The analogue modem: PCM codewords down from the digital modem, V.34 up.

use crate::pump::DataPump;
use crate::uart::Decoder as Characters;
use crate::v34::phase2::{PcmOutcome, Phase2};

/// The analogue modem, from phase 2 on.
#[derive(Debug)]
pub struct Analogue {
    phase2: Phase2,
    outcome: Option<PcmOutcome>,
}

impl Default for Analogue {
    fn default() -> Self {
        Self::new()
    }
}

impl Analogue {
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase2: Phase2::analogue(),
            outcome: None,
        }
    }
}

impl DataPump for Analogue {
    fn engaged(&self) -> bool {
        self.phase2.engaged()
    }

    fn bit_rate(&self) -> u32 {
        0
    }

    fn decoder(&self) -> Characters {
        Characters::v14()
    }

    fn push_bits(&mut self, _bits: &[bool]) {}

    fn pending(&self) -> usize {
        0
    }

    fn transmit(&mut self, out: &mut [i16]) {
        if self.outcome.is_some() {
            out.fill(0);
        } else {
            self.phase2.transmit(out);
        }
    }

    fn receive(&mut self, input: &[i16], _bits: &mut Vec<bool>) {
        if self.outcome.is_none() {
            self.phase2.receive(input);
            self.outcome = self.phase2.pcm_outcome().filter(|_| self.phase2.done());
        }
    }

    fn carrier(&self) -> bool {
        false
    }

    fn connected(&self) -> bool {
        false
    }
}
