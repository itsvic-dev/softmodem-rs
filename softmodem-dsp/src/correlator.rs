use std::collections::VecDeque;
use std::f64::consts::TAU;

use crate::SAMPLE_RATE;

/// Power of one frequency over a sliding window.
#[derive(Debug)]
pub(crate) struct Correlator {
    step: f64,
    phase: f64,
    history: VecDeque<(f64, f64)>,
    sum: (f64, f64),
    window: f64,
}

impl Correlator {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the window is a small positive whole number"
    )]
    pub(crate) fn new(hz: f64, window: f64) -> Self {
        Self {
            step: hz / SAMPLE_RATE,
            phase: 0.0,
            history: VecDeque::from(vec![(0.0, 0.0); window as usize]),
            sum: (0.0, 0.0),
            window,
        }
    }

    pub(crate) fn push(&mut self, x: f64) -> f64 {
        let angle = TAU * self.phase;
        let product = (x * angle.cos(), -x * angle.sin());
        self.phase = (self.phase + self.step).fract();

        let (old_re, old_im) = self.history.pop_front().unwrap_or_default();
        self.history.push_back(product);
        self.sum.0 += product.0 - old_re;
        self.sum.1 += product.1 - old_im;

        let re = self.sum.0 / self.window;
        let im = self.sum.1 / self.window;
        re * re + im * im
    }
}
