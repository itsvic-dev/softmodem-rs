// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The side of the modem that a computer sees.

pub mod command;
pub mod cuse;
pub mod escape;
pub mod line;
pub mod port;
pub mod pty;
pub mod settings;
pub mod tcp;
pub mod tty;
