//! Records a call to two WAV files, one per direction.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

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

/// Wraps `call` so that everything sent goes to `<prefix>-tx.wav` and
/// everything received, after gap fill, to `<prefix>-rx.wav`.
///
/// # Errors
///
/// Fails if either file cannot be created.
pub fn record(call: Call, prefix: &Path) -> hound::Result<Call> {
    let tx = WavWriter::create(with_suffix(prefix, "tx"), SPEC)?;
    let rx = WavWriter::create(with_suffix(prefix, "rx"), SPEC)?;
    let Call {
        audio_out,
        audio_in,
    } = call;

    let (recorded_out, outgoing) = mpsc::channel(8);
    tokio::spawn(relay(outgoing, audio_out, tx));
    let (incoming, recorded_in) = mpsc::channel(64);
    tokio::spawn(relay(audio_in, incoming, rx));

    Ok(Call {
        audio_out: recorded_out,
        audio_in: recorded_in,
    })
}

fn with_suffix(prefix: &Path, direction: &str) -> std::path::PathBuf {
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
