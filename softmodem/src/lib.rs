// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! A V.21, V.22 and V.22bis modem that a computer drives with AT commands.

mod journal;
mod line;
mod modem;
pub mod replay;

pub use journal::Journal;
pub use modem::{Modem, profile};
pub use softmodem_dsp::pump::Role;
