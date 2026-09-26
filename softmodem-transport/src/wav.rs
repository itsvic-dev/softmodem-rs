// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Records a call to two WAV files, one per direction, and reads them back.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use tokio::sync::mpsc;
use tracing::warn;

use crate::Call;

const SPEC: WavSpec = WavSpec {
    channels: 1,
    sample_rate: 8000,
    bits_per_sample: 16,
    sample_format: SampleFormat::Int,
};

/// Two open WAV files, waiting for a call to record.
pub struct Recorder {
    sent: WavWriter<BufWriter<File>>,
    received: WavWriter<BufWriter<File>>,
}

impl Recorder {
    /// Creates `<prefix>-tx.wav` for what is sent and `<prefix>-rx.wav` for
    /// what is received, after gap fill.
    ///
    /// # Errors
    ///
    /// Fails if either file cannot be created.
    pub fn create(prefix: &Path) -> hound::Result<Self> {
        Ok(Self {
            sent: WavWriter::create(with_suffix(prefix, "tx"), SPEC)?,
            received: WavWriter::create(with_suffix(prefix, "rx"), SPEC)?,
        })
    }

    /// Wraps `call` so that its audio in both directions is recorded.
    #[must_use]
    pub fn record(self, call: Call) -> Call {
        let Call {
            audio_out,
            audio_in,
            mut tasks,
        } = call;

        let (recorded_out, outgoing) = mpsc::channel(8);
        tasks.push(tokio::spawn(relay(outgoing, audio_out, self.sent)));
        let (incoming, recorded_in) = mpsc::channel(64);
        tasks.push(tokio::spawn(relay(audio_in, incoming, self.received)));

        Call {
            audio_out: recorded_out,
            audio_in: recorded_in,
            tasks,
        }
    }
}

/// The samples of `channel`, from 0, in a WAV file of 16-bit samples at
/// 8 kHz, as [`Recorder`] writes them.
///
/// # Errors
///
/// Fails if the file cannot be read, has another format, or has no such
/// channel.
pub fn read(path: &Path, channel: u16) -> hound::Result<Vec<i16>> {
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    if spec.sample_rate != SPEC.sample_rate
        || spec.bits_per_sample != SPEC.bits_per_sample
        || spec.sample_format != SPEC.sample_format
    {
        return Err(hound::Error::FormatError(
            "not 16-bit samples at 8 kHz, which sox -r 8000 -b 16 -e signed gives",
        ));
    }
    if channel >= spec.channels {
        return Err(hound::Error::FormatError("no such channel"));
    }
    reader
        .samples::<i16>()
        .skip(channel.into())
        .step_by(spec.channels.into())
        .collect()
}

fn with_suffix(prefix: &Path, direction: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(format!("-{direction}.wav"));
    name.into()
}

async fn relay(
    mut from: mpsc::Receiver<Vec<i16>>,
    to: mpsc::Sender<Vec<i16>>,
    mut writer: WavWriter<BufWriter<File>>,
) {
    while let Some(frame) = from.recv().await {
        for &sample in &frame {
            if let Err(error) = writer.write_sample(sample) {
                warn!(%error, "recording failed");
            }
        }
        if to.send(frame).await.is_err() {
            break;
        }
    }
    if let Err(error) = writer.finalize() {
        warn!(%error, "recording not finalised");
    }
}
