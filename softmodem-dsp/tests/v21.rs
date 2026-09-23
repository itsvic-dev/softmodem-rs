use softmodem_dsp::fsk::{Channel, Demodulator, Modulator, V21_ANSWER, V21_ORIGINATE};
use softmodem_dsp::uart::{Decoder, frame};

const FRAME: usize = 160;
const LEVEL: i16 = 8000;

fn payload() -> Vec<u8> {
    (0..=255)
        .chain(*b"The quick brown fox jumps over the lazy dog.")
        .collect()
}

fn modulate(channel: Channel, bytes: &[u8]) -> Vec<i16> {
    let mut modulator = Modulator::new(channel, LEVEL);
    let mut out = vec![0; 800];
    modulator.render(&mut out);
    modulator.push_bits(bytes.iter().flat_map(|&b| frame(b)));
    while modulator.pending() > 0 {
        let mut chunk = [0; FRAME];
        modulator.render(&mut chunk);
        out.extend_from_slice(&chunk);
    }
    let mut tail = [0; 800];
    modulator.render(&mut tail);
    out.extend_from_slice(&tail);
    out
}

struct Received {
    bytes: Vec<u8>,
    carrier_drops: usize,
}

fn demodulate(channel: Channel, samples: &[i16]) -> Received {
    let mut demodulator = Demodulator::new(channel);
    let mut decoder = Decoder::new();
    let mut bits = Vec::new();
    let mut bytes = Vec::new();
    let mut carrier_drops = 0;
    let mut had_carrier = false;
    for chunk in samples.chunks(FRAME) {
        demodulator.process(chunk, &mut bits);
        bytes.extend(bits.drain(..).filter_map(|b| decoder.push(b)));
        if had_carrier && !demodulator.carrier() {
            carrier_drops += 1;
            decoder.reset();
        }
        had_carrier = demodulator.carrier();
    }
    Received {
        bytes,
        carrier_drops,
    }
}

fn link(channel: Channel, impair: impl FnOnce(Vec<i16>) -> Vec<i16>) -> Received {
    demodulate(channel, &impair(modulate(channel, &payload())))
}

fn random_bytes(count: usize) -> Vec<u8> {
    let mut noise = Noise(0x2545_F491_4F6C_DD1D);
    (0..count)
        .map(|_| {
            noise.uniform();
            noise.0.to_le_bytes()[7]
        })
        .collect()
}

fn common_subsequence(a: &[u8], b: &[u8]) -> usize {
    let mut row = vec![0; b.len() + 1];
    for &x in a {
        let mut diagonal = 0;
        for (j, &y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if x == y {
                diagonal + 1
            } else {
                above.max(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
}

struct Noise(u64);

impl Noise {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        f64::from(u32::try_from(self.0 >> 32).unwrap()) / f64::from(u32::MAX) - 0.5
    }

    fn gaussian(&mut self) -> f64 {
        (0..12).map(|_| self.uniform()).sum()
    }
}

#[expect(clippy::cast_possible_truncation, reason = "clamped first")]
fn clamp(x: f64) -> i16 {
    x.round().clamp(-32768.0, 32767.0) as i16
}

fn add_noise(samples: Vec<i16>, snr_db: f64) -> Vec<i16> {
    let sigma = f64::from(LEVEL) / 2f64.sqrt() / 10f64.powf(snr_db / 20.0);
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    samples
        .into_iter()
        .map(|s| clamp(f64::from(s) + sigma * noise.gaussian()))
        .collect()
}

fn scale(samples: Vec<i16>, gain: f64) -> Vec<i16> {
    samples
        .into_iter()
        .map(|s| clamp(f64::from(s) * gain))
        .collect()
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "sample positions are small and positive"
)]
fn resample(samples: &[i16], ratio: f64) -> Vec<i16> {
    let count = ((samples.len() - 1) as f64 / ratio) as usize;
    (0..count)
        .map(|n| {
            let position = n as f64 * ratio;
            let i = position as usize;
            let t = position - i as f64;
            let a = f64::from(samples[i]);
            let b = f64::from(samples[i + 1]);
            clamp(a + (b - a) * t)
        })
        .collect()
}

#[test]
fn clean_line_round_trips_both_channels() {
    for channel in [V21_ORIGINATE, V21_ANSWER] {
        let received = link(channel, |s| s);
        assert_eq!(received.bytes, payload());
        assert_eq!(received.carrier_drops, 0);
    }
}

#[test]
fn survives_noise_at_12_db() {
    let received = link(V21_ORIGINATE, |s| add_noise(s, 12.0));
    assert_eq!(received.bytes, payload());
}

#[test]
fn ignores_gain() {
    for gain in [0.05, 0.5, 2.0] {
        let received = link(V21_ORIGINATE, |s| scale(s, gain));
        assert_eq!(received.bytes, payload(), "corrupted at line gain {gain}");
    }
}

#[test]
fn tracks_a_sender_clock_that_is_off_by_half_a_percent() {
    for ratio in [0.995, 1.005] {
        let received = link(V21_ORIGINATE, |s| resample(&s, ratio));
        assert_eq!(
            received.bytes,
            payload(),
            "lost bit sync at clock ratio {ratio}"
        );
    }
}

#[test]
fn rejects_the_other_channel_in_full_duplex() {
    let other = modulate(V21_ANSWER, &payload().into_iter().rev().collect::<Vec<_>>());
    let received = link(V21_ORIGINATE, |s| {
        s.iter()
            .zip(other.iter().chain(std::iter::repeat(&0)))
            .map(|(&a, &b)| clamp(f64::from(a) + f64::from(b)))
            .collect()
    });
    assert_eq!(received.bytes, payload());
}

#[test]
fn a_lost_packet_costs_a_few_characters_and_not_the_carrier() {
    let losses = 8;
    let expected = random_bytes(300);
    let mut samples = modulate(V21_ORIGINATE, &expected);
    for n in 1..=losses {
        let start = n * samples.len() / (losses + 1) / FRAME * FRAME;
        samples[start..start + FRAME].fill(0);
    }
    let received = demodulate(V21_ORIGINATE, &samples);
    let kept = common_subsequence(&received.bytes, &expected);
    assert!(
        kept >= expected.len() - 5 * losses,
        "packet loss spread past the characters it hit: {kept} of {} survived",
        expected.len()
    );
    assert_eq!(received.carrier_drops, 0);
}

#[test]
fn silence_and_idle_mark_yield_nothing() {
    assert!(demodulate(V21_ORIGINATE, &[0; 8000]).bytes.is_empty());

    let mut modulator = Modulator::new(V21_ORIGINATE, LEVEL);
    let mut idle = vec![0; 8000];
    modulator.render(&mut idle);
    assert!(demodulate(V21_ORIGINATE, &idle).bytes.is_empty());
}

#[test]
fn carrier_comes_and_goes() {
    let mut modulator = Modulator::new(V21_ORIGINATE, LEVEL);
    let mut tone = vec![0; 1600];
    modulator.render(&mut tone);

    let mut demodulator = Demodulator::new(V21_ORIGINATE);
    let mut bits = Vec::new();
    demodulator.process(&tone, &mut bits);
    assert!(demodulator.carrier());
    demodulator.process(&[0; 800], &mut bits);
    assert!(!demodulator.carrier());
}
