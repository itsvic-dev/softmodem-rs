// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Two instances talking directly over UDP, for development. Audio is A-law
//! RTP. Call setup is text messages on the same socket, which cannot be
//! mistaken for RTP because RTP's first byte has the version bits set:
//! `DIAL <number>`, repeated until the far end sends `ANSWER` or `BUSY`, and
//! `BYE` to hang up or to abandon a call that is still ringing. The number
//! keeps any digits after a comma, for a far end that is a phone and can key
//! them out of band once its own call is answered.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::info;

pub use crate::rtp::Impairment;
use crate::rtp::{Session, Signalling};
use crate::{Call, DialError, Incoming, Transport};

const DIAL_INTERVAL: Duration = Duration::from_millis(500);
const RING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub struct Wire {
    socket: Arc<UdpSocket>,
    peer: Option<SocketAddr>,
    impairment: Impairment,
    ringing: Option<(SocketAddr, Instant)>,
}

impl Wire {
    /// `peer` is where [`Transport::dial`] calls. Calls are taken from anyone.
    ///
    /// # Errors
    ///
    /// Fails if `local` cannot be bound.
    pub async fn bind(
        local: SocketAddr,
        peer: Option<SocketAddr>,
        impairment: Impairment,
    ) -> io::Result<Self> {
        Ok(Self {
            socket: Arc::new(UdpSocket::bind(local).await?),
            peer,
            impairment,
            ringing: None,
        })
    }

    /// The bound address, which the far end dials.
    ///
    /// # Errors
    ///
    /// Fails if the socket has no local address.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn start(&self, peer: SocketAddr, answering: bool) -> Call {
        Session {
            socket: self.socket.clone(),
            peer,
            signalling: Signalling::Wire { answering },
            impairment: self.impairment,
        }
        .start(None, None)
    }
}

// Sends BYE when a dial is dropped while the call still rings.
struct Abandon {
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    armed: bool,
}

impl Drop for Abandon {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.socket.try_send_to(b"BYE", self.peer);
        }
    }
}

impl Transport for Wire {
    type Caller = SocketAddr;

    fn dials_after_answer(&self) -> bool {
        true
    }

    async fn dial(&mut self, number: &str) -> Result<Call, DialError> {
        let peer = self
            .peer
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no peer to dial"))?;
        let mut abandon = Abandon {
            socket: self.socket.clone(),
            peer,
            armed: true,
        };
        let request = format!("DIAL {number}");
        let mut buf = [0; 64];
        loop {
            self.socket.send_to(request.as_bytes(), peer).await?;
            let deadline = Instant::now() + DIAL_INTERVAL;
            while let Ok(received) = timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.socket.recv_from(&mut buf),
            )
            .await
            {
                let (n, from) = received?;
                if from != peer {
                    continue;
                }
                match &buf[..n] {
                    b"ANSWER" => {
                        abandon.armed = false;
                        info!(%peer, number, "answered");
                        return Ok(self.start(peer, false));
                    }
                    b"BUSY" => {
                        abandon.armed = false;
                        return Err(DialError::Busy);
                    }
                    _ => {}
                }
            }
        }
    }

    async fn incoming(&mut self) -> io::Result<Incoming<SocketAddr>> {
        let mut buf = [0; 64];
        loop {
            let gives_up = self.ringing.map(|(_, heard)| heard + RING_TIMEOUT);
            let received = tokio::select! {
                received = self.socket.recv_from(&mut buf) => received?,
                () = sleep_until(gives_up.unwrap_or_else(Instant::now)), if gives_up.is_some() => {
                    if let Some((caller, _)) = self.ringing.take() {
                        return Ok(Incoming::Gone(caller));
                    }
                    continue;
                }
            };
            let (n, from) = received;
            let ringing = self.ringing.map(|(caller, _)| caller);
            match (&buf[..n], ringing) {
                (message, None) if message.starts_with(b"DIAL ") => {
                    let number = String::from_utf8_lossy(&message[5..]).into_owned();
                    info!(%from, number, "incoming call");
                    self.ringing = Some((from, Instant::now()));
                    return Ok(Incoming::Ringing {
                        caller: from,
                        number,
                    });
                }
                (message, Some(caller)) if message.starts_with(b"DIAL ") => {
                    if from == caller {
                        self.ringing = Some((caller, Instant::now()));
                    } else {
                        self.socket.send_to(b"BUSY", from).await?;
                    }
                }
                (b"BYE", Some(caller)) if from == caller => {
                    self.ringing = None;
                    return Ok(Incoming::Gone(caller));
                }
                _ => {}
            }
        }
    }

    async fn answer(&mut self, caller: &SocketAddr) -> io::Result<Call> {
        self.ringing = None;
        self.socket.send_to(b"ANSWER", caller).await?;
        Ok(self.start(*caller, true))
    }

    async fn reject(&mut self, caller: &SocketAddr) -> io::Result<()> {
        self.ringing = None;
        self.socket.send_to(b"BUSY", caller).await?;
        Ok(())
    }
}
