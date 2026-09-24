//! Joins slmodemd, from Aon's D-Modem, to a softmodem over its UDP wire, to
//! test against the Smart Link DSP. slmodemd runs this on ATD as
//! `slmodem-bridge NUMBER FD`, where FD is its audio socket, and it dials
//! `SOFTMODEM_PEER`. `SOFTMODEM_NOISE_AFTER` and `SOFTMODEM_NOISE_RMS` add
//! noise both ways from that many seconds after the answer.
//!
//! The socket carries 16-bit samples at 9600 Hz, and slmodemd answers each
//! block it reads with a block of the same length. So the far softmodem's
//! frames set the clock: each one goes to slmodemd, and slmodemd's answer
//! goes back as the next frame.

mod noise;
mod resample;

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::os::fd::FromRawFd;

use anyhow::{Context, bail};
use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{FRAME_SAMPLES, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::info;

use crate::noise::Noise;
use crate::resample::Resampler;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let mut args = std::env::args().skip(1);
    let (Some(number), Some(fd)) = (args.next(), args.next()) else {
        bail!("usage: slmodem-bridge NUMBER FD");
    };
    let fd: i32 = fd.parse().context("reading the socket descriptor")?;
    let peer: SocketAddr = std::env::var("SOFTMODEM_PEER")
        .context("reading $SOFTMODEM_PEER")?
        .parse()
        .context("parsing $SOFTMODEM_PEER")?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run(&number, fd, peer))
}

async fn run(number: &str, fd: i32, peer: SocketAddr) -> anyhow::Result<()> {
    // SAFETY: slmodemd hands this descriptor to the program it runs, to own.
    let socket = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    socket.set_nonblocking(true)?;
    let (mut from_slmodemd, mut to_slmodemd) = UnixStream::from_std(socket)?.into_split();

    let mut wire = Wire::bind("127.0.0.1:0".parse()?, Some(peer), Impairment::default()).await?;
    info!(number, %peer, "dialling");
    let mut call = wire.dial(number).await.context("dialling")?;
    info!("answered");

    let mut up = Resampler::new(6, 5);
    let mut down = Resampler::new(5, 6);
    let mut heard: VecDeque<i16> = VecDeque::new();
    let mut odd_byte: Option<u8> = None;
    let mut buf = [0u8; 4096];
    let mut high = Vec::new();
    let mut low = Vec::new();
    let mut noise = Noise::from_env();
    loop {
        tokio::select! {
            frame = call.audio_in.recv() => {
                let Some(mut frame) = frame else {
                    info!("the far softmodem hung up");
                    break;
                };
                if let Some(noise) = &mut noise {
                    if noise.tick() {
                        info!("noise on the line from now");
                    }
                    if noise.to_slmodemd {
                        noise.add(&mut frame);
                    }
                }
                high.clear();
                up.process(&frame, &mut high);
                let bytes: Vec<u8> = high.iter().flat_map(|s| s.to_le_bytes()).collect();
                if to_slmodemd.write_all(&bytes).await.is_err() {
                    info!("slmodemd hung up");
                    break;
                }
                let mut reply: Vec<i16> = if heard.len() >= FRAME_SAMPLES {
                    heard.drain(..FRAME_SAMPLES).collect()
                } else {
                    vec![0; FRAME_SAMPLES]
                };
                if let Some(noise) = &mut noise
                    && noise.to_softmodem
                {
                    noise.add(&mut reply);
                }
                if call.audio_out.send(reply).await.is_err() {
                    break;
                }
            }
            read = from_slmodemd.read(&mut buf) => {
                let n = read.unwrap_or(0);
                if n == 0 {
                    info!("slmodemd hung up");
                    break;
                }
                let mut bytes: Vec<u8> = odd_byte.take().into_iter().collect();
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() % 2 == 1 {
                    odd_byte = bytes.pop();
                }
                let samples: Vec<i16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&pair| i16::from_le_bytes(pair))
                    .collect();
                low.clear();
                down.process(&samples, &mut low);
                heard.extend(&low);
            }
        }
    }
    call.hang_up().await;
    Ok(())
}
