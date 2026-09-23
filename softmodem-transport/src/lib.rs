//! Carries modem audio between two ends.

pub mod alaw;
pub mod reorder;
pub mod wav;
pub mod wire;

use std::io;

use tokio::sync::mpsc;

/// Samples in one 20 ms frame at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;

/// A call in progress. Dropping it hangs up.
#[derive(Debug)]
pub struct Call {
    /// Frames of linear samples to send, paced by the caller.
    pub audio_out: mpsc::Sender<Vec<i16>>,
    /// Received samples, in order, with gaps filled with silence. It closes
    /// when the far end hangs up.
    pub audio_in: mpsc::Receiver<Vec<i16>>,
}

/// Something that can place and take calls.
pub trait Transport {
    fn dial(&mut self, number: &str) -> impl Future<Output = io::Result<Call>> + Send;
    fn accept(&mut self) -> impl Future<Output = io::Result<Call>> + Send;
}
