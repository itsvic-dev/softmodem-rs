//! A V.21 modem that a computer drives with AT commands.

mod line;
mod modem;

pub use line::Role;
pub use modem::{Modem, profile};
