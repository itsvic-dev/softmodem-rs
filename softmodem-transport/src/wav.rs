// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Records a call to two WAV files, one per direction.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};
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
