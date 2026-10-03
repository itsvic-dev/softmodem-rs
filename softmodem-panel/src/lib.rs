// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The modem's front panel: its lights, and the text below them.

use std::time::Instant;

pub mod lights;
pub mod text;

/// What the front panel shows, as the modem last left it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(clippy::struct_excessive_bools, reason = "one for each light")]
pub struct Status {
    /// TR: a computer is on the serial port.
    pub terminal_ready: bool,
    /// AA: `S0` lets the modem answer by itself.
    pub auto_answer: bool,
    /// A ring is sounding, which lights AA.
    pub ringing: bool,
    /// OH.
    pub off_hook: bool,
    /// What the modems send and listen for, from the first tone of a call.
    pub stage: Option<String>,
    /// CD: the call is connected.
    pub connection: Option<Connection>,
    /// Bytes from the line to the computer, which flash RD as they grow.
    pub received: u64,
    /// Bytes from the computer to the line, which flash SD as they grow.
    pub sent: u64,
}

impl Status {
    /// MR blinks while the modems train.
    #[must_use]
    pub fn training(&self) -> bool {
        self.stage.is_some() && self.connection.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Connection {
    pub since: Instant,
    pub receive_rate: u32,
    pub transmit_rate: u32,
    /// ARQ: LAPM corrects errors.
    pub error_control: bool,
    pub compression: bool,
    /// I frames sent again, which flash ARQ as they grow.
    pub retransmissions: u64,
}
