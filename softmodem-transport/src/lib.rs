//! Carries modem audio between two ends.

pub mod alaw;
pub mod loopback;
pub mod reorder;
pub mod wav;
pub mod wire;

use std::fmt;
use std::io;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Samples in one 20 ms frame at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;

/// A call in progress. Dropping it hangs up, but only [`Call::hang_up`]
/// waits until the far end has been told and any recording is complete.
#[derive(Debug)]
pub struct Call {
    /// Frames of linear samples to send, paced by the caller.
    pub audio_out: mpsc::Sender<Vec<i16>>,
    /// Received samples, in order, with gaps filled with silence. It closes
    /// when the far end hangs up.
    pub audio_in: mpsc::Receiver<Vec<i16>>,
    tasks: Vec<JoinHandle<()>>,
}

impl Call {
    pub async fn hang_up(self) {
        drop(self.audio_out);
        drop(self.audio_in);
        for task in self.tasks {
            let _ = task.await;
        }
    }
}

/// What an incoming caller did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming<C> {
    /// A call arrived and is waiting to be answered.
    Ringing { caller: C, number: String },
    /// The caller gave up before an answer.
    Gone(C),
}

#[derive(Debug)]
pub enum DialError {
    Busy,
    Io(io::Error),
}

impl fmt::Display for DialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("busy"),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for DialError {}

impl From<io::Error> for DialError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// A line that can place and take calls.
pub trait Transport {
    /// Identifies an incoming caller between [`Transport::incoming`] and
    /// [`Transport::answer`] or [`Transport::reject`].
    type Caller: Clone + PartialEq + fmt::Debug + Send;

    /// Places a call and waits, without limit, until it is answered or
    /// refused. Dropping the future abandons the call.
    fn dial(&mut self, number: &str) -> impl Future<Output = Result<Call, DialError>> + Send;

    /// Waits for the next thing an incoming caller does. While one call
    /// rings, any other caller is refused as busy.
    fn incoming(&mut self) -> impl Future<Output = io::Result<Incoming<Self::Caller>>> + Send;

    fn answer(&mut self, caller: &Self::Caller) -> impl Future<Output = io::Result<Call>> + Send;

    fn reject(&mut self, caller: &Self::Caller) -> impl Future<Output = io::Result<()>> + Send;
}
