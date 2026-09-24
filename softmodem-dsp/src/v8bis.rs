//! V.8 bis: the tone signals that start a transaction.

use std::collections::VecDeque;

use crate::correlator::Correlator;
use crate::sine_peak;
use crate::tone::Tone;

// § 7.1.1, tables 1 and 2.
const INITIATING_HZ: [f64; 2] = [1375.0, 2002.0];
const RESPONDING_HZ: [f64; 2] = [1529.0, 2225.0];
// § 7.1.2: segment 1 is 400 ms, or 285 ms for MRe and CRe, and segment 2 is 100 ms.
const SEGMENT_1_SAMPLES: usize = 3200;
const SHORT_SEGMENT_1_SAMPLES: usize = 2280;
const SEGMENT_2_SAMPLES: usize = 800;

const WINDOW: usize = 160;
const PURITY: f64 = 0.7;
const DETECT_DBM0: f64 = -43.0;
// Most of a short segment 1, and half of segment 2.
const PAIR_SAMPLES: usize = 1600;
const SINGLE_SAMPLES: usize = 400;
const SEGMENT_2_WAIT_SAMPLES: usize = 1200;

/// A V.8 bis signal, named by its segment 2 tone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    MRe,
    MRd,
    CRe,
    CRd,
    ESi,
    ESr,
}

/// The dual tone of segment 1: the one a transaction starts with, or the
/// one that answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pair {
    Initiating,
    Responding,
}

impl Pair {
    fn hz(self) -> [f64; 2] {
        match self {
            Self::Initiating => INITIATING_HZ,
            Self::Responding => RESPONDING_HZ,
        }
    }

    fn signals(self) -> &'static [Signal] {
        match self {
            Self::Initiating => &[
                Signal::MRe,
                Signal::MRd,
                Signal::CRe,
                Signal::CRd,
                Signal::ESi,
            ],
            Self::Responding => &[Signal::MRd, Signal::CRd, Signal::ESr],
        }
    }
}

impl Signal {
    fn segment_2_hz(self) -> f64 {
        match self {
            Self::MRe => 650.0,
            Self::MRd => 1150.0,
            Self::CRe => 400.0,
            Self::CRd => 1900.0,
            Self::ESi => 980.0,
            Self::ESr => 1650.0,
        }
    }

    // § 7.1.2 allows the shorter segment 1 so a modem without V.8 bis ignores it.
    fn segment_1_samples(self) -> usize {
        match self {
            Self::MRe | Self::CRe => SHORT_SEGMENT_1_SAMPLES,
            _ => SEGMENT_1_SAMPLES,
        }
    }
}

/// Sends one signal, then silence.
#[derive(Debug)]
pub struct SignalSender {
    pair: [Tone; 2],
    single: Tone,
    segment_1: usize,
    sent: usize,
}

impl SignalSender {
    /// A sender of `signal` after the dual tone of `pair`, at a total power
    /// of `level_dbm0`.
    #[must_use]
    pub fn new(signal: Signal, pair: Pair, level_dbm0: f64) -> Self {
        let [low, high] = pair.hz();
        let half = level_dbm0 - 10.0 * 2f64.log10();
        Self {
            pair: [Tone::new(low, half), Tone::new(high, half)],
            single: Tone::new(signal.segment_2_hz(), level_dbm0),
            segment_1: signal.segment_1_samples(),
            sent: 0,
        }
    }

    #[must_use]
    pub fn done(&self) -> bool {
        self.sent >= self.segment_1 + SEGMENT_2_SAMPLES
    }

    pub fn render(&mut self, out: &mut [i16]) {
        let mut high = vec![0; out.len()];
        let mut single = vec![0; out.len()];
        self.pair[0].render(out);
        self.pair[1].render(&mut high);
        self.single.render(&mut single);
        for (i, sample) in out.iter_mut().enumerate() {
            let at = self.sent + i;
            *sample = if at < self.segment_1 {
                sample.saturating_add(high[i])
            } else if at < self.segment_1 + SEGMENT_2_SAMPLES {
                single[i]
            } else {
                0
            };
        }
        self.sent += out.len();
    }
}

#[derive(Debug)]
struct Power {
    squares: VecDeque<f64>,
    sum: f64,
}

