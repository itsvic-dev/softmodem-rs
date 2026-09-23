//! Runs a V.21 modem over a call: bytes in, bytes out.

use std::io;
use std::time::Duration;

use softmodem_dsp::fsk::{
    Channel, Demodulator, Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0, V21_ORIGINATE,
};
use softmodem_dsp::uart::{Decoder, frame};
use softmodem_transport::{Call, FRAME_SAMPLES};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Instant, MissedTickBehavior, interval};
use tracing::info;

const FRAME_INTERVAL: Duration = Duration::from_millis(20);
const LOW_WATER_BITS: usize = 20;
const TAIL_FRAMES: usize = 10;
// Hayes S10 default: 1.4 s without carrier before hanging up.
const CARRIER_LOSS_HANG_UP: Duration = Duration::from_millis(1400);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Originate,
    Answer,
}

impl Role {
    fn channels(self) -> (Channel, Channel) {
        match self {
            Self::Originate => (V21_ORIGINATE, V21_ANSWER),
            Self::Answer => (V21_ANSWER, V21_ORIGINATE),
        }
    }
}

/// Sends `input` and writes what the far end sends to `output`, until the
/// far end hangs up, carrier is lost, or `input` ends and has been sent.
///
/// # Errors
///
/// Fails if `input` or `output` fails.
pub async fn run<R, W>(mut call: Call, role: Role, input: R, output: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let result = pump(&mut call, role, input, output).await;
    call.hang_up().await;
    result
}

async fn pump<R, W>(call: &mut Call, role: Role, mut input: R, mut output: W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (transmit, receive) = role.channels();
    let mut modulator = Modulator::new(transmit, V21_MAX_LEVEL_DBM0);
    let mut demodulator = Demodulator::new(receive);
    let mut decoder = Decoder::new();
    let Call {
        audio_out,
        audio_in,
        ..
    } = call;

    let mut ticker = interval(FRAME_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut connected = false;
    let mut input_done = false;
    let mut tail = 0;
    let mut carrier_lost_at: Option<Instant> = None;
    let mut read_buf = [0; 4];
    let mut bits = Vec::new();

    loop {
        let want_input = connected && !input_done && modulator.pending() < LOW_WATER_BITS;
        tokio::select! {
            _ = ticker.tick() => {
                let mut samples = vec![0; FRAME_SAMPLES];
                modulator.render(&mut samples);
                if audio_out.send(samples).await.is_err() {
                    return Ok(());
                }
                if input_done && modulator.pending() == 0 {
                    tail += 1;
                    if tail >= TAIL_FRAMES {
                        info!("input sent, hanging up");
                        return Ok(());
                    }
                }
                if carrier_lost_at.is_some_and(|lost| lost.elapsed() >= CARRIER_LOSS_HANG_UP) {
                    info!("NO CARRIER");
                    return Ok(());
                }
            }
            read = input.read(&mut read_buf), if want_input => {
                match read? {
                    0 => input_done = true,
                    n => modulator.push_bits(read_buf[..n].iter().flat_map(|&b| frame(b))),
                }
            }
            samples = audio_in.recv() => {
                let Some(samples) = samples else {
                    info!("far end hung up");
                    return Ok(());
                };
                demodulator.process(&samples, &mut bits);
                let bytes: Vec<u8> = bits.drain(..).filter_map(|b| decoder.push(b)).collect();
                if !bytes.is_empty() {
                    output.write_all(&bytes).await?;
                    output.flush().await?;
                }
                match (connected, demodulator.carrier()) {
                    (false, true) => {
                        connected = true;
                        info!("CONNECT 300");
                    }
                    (true, false) if carrier_lost_at.is_none() => {
                        carrier_lost_at = Some(Instant::now());
                        decoder.reset();
                    }
                    (true, true) => carrier_lost_at = None,
                    _ => {}
                }
            }
        }
    }
}
