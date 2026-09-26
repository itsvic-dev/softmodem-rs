// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! V.34 both ways, where phase 2 of V.90 settles on it (§ 9.2.1.1.8 and
//! § 9.2.2.1.9), with phase 2 of V.90 again for each retrain.

use crate::pump::DataPump;
use crate::uart::Decoder as Characters;
use crate::v34::pump::V34;

/// A V.90 modem under a [`Fallback`].
pub trait Settles: DataPump {
    /// V.34 from phase 3, once phase 2 has settled on it.
    fn settled_on_v34(&self) -> Option<V34>;

    /// Phase 2 of V.90 again, for a retrain from V.34 at `rate`, which
    /// connected if `online`.
    fn retrain_from_v34(&mut self, rate: u32, online: bool);
}

/// A V.90 modem that goes on in V.34 where phase 2 settles on it.
#[derive(Debug)]
pub struct Fallback<M> {
    v90: M,
    v34: Option<V34>,
    online: bool,
}

impl<M: Settles> Fallback<M> {
    #[must_use]
    pub fn new(v90: M) -> Self {
        Self {
            v90,
            v34: None,
            online: false,
        }
    }

    fn pump(&self) -> &dyn DataPump {
        match &self.v34 {
            Some(v34) => v34,
            None => &self.v90,
        }
    }

    fn pump_mut(&mut self) -> &mut dyn DataPump {
        match &mut self.v34 {
            Some(v34) => v34,
            None => &mut self.v90,
        }
    }

    fn leave_v34(&mut self) {
        if let Some(v34) = self.v34.take_if(|v34| v34.retrain_asked()) {
            self.v90.retrain_from_v34(v34.bit_rate(), self.online);
        }
    }
}

impl<M: Settles> DataPump for Fallback<M> {
    fn engaged(&self) -> bool {
        self.v90.engaged()
    }

    fn bit_rate(&self) -> u32 {
        self.pump().bit_rate()
    }

    fn transmit_rate(&self) -> u32 {
        self.pump().transmit_rate()
    }

    fn decoder(&self) -> Characters {
        self.pump().decoder()
    }

    fn retrain(&mut self) {
        self.pump_mut().retrain();
        self.leave_v34();
    }

    fn renegotiate(&mut self) {
        self.pump_mut().renegotiate();
    }

    fn clear_down(&mut self) {
        self.pump_mut().clear_down();
    }

    fn cleared(&self) -> bool {
        self.pump().cleared()
    }

    fn push_bits(&mut self, bits: &[bool]) {
        self.pump_mut().push_bits(bits);
    }

    fn pending(&self) -> usize {
        self.pump().pending()
    }

    fn transmit(&mut self, out: &mut [i16]) {
        self.pump_mut().transmit(out);
    }

    fn receive(&mut self, input: &[i16], bits: &mut Vec<bool>) {
        if let Some(v34) = &mut self.v34 {
            v34.receive(input, bits);
            self.online |= v34.connected();
            self.leave_v34();
        } else {
            self.v90.receive(input, bits);
            self.v34 = self.v90.settled_on_v34();
        }
    }

    fn carrier(&self) -> bool {
        self.pump().carrier()
    }

    fn connected(&self) -> bool {
        self.pump().connected()
    }
}