impl Power {
    fn new() -> Self {
        Self {
            squares: VecDeque::from(vec![0.0; WINDOW]),
            sum: 0.0,
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    fn push(&mut self, x: f64) -> f64 {
        let square = x * x;
        self.sum += square - self.squares.pop_front().unwrap_or_default();
        self.squares.push_back(square);
        self.sum.max(0.0) / WINDOW as f64
    }
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    Pair {
        lasted: usize,
    },
    Single {
        waited: usize,
        lasted: usize,
        at: usize,
    },
}

/// Hears the signals that start with one dual tone and tells them apart by
/// their segment 2 tone.
#[derive(Debug)]
pub struct SignalDetector {
    signals: &'static [Signal],
    pair: [Correlator; 2],
    singles: Vec<Correlator>,
    power: Power,
    threshold: f64,
    stage: Stage,
}

impl SignalDetector {
    #[expect(clippy::cast_precision_loss, reason = "the window is 160")]
    #[must_use]
    pub fn new(pair: Pair) -> Self {
        let [low, high] = pair.hz();
        let signals = pair.signals();
        Self {
            signals,
            pair: [
                Correlator::new(low, WINDOW as f64),
                Correlator::new(high, WINDOW as f64),
            ],
            singles: signals
                .iter()
                .map(|s| Correlator::new(s.segment_2_hz(), WINDOW as f64))
                .collect(),
            power: Power::new(),
            threshold: sine_peak(DETECT_DBM0 - 10.0 * 2f64.log10()),
            stage: Stage::Pair { lasted: 0 },
        }
    }

    /// The signal that ended its segment 2 within `input`, if any.
    pub fn process(&mut self, input: &[i16]) -> Option<Signal> {
        let mut heard = None;
        for &sample in input {
            let x = f64::from(sample);
            let mean_square = self.power.push(x);
            let pair = self.pair.each_mut().map(|c| c.push(x));
            let singles: Vec<f64> = self.singles.iter_mut().map(|c| c.push(x)).collect();
            let loud = |power: f64| 2.0 * power.sqrt() >= self.threshold;
            let pure = |power: f64| 2.0 * power >= PURITY * mean_square;
            let paired = pair.iter().all(|&p| loud(p)) && pure(pair[0] + pair[1]);
            let single = singles.iter().position(|&p| loud(p) && pure(p));

            self.stage = match self.stage {
                Stage::Pair { lasted } if paired => Stage::Pair { lasted: lasted + 1 },
                Stage::Pair { lasted } if lasted >= PAIR_SAMPLES => Stage::Single {
                    waited: 0,
                    lasted: 0,
                    at: usize::MAX,
                },
                Stage::Pair { .. } => Stage::Pair { lasted: 0 },
                Stage::Single { waited, .. } if waited >= SEGMENT_2_WAIT_SAMPLES => {
                    Stage::Pair { lasted: 0 }
                }
                Stage::Single { waited, lasted, at } => match single {
                    Some(i) if i == at && lasted + 1 >= SINGLE_SAMPLES => {
                        heard = Some(self.signals[i]);
                        Stage::Pair { lasted: 0 }
                    }
                    Some(i) if i == at => Stage::Single {
                        waited: waited + 1,
                        lasted: lasted + 1,
                        at,
                    },
                    Some(i) => Stage::Single {
                        waited: waited + 1,
                        lasted: 1,
                        at: i,
                    },
                    None => Stage::Single {
                        waited: waited + 1,
                        lasted: 0,
                        at: usize::MAX,
                    },
                },
            };
        }
        heard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansam::{AnswerTone, AnswerToneKind};
    use crate::fsk::{Modulator, V21_ANSWER, V21_MAX_LEVEL_DBM0};

    const LEVEL: f64 = V21_MAX_LEVEL_DBM0;
    // § 7.1.4: 12 to 15 dB below the level of continuous signals.
    const LOW_LEVEL: f64 = V21_MAX_LEVEL_DBM0 - 13.0;

    fn signal(signal: Signal, pair: Pair, level: f64) -> Vec<i16> {
        let mut sender = SignalSender::new(signal, pair, level);
        let mut out = vec![0; 800];
        while !sender.done() {
            let mut chunk = [0; 160];
            sender.render(&mut chunk);
            out.extend(chunk);
        }
        out.extend([0; 800]);
        out
    }

    fn heard(pair: Pair, samples: &[i16]) -> Vec<Signal> {
        let mut detector = SignalDetector::new(pair);
        samples
            .chunks(160)
            .filter_map(|c| detector.process(c))
            .collect()
    }

    #[test]
    fn hears_each_signal_after_its_own_dual_tone() {
        for pair in [Pair::Initiating, Pair::Responding] {
            for &s in pair.signals() {
                assert_eq!(heard(pair, &signal(s, pair, LEVEL)), [s], "{s:?}");
            }
        }
    }

    #[test]
    fn hears_cre_at_its_low_level_with_the_short_segment_1() {
        let cre = signal(Signal::CRe, Pair::Initiating, LOW_LEVEL);
        assert_eq!(heard(Pair::Initiating, &cre), [Signal::CRe]);
    }

    #[test]
    fn does_not_hear_the_other_pair() {
        let cre = signal(Signal::CRe, Pair::Initiating, LEVEL);
        assert!(heard(Pair::Responding, &cre).is_empty());
        let crd = signal(Signal::CRd, Pair::Responding, LEVEL);
        assert!(heard(Pair::Initiating, &crd).is_empty());
    }

    #[test]
    fn esr_is_heard_when_v21_mark_goes_on_after_it() {
        let mut samples = signal(Signal::ESr, Pair::Responding, LEVEL);
        samples.truncate(samples.len() - 800);
        let mut mark = vec![0; 4000];
        Modulator::new(V21_ANSWER, LEVEL).render(&mut mark);
        samples.extend(mark);
        assert_eq!(heard(Pair::Responding, &samples), [Signal::ESr]);
    }

    #[test]
    fn ignores_ansam_v21_and_silence() {
        let mut ansam = vec![0; 16_000];
        AnswerTone::new(AnswerToneKind::Ansam, true, LEVEL).render(&mut ansam);
        let mut modulator = Modulator::new(V21_ANSWER, LEVEL);
        modulator.push_bits((0..2000).map(|n| n % 3 == 0));
        let mut v21 = vec![0; 16_000];
        modulator.render(&mut v21);
        for pair in [Pair::Initiating, Pair::Responding] {
            assert!(heard(pair, &ansam).is_empty());
            assert!(heard(pair, &v21).is_empty());
            assert!(heard(pair, &[0; 8000]).is_empty());
        }
    }
}
