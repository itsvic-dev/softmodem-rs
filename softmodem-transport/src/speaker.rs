// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Plays a call on the host's sound output, as a modem's speaker does.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
    Device, ErrorKind, FromSample, OutputCallbackInfo, SampleFormat, SizedSample, Stream,
    StreamConfig,
};
use tokio::sync::mpsc;
use tracing::warn;

use crate::Call;

const SAMPLE_RATE: f32 = 8000.0;
/// Held back after a queue runs dry, to ride out late frames.
const PREFILL: usize = 480;
const LIMIT: usize = 1600;

/// The default output device, playing both directions of a call mixed.
pub struct Speaker {
    shared: Arc<Shared>,
    _stream: Stream,
}

#[derive(Default)]
struct Shared {
    gain: AtomicU32,
    sent: Mutex<Queue>,
    received: Mutex<Queue>,
}

impl Shared {
    fn next(&self) -> f32 {
        let sum = i32::from(lock(&self.sent).pop()) + i32::from(lock(&self.received).pop());
        #[expect(clippy::cast_precision_loss, reason = "the sum of two i16 fits in f32")]
        let sample = sum as f32 / 32768.0;
        sample
    }
}

#[derive(Default)]
struct Queue {
    samples: VecDeque<i16>,
    playing: bool,
}

impl Queue {
    fn push(&mut self, frame: &[i16]) {
        self.samples.extend(frame);
        let excess = self.samples.len().saturating_sub(LIMIT);
        self.samples.drain(..excess);
    }

    fn pop(&mut self) -> i16 {
        self.playing |= self.samples.len() >= PREFILL;
        if !self.playing {
            return 0;
        }
        self.samples.pop_front().unwrap_or_else(|| {
            self.playing = false;
            0
        })
    }
}

fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Speaker {
    /// Opens the default output device, silent until [`Speaker::set_gain`].
    ///
    /// # Errors
    ///
    /// Fails if there is no output device or it cannot play.
    pub fn open() -> Result<Self, cpal::Error> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or(ErrorKind::DeviceNotAvailable)?;
        let supported = device.default_output_config()?;
        let format = supported.sample_format();
        let config = supported.config();
        let shared = Arc::new(Shared::default());
        let stream = match format {
            SampleFormat::I16 => build::<i16>(&device, &config, &shared),
            SampleFormat::I32 => build::<i32>(&device, &config, &shared),
            SampleFormat::U16 => build::<u16>(&device, &config, &shared),
            SampleFormat::F32 => build::<f32>(&device, &config, &shared),
            SampleFormat::F64 => build::<f64>(&device, &config, &shared),
            _ => Err(ErrorKind::InvalidInput.into()),
        }?;
        stream.play()?;
        Ok(Self {
            shared,
            _stream: stream,
        })
    }

    /// Sets the volume, from 0 for silence to 1 for full scale.
    pub fn set_gain(&self, gain: f32) {
        self.shared.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Wraps `call` so that its audio in both directions is played.
    #[must_use]
    pub fn play(&self, call: Call) -> Call {
        let Call {
            audio_out,
            audio_in,
            mut tasks,
        } = call;

        let (played_out, outgoing) = mpsc::channel(8);
        tasks.push(tokio::spawn(relay(
            outgoing,
            audio_out,
            self.shared.clone(),
            |shared| &shared.sent,
        )));
        let (incoming, played_in) = mpsc::channel(64);
        tasks.push(tokio::spawn(relay(
            audio_in,
            incoming,
            self.shared.clone(),
            |shared| &shared.received,
        )));

        Call {
            audio_out: played_out,
            audio_in: played_in,
            tasks,
        }
    }
}

fn build<T>(
    device: &Device,
    config: &StreamConfig,
    shared: &Arc<Shared>,
) -> Result<Stream, cpal::Error>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample rates are far below 2^24"
    )]
    let step = SAMPLE_RATE / config.sample_rate as f32;
    let shared = shared.clone();
    let mut phase = 0.0;
    let mut previous = 0.0;
    let mut next = 0.0;
    device.build_output_stream(
        *config,
        move |data: &mut [T], _: &OutputCallbackInfo| {
            let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed));
            for frame in data.chunks_mut(channels) {
                phase += step;
                while phase >= 1.0 {
                    phase -= 1.0;
                    previous = next;
                    next = shared.next();
                }
                let sample = (previous + (next - previous) * phase) * gain;
                frame.fill(T::from_sample(sample.clamp(-1.0, 1.0)));
            }
        },
        |error| warn!(%error, "speaker failed"),
        None,
    )
}

async fn relay(
    mut from: mpsc::Receiver<Vec<i16>>,
    to: mpsc::Sender<Vec<i16>>,
    shared: Arc<Shared>,
    queue: fn(&Shared) -> &Mutex<Queue>,
) {
    while let Some(frame) = from.recv().await {
        lock(queue(&shared)).push(&frame);
        if to.send(frame).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_waits_for_prefill_then_plays_in_order() {
        let mut queue = Queue::default();
        queue.push(&[1; PREFILL - 1]);
        assert_eq!(queue.pop(), 0);
        queue.push(&[2]);
        assert_eq!(queue.pop(), 1);
        for _ in 1..PREFILL {
            queue.pop();
        }
        assert_eq!(queue.pop(), 0);
        queue.push(&[3]);
        assert_eq!(queue.pop(), 0);
    }

    #[test]
    fn queue_drops_the_oldest_past_the_limit() {
        let mut queue = Queue::default();
        queue.push(&[1; LIMIT]);
        queue.push(&[2; 10]);
        assert_eq!(queue.samples.len(), LIMIT);
        assert_eq!(queue.pop(), 1);
        assert_eq!(queue.samples.back(), Some(&2));
    }
}
