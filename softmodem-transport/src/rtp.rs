// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! One call's A-law RTP stream on a UDP socket, shared by the transports.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use ezk_rtp::{RtpExtensionIds, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tracing::{debug, info, warn};

use crate::reorder::Reorder;
use crate::{Call, alaw};

pub(crate) const PCMA: u8 = 8;
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
    /// Chance that a frame vanishes with no gap in the sequence numbers or
    /// timestamps, so that the far end cannot fill it.
    pub slip: f64,
    /// Chance that the stream stops for `stall_for` before a frame, both
    /// ways, as on a busy host.
    pub stall: f64,
    pub stall_for: Duration,
    pub seed: u64,
}

/// How a call's setup and teardown travel alongside its audio.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Signalling {
    /// The wire's text messages share the socket with the audio.
    Wire { answering: bool },
    /// Signalling is elsewhere, and the far end may send from any port.
    Separate,
}

pub(crate) struct Session {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) peer: SocketAddr,
    pub(crate) signalling: Signalling,
    pub(crate) impairment: Impairment,
}

impl Session {
    /// Starts the stream. It ends when the call is hung up or `stop` fires,
    /// and fires `done` as it ends.
    pub(crate) fn start(
        self,
        stop: Option<oneshot::Receiver<()>>,
        done: Option<oneshot::Sender<()>>,
    ) -> Call {
        let (audio_out, outgoing) = mpsc::channel(8);
        let (incoming, audio_in) = mpsc::channel(64);
        let running = Running {
            rng: fastrand::Rng::with_seed(self.impairment.seed),
            session: self,
            held_back: None,
        };
        let task = tokio::spawn(async move {
            running.run(outgoing, incoming, stop).await;
            if let Some(done) = done {
                let _ = done.send(());
            }
        });
        Call {
            audio_out,
            audio_in,
            tasks: vec![task],
        }
    }
}

struct Running {
    session: Session,
    rng: fastrand::Rng,
    held_back: Option<Vec<u8>>,
}

impl Running {
    async fn run(
        mut self,
        mut outgoing: mpsc::Receiver<Vec<i16>>,
        incoming: mpsc::Sender<Vec<i16>>,
        stop: Option<oneshot::Receiver<()>>,
    ) {
        let ssrc = Ssrc(fastrand::u32(..));
        let mut sequence = SequenceNumber(fastrand::u16(..));
        let mut timestamp = fastrand::u32(..);
        let mut reorder = Reorder::new(REORDER_WINDOW);
        let mut frames = Vec::new();
        let mut buf = vec![0; 2048];
        let mut last_heard = Instant::now();
        let mut stop = stop;

        loop {
            tokio::select! {
                frame = outgoing.recv() => {
                    let Some(samples) = frame else {
                        self.hang_up().await;
                        break;
                    };
                    let impairment = self.session.impairment;
                    if impairment.stall > 0.0 && self.rng.f64() < impairment.stall {
                        tokio::time::sleep(impairment.stall_for).await;
                    }
                    if impairment.slip > 0.0 && self.rng.f64() < impairment.slip {
                        continue;
                    }
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
                () = stopped(&mut stop) => {
                    info!("far end hung up");
                    break;
                }
                received = self.session.socket.recv_from(&mut buf) => {
                    let (n, from) = match received {
                        Ok(received) => received,
                        Err(error) => {
                            warn!(%error, "receive failed");
                            break;
                        }
                    };
                    if !self.is_peer(from) {
                        if let Signalling::Wire { .. } = self.session.signalling
                            && buf[..n].starts_with(b"DIAL ")
                        {
                            let _ = self.session.socket.send_to(b"BUSY", from).await;
                        }
                        continue;
                    }
                    last_heard = Instant::now();
                    let data = &buf[..n];
                    if let Signalling::Wire { answering } = self.session.signalling {
                        if data == b"BYE" {
                            info!("far end hung up");
                            break;
                        }
                        if data.starts_with(b"DIAL ") && answering {
                            let _ = self.session.socket.send_to(b"ANSWER", self.session.peer).await;
                            continue;
                        }
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

    fn is_peer(&self, from: SocketAddr) -> bool {
        match self.session.signalling {
            Signalling::Wire { .. } => from == self.session.peer,
            Signalling::Separate => from.ip() == self.session.peer.ip(),
        }
    }

    async fn send_impaired(&mut self, packet: Vec<u8>) -> io::Result<()> {
        let impairment = self.session.impairment;
        if self.rng.f64() < impairment.loss {
            return Ok(());
        }
        if self.held_back.is_none() && self.rng.f64() < impairment.reorder {
            self.held_back = Some(packet);
            return Ok(());
        }
        self.session
            .socket
            .send_to(&packet, self.session.peer)
            .await?;
        if let Some(held) = self.held_back.take() {
            self.session
                .socket
                .send_to(&held, self.session.peer)
                .await?;
        }
        Ok(())
    }

    async fn hang_up(&self) {
        if let Signalling::Wire { .. } = self.session.signalling {
            for _ in 0..BYE_REPEATS {
                let _ = self.session.socket.send_to(b"BYE", self.session.peer).await;
            }
        }
    }
}

async fn stopped(stop: &mut Option<oneshot::Receiver<()>>) {
    match stop {
        Some(receiver) => {
            let _ = receiver.await;
        }
        None => std::future::pending().await,
    }
}
