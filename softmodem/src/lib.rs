//! A V.21, V.22 and V.22bis modem that a computer drives with AT commands.

mod line;
mod modem;

pub use modem::{Modem, profile};
pub use softmodem_dsp::pump::Role;
