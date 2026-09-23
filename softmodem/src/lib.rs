//! A V.21 modem that a computer drives with AT commands.

mod line;
mod modem;

pub use modem::{Modem, profile};
pub use softmodem_dsp::pump::Role;
