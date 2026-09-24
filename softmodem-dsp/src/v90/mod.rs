//! V.90: the digital modem, which sends PCM codewords down, and the
//! analogue modem, which receives them and sends V.34 up.

pub mod analogue;
pub mod digital;
pub mod info;
pub mod ucode;
