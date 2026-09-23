//! Two transports joined in memory, for tests that must not wait on real
//! time or real sockets.

use std::io;

use tokio::sync::{mpsc, oneshot};

use crate::{Call, DialError, Incoming, Transport};

const AUDIO_QUEUE: usize = 64;

#[derive(Debug)]
struct Offer {
    number: String,
    reply: oneshot::Sender<Result<Call, DialError>>,
}

#[derive(Debug)]
pub struct Loopback {
    to_peer: mpsc::UnboundedSender<Offer>,
    from_peer: mpsc::UnboundedReceiver<Offer>,
    ringing: Option<Offer>,
}

/// Two ends of one line. What one dials, the other hears ring.
#[must_use]
pub fn pair() -> (Loopback, Loopback) {
    let (to_a, from_b) = mpsc::unbounded_channel();
    let (to_b, from_a) = mpsc::unbounded_channel();
    (
        Loopback {
            to_peer: to_b,
            from_peer: from_b,
            ringing: None,
        },
        Loopback {
            to_peer: to_a,
            from_peer: from_a,
            ringing: None,
        },
    )
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the other end is gone")
}

impl Transport for Loopback {
    type Caller = ();

    async fn dial(&mut self, number: &str) -> Result<Call, DialError> {
        let (reply, answer) = oneshot::channel();
        self.to_peer
            .send(Offer {
                number: number.to_owned(),
                reply,
            })
            .map_err(|_| gone())?;
        answer.await.map_err(|_| gone())?
    }

    async fn incoming(&mut self) -> io::Result<Incoming<()>> {
        loop {
            let Some(ringing) = self.ringing.as_mut() else {
                let offer = self.from_peer.recv().await.ok_or_else(gone)?;
                let number = offer.number.clone();
                self.ringing = Some(offer);
                return Ok(Incoming::Ringing { caller: (), number });
            };
            tokio::select! {
                biased;
                () = ringing.reply.closed() => {
                    self.ringing = None;
                    return Ok(Incoming::Gone(()));
                }
                offer = self.from_peer.recv() => {
                    let offer = offer.ok_or_else(gone)?;
                    let _ = offer.reply.send(Err(DialError::Busy));
                }
            }
        }
    }

    fn answer(&mut self, (): &()) -> impl Future<Output = io::Result<Call>> + Send {
        std::future::ready(self.connect())
    }

    fn reject(&mut self, (): &()) -> impl Future<Output = io::Result<()>> + Send {
        if let Some(offer) = self.ringing.take() {
            let _ = offer.reply.send(Err(DialError::Busy));
        }
        std::future::ready(Ok(()))
    }
}

impl Loopback {
    fn connect(&mut self) -> io::Result<Call> {
        let offer = self.ringing.take().ok_or_else(gone)?;
        let (to_answerer, from_caller) = mpsc::channel(AUDIO_QUEUE);
        let (to_caller, from_answerer) = mpsc::channel(AUDIO_QUEUE);
        let caller = Call {
            audio_out: to_answerer,
            audio_in: from_answerer,
            tasks: Vec::new(),
        };
        offer.reply.send(Ok(caller)).map_err(|_| gone())?;
        Ok(Call {
            audio_out: to_caller,
            audio_in: from_caller,
            tasks: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_dialled_call_rings_and_carries_audio_once_answered() {
        let (mut calling, mut answering) = pair();
        let dialling = tokio::spawn(async move { calling.dial("0300").await });
        assert_eq!(
            answering.incoming().await.unwrap(),
            Incoming::Ringing {
                caller: (),
                number: "0300".into()
            }
        );
        let mut answered = answering.answer(&()).await.unwrap();
        let placed = dialling.await.unwrap().unwrap();
        placed.audio_out.send(vec![1, 2, 3]).await.unwrap();
        assert_eq!(answered.audio_in.recv().await, Some(vec![1, 2, 3]));
        drop(placed);
        assert_eq!(answered.audio_in.recv().await, None);
    }

    #[tokio::test]
    async fn a_rejected_call_is_busy() {
        let (mut calling, mut answering) = pair();
        let dialling = tokio::spawn(async move { calling.dial("0300").await });
        answering.incoming().await.unwrap();
        answering.reject(&()).await.unwrap();
        assert!(matches!(dialling.await.unwrap(), Err(DialError::Busy)));
    }

    #[tokio::test]
    async fn an_abandoned_call_is_gone() {
        let (mut calling, mut answering) = pair();
        let dialling = tokio::spawn(async move { calling.dial("0300").await });
        answering.incoming().await.unwrap();
        dialling.abort();
        assert_eq!(answering.incoming().await.unwrap(), Incoming::Gone(()));
    }
}
