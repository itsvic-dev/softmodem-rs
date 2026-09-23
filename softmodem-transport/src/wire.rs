//! Two instances talking directly over UDP, for development. Audio is A-law
//! RTP. Call setup is three text messages on the same socket: `DIAL
//! <number>`, `ANSWER` and `BYE`, which cannot be mistaken for RTP because
//! RTP's first byte has the version bits set.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ezk_rtp::{RtpExtensionIds, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep_until, timeout};
use tracing::{debug, info, warn};

use crate::reorder::Reorder;
use crate::{Call, Transport, alaw};

const PCMA: u8 = 8;
const DIAL_ATTEMPTS: u32 = 20;
const DIAL_INTERVAL: Duration = Duration::from_millis(500);
const SILENCE_TIMEOUT: Duration = Duration::from_secs(5);
const REORDER_WINDOW: usize = 3;
const BYE_REPEATS: usize = 3;

/// Damage done to outgoing packets, to exercise the far end's receive path.
#[derive(Debug, Clone, Copy, Default)]
pub struct Impairment {
    /// Chance that a packet is dropped.
    pub loss: f64,
    /// Chance that a packet is held back and sent after the next one.
    pub reorder: f64,
    pub seed: u64,
}

#[derive(Debug)]
pub struct Wire {
    socket: Arc<UdpSocket>,
    peer: Option<SocketAddr>,
    impairment: Impairment,
}

impl Wire {
    /// `peer` is needed to dial. An answering wire learns it from the call.
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
        let (audio_out, outgoing) = mpsc::channel(8);
        let (incoming, audio_in) = mpsc::channel(64);
        let session = Session {
            socket: self.socket.clone(),
            peer,
            answering,
            impairment: self.impairment,
            rng: fastrand::Rng::with_seed(self.impairment.seed),
            held_back: None,
        };
        tokio::spawn(session.run(outgoing, incoming));
        Call {
            audio_out,
            audio_in,
        }
    }
}

impl Transport for Wire {
    async fn dial(&mut self, number: &str) -> io::Result<Call> {
        let peer = self
            .peer
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no peer to dial"))?;
        let request = format!("DIAL {number}");
        let mut buf = [0; 64];
        for _ in 0..DIAL_ATTEMPTS {
            self.socket.send_to(request.as_bytes(), peer).await?;
            let deadline = Instant::now() + DIAL_INTERVAL;
            while let Ok(received) = timeout(
                deadline.saturating_duration_since(Instant::now()),
                self.socket.recv_from(&mut buf),
            )
            .await
            {
                let (n, from) = received?;
                if from == peer && &buf[..n] == b"ANSWER" {
                    info!(%peer, number, "answered");
                    return Ok(self.start(peer, false));
                }
            }
        }
        Err(io::Error::new(io::ErrorKind::TimedOut, "no answer"))
    }

    async fn accept(&mut self) -> io::Result<Call> {
        let mut buf = [0; 64];
        loop {
            let (n, from) = self.socket.recv_from(&mut buf).await?;
            if let Some(number) = buf[..n].strip_prefix(b"DIAL ") {
                info!(%from, number = %String::from_utf8_lossy(number), "incoming call");
                self.socket.send_to(b"ANSWER", from).await?;
                return Ok(self.start(from, true));
            }
        }
    }
}

struct Session {
    socket: Arc<UdpSocket>,
    peer: SocketAddr,
    answering: bool,
    impairment: Impairment,
    rng: fastrand::Rng,
    held_back: Option<Vec<u8>>,
}

impl Session {
    async fn run(
        mut self,
        mut outgoing: mpsc::Receiver<Vec<i16>>,
        incoming: mpsc::Sender<Vec<i16>>,
    ) {
        let ssrc = Ssrc(fastrand::u32(..));
        let mut sequence = SequenceNumber(fastrand::u16(..));
        let mut timestamp = fastrand::u32(..);
        let mut reorder = Reorder::new(REORDER_WINDOW);
        let mut frames = Vec::new();
        let mut buf = vec![0; 2048];
        let mut last_heard = Instant::now();

        loop {
            tokio::select! {
                frame = outgoing.recv() => {
                    let Some(samples) = frame else {
                        self.hang_up().await;
                        break;
                    };
                    let packet = RtpPacket {
                        pt: PCMA,
                        sequence_number: sequence,
                        ssrc,
                        timestamp: RtpTimestamp(timestamp),
                        extensions: ezk_rtp::RtpExtensions::default(),
                        payload: Bytes::from_iter(samples.iter().map(|&s| alaw::encode(s))),
                    };
                    sequence.0 = sequence.0.wrapping_add(1);
                    timestamp = timestamp.wrapping_add(u32::try_from(samples.len()).unwrap_or(0));
                    if let Err(error) = self.send_impaired(packet.to_vec(RtpExtensionIds::default())).await {
                        warn!(%error, "send failed");
                        break;
                    }
                }
                received = self.socket.recv_from(&mut buf) => {
                    let (n, from) = match received {
                        Ok(received) => received,
                        Err(error) => {
                            warn!(%error, "receive failed");
                            break;
                        }
                    };
                    if from != self.peer {
                        continue;
                    }
                    last_heard = Instant::now();
                    let data = &buf[..n];
                    if data == b"BYE" {
                        info!("far end hung up");
                        break;
                    }
                    if data.starts_with(b"DIAL ") && self.answering {
                        let _ = self.socket.send_to(b"ANSWER", self.peer).await;
                        continue;
                    }
                    let Ok(packet) = RtpPacket::parse(RtpExtensionIds::default(), data.to_vec()) else {
                        continue;
                    };
                    if packet.pt != PCMA {
                        debug!(pt = packet.pt, "ignoring payload type");
                        continue;
                    }
                    let samples = packet.payload.iter().map(|&b| alaw::decode(b)).collect();
                    reorder.push(packet.sequence_number.0, packet.timestamp.0, samples, &mut frames);
                    for frame in frames.drain(..) {
                        let _ = incoming.send(frame).await;
                    }
                }
                () = sleep_until(last_heard + SILENCE_TIMEOUT) => {
                    warn!("far end went silent");
                    break;
                }
            }
        }

        reorder.flush(&mut frames);
        for frame in frames {
            let _ = incoming.send(frame).await;
        }
    }

    async fn send_impaired(&mut self, packet: Vec<u8>) -> io::Result<()> {
        if self.rng.f64() < self.impairment.loss {
            return Ok(());
        }
        if self.held_back.is_none() && self.rng.f64() < self.impairment.reorder {
            self.held_back = Some(packet);
            return Ok(());
        }
        self.socket.send_to(&packet, self.peer).await?;
        if let Some(held) = self.held_back.take() {
            self.socket.send_to(&held, self.peer).await?;
        }
        Ok(())
    }

    async fn hang_up(&self) {
        for _ in 0..BYE_REPEATS {
            let _ = self.socket.send_to(b"BYE", self.peer).await;
        }
    }
}
