// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Cancels the echo of what this end sends from what it hears, as a
//! hybrid beyond a gateway for voice over IP returns it: one linear path, a
//! few milliseconds long, some hundreds of milliseconds late, whose delay
//! jumps whenever the gateway's jitter buffer moves.

use crate::to_sample;

const RING: usize = 8192;
/// The latest echo, in samples, that the canceller looks for.
pub const MAX_DELAY: usize = 4800;
const TAPS: usize = 64;
// Taps before the peak of the echo.
const LEAD: usize = 16;
// Lags either side of the echo's peak that are watched for a jump.
const TRACK: usize = 640;
// A move of the peak by up to this many samples is left to the taps to follow.
const GUARD: usize = 8;
// The search takes one sample in this many, over a memory short enough for phase 3's silent gap.
const SEARCH_STRIDE: u64 = 4;
const SEARCH_SPAN: f64 = 800.0;
const SPAN: f64 = 4000.0;
const CHECK: usize = 160;
// How far above the median of its lags a peak must stand.
const CLEAR: f64 = 8.0;
// A normalised correlation above this means the far end is silent.
const ECHO_ALONE: f64 = 0.3;
// Checks that a jump must hold for, matching this share of the echo's power.
const CONFIRM: u32 = 2;
const JUMP: f64 = 0.5;
// Below this share of the estimate that the input agrees with, for this many checks, the echo is lost.
const AGREEMENT: f64 = 0.3;
const LOST: u32 = 100;
const STEP: f64 = 0.5;
const LEAST_STEP: f64 = 1e-6;
// Samples over which the step follows the powers of the input and the echo, and a quicker one for the far end starting to talk.
const STEADY: f64 = 500.0;
const ONSET: f64 = 64.0;
// The latest samples heard that the taps are solved from when the echo is found.
const SOLVED: u64 = 1600;
// Above this share of the input that the estimate explains, the far end is silent and the taps are solved again.
const ALONE: f64 = 0.5;
// A residual this many times its recent power holds adaptation back for this many samples.
const TALK: f64 = 16.0;
const HOLD: u32 = 2000;
// Keeps taps from growing where what was sent has no energy, as near 4 kHz.
const RIDGE: f64 = 1e-2;

#[derive(Debug)]
struct History {
    sent: Vec<f64>,
    // Only what can train the canceller: a pure tone or a short period gives no single delay.
    broadband: Vec<f64>,
    total: u64,
    // The first sample of the run of broadband ones that goes on to the newest.
    broadband_from: Option<u64>,
}

impl History {
    fn new() -> Self {
        Self {
            sent: vec![0.0; 2 * RING],
            broadband: vec![0.0; 2 * RING],
            total: 0,
            broadband_from: None,
        }
    }

    #[expect(clippy::cast_possible_truncation, reason = "reduced modulo RING")]
    fn at(t: u64) -> usize {
        RING - 1 - (t % RING as u64) as usize
    }

    fn push(&mut self, x: f64, broadband: bool) {
        let p = Self::at(self.total);
        let probe = if broadband { x } else { 0.0 };
        self.sent[p] = x;
        self.sent[p + RING] = x;
        self.broadband[p] = probe;
        self.broadband[p + RING] = probe;
        self.broadband_from = if broadband {
            self.broadband_from.or(Some(self.total))
        } else {
            None
        };
        self.total += 1;
    }

    // Sample `t` and the `len - 1` before it, newest first.
    fn back_from(&self, t: u64, len: usize, broadband: bool) -> Option<&[f64]> {
        let kept =
            t < self.total && self.total - t + len as u64 <= RING as u64 && t + 1 >= len as u64;
        let from = Self::at(t);
        let buffer = if broadband {
            &self.broadband
        } else {
            &self.sent
        };
        kept.then(|| &buffer[from..from + len])
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// Solves `matrix * x = vector` by Gaussian elimination, after adding RIDGE of the mean diagonal to the diagonal.
fn ridge_solve(mut matrix: Vec<Vec<f64>>, mut vector: Vec<f64>) -> Option<Vec<f64>> {
    let size = vector.len();
    let diagonal: f64 = matrix.iter().enumerate().map(|(i, row)| row[i]).sum();
    let ridge = RIDGE * diagonal / f64::from(u32::try_from(size).ok()?);
    for (i, row) in matrix.iter_mut().enumerate() {
        row[i] += ridge;
    }
    for column in 0..size {
        let pivot = matrix[column][column];
        if pivot.abs() < 1e-9 {
            return None;
        }
        let (above, below) = matrix.split_at_mut(column + 1);
        let pivot_row = &above[column];
        for (offset, row) in below.iter_mut().enumerate() {
            let factor = row[column] / pivot;
            for (value, &from) in row[column..].iter_mut().zip(&pivot_row[column..]) {
                *value -= factor * from;
            }
            vector[column + 1 + offset] -= factor * vector[column];
        }
    }
    let mut solution = vec![0.0; size];
    for row in (0..size).rev() {
        let known: f64 = (row + 1..size).map(|k| matrix[row][k] * solution[k]).sum();
        solution[row] = (vector[row] - known) / matrix[row][row];
    }
    Some(solution)
}

/// The echo canceller of one call.
#[derive(Debug)]
pub struct Canceller {
    history: History,
    heard: u64,
    // What was heard lately, for the taps to be solved from.
    recent: Vec<f64>,
    // Per lag, the mean of what is left after cancelling times what was sent that long before.
    correlation: Vec<f64>,
    heard_power: f64,
    sent_power: f64,
    updates: u64,
    // The lag of the first tap, and of the peak the taps were placed at.
    delay: Option<usize>,
    peak: usize,
    taps: Vec<f64>,
    echo_power: f64,
    recent_echo: f64,
    recent_input: f64,
    recent_left: f64,
    onset: f64,
    // The mean of the input times the estimate, which falls to nothing once the echo moves away.
    agreement: f64,
    // The run of broadband samples that the correlation comes from, and the longest lag it covers.
    run: Option<u64>,
    reach: usize,
    moved_to: Option<(usize, u32)>,
    disagreed: u32,
    since_check: usize,
    // The first sample of the echo heard alone lately, while the far end is silent.
    alone_since: Option<u64>,
    held: u32,
}

impl Default for Canceller {
    fn default() -> Self {
        Self::new()
    }
}

impl Canceller {
    #[must_use]
    pub fn new() -> Self {
        Self {
            history: History::new(),
            heard: 0,
            recent: vec![0.0; RING],
            correlation: vec![0.0; MAX_DELAY + 1],
            heard_power: 0.0,
            sent_power: 0.0,
            updates: 0,
            delay: None,
            peak: 0,
            taps: vec![0.0; TAPS],
            echo_power: 0.0,
            recent_echo: 0.0,
            recent_input: 0.0,
            recent_left: 0.0,
            onset: 0.0,
            agreement: 0.0,
            run: None,
            reach: 0,
            moved_to: None,
            disagreed: 0,
            since_check: 0,
            alone_since: None,
            held: 0,
        }
    }

    /// The delay of the echo's peak in samples, once it is found and cancelled.
    #[must_use]
    pub fn delay(&self) -> Option<usize> {
        self.delay.map(|_| self.peak)
    }

    /// Samples this end sent, in order. Only `broadband` ones, such as TRN,
    /// MP and data, train the canceller.
    pub fn sent(&mut self, samples: &[i16], broadband: bool) {
        for &sample in samples {
            self.history.push(f64::from(sample), broadband);
        }
    }

    /// `input` less the echo. With `search`, as while the pump trains, it
    /// looks for an echo at every delay, and takes one that makes up most of
    /// the input.
    pub fn cancel(&mut self, input: &[i16], search: bool) -> Vec<i16> {
        input
            .iter()
            .map(|&sample| {
                let heard = f64::from(sample);
                let at = self.heard;
                self.heard += 1;
                self.recent[History::at(at)] = heard;
                let left = self.adapt(at, heard);
                self.correlate(at, heard, left, search);
                self.since_check += 1;
                if self.since_check >= CHECK {
                    self.since_check = 0;
                    self.check(search);
                }
                to_sample(left)
            })
            .collect()
    }

    // `heard` less the estimate of its echo, after one step of NLMS on the taps.
    fn adapt(&mut self, at: u64, heard: f64) -> f64 {
        let Some(from) = self.delay.and_then(|delay| at.checked_sub(delay as u64)) else {
            return heard;
        };
        let Some(sent) = self.history.back_from(from, TAPS, false) else {
            return heard;
        };
        let estimate = dot(&self.taps, sent);
        let left = heard - estimate;
        // The far end starting to talk: the residual leaps, before any mean of powers can follow it.
        if left * left > TALK * self.recent_left.max(1.0) {
            self.held = HOLD;
        }
        self.echo_power += (estimate * estimate - self.echo_power) / SPAN;
        self.agreement += (heard * estimate - self.agreement) / SPAN;
        self.recent_echo += (estimate * estimate - self.recent_echo) / STEADY;
        self.recent_input += (heard * heard - self.recent_input) / STEADY;
        self.recent_left += (left * left - self.recent_left) / STEADY;
        self.onset += (heard * heard - self.onset) / ONSET;
        if self.held > 0 {
            self.held -= 1;
            return left;
        }
        if let Some(probe) = self.history.back_from(from, TAPS, true) {
            let energy = dot(probe, probe);
            if energy > 0.0 {
                // Slow while the far end talks, whose signal is noise to the taps.
                let input = self.recent_input.max(self.onset).max(1.0);
                let share = (self.recent_echo / input).min(1.0);
                let gain = (STEP * share.powi(3)).max(LEAST_STEP) * left / energy;
                for (tap, p) in self.taps.iter_mut().zip(probe) {
                    *tap += gain * p;
                }
            }
        }
        left
    }

    fn window(&self) -> (usize, usize) {
        match self.delay {
            Some(_) => (
                self.peak.saturating_sub(TRACK),
                (self.peak + TRACK).min(MAX_DELAY),
            ),
            None => (0, MAX_DELAY),
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "counts of a few thousand")]
    fn correlate(&mut self, at: u64, heard: f64, left: f64, search: bool) {
        if self.delay.is_none() && !(search && at.is_multiple_of(SEARCH_STRIDE)) {
            return;
        }
        let Some(from) = self.history.broadband_from.filter(|&from| from <= at) else {
            return;
        };
        if self.run != Some(from) {
            self.run = Some(from);
            self.correlation.fill(0.0);
            self.updates = 0;
        }
        let (low, high) = self.window();
        let unsent =
            usize::try_from((at + 1).saturating_sub(self.history.total)).unwrap_or(usize::MAX);
        let low = low.max(unsent);
        let high = high.min(usize::try_from(at - from).unwrap_or(usize::MAX));
        self.reach = high;
        if low > high {
            return;
        }
        let Some(probe) = at
            .checked_sub(low as u64)
            .and_then(|newest| self.history.back_from(newest, high - low + 1, true))
        else {
            return;
        };
        self.updates += 1;
        let alpha = if self.delay.is_some() {
            (1.0 / self.updates as f64).max(1.0 / SPAN)
        } else {
            SEARCH_STRIDE as f64 / SEARCH_SPAN
        };
        for (c, p) in self.correlation[low..=high].iter_mut().zip(probe) {
            *c += alpha * (left * p - *c);
        }
        let middle = probe[probe.len() / 2];
        self.heard_power += alpha * (heard * heard - self.heard_power);
        self.sent_power += alpha * (middle * middle - self.sent_power);
    }

    fn median(values: impl Iterator<Item = f64>) -> f64 {
        let mut magnitudes: Vec<f64> = values.map(f64::abs).collect();
        let middle = magnitudes.len() / 2;
        *magnitudes.select_nth_unstable_by(middle, f64::total_cmp).1
    }

    // The strongest lag of the search, if it stands clear and makes up most of the input.
    #[expect(clippy::cast_precision_loss, reason = "counts of a few thousand")]
    fn found(&self) -> Option<usize> {
        let lags = &self.correlation[..=self.reach];
        let (peak, strength) =
            lags.iter()
                .map(|c| c.abs())
                .enumerate()
                .fold(
                    (0, 0.0),
                    |best, (lag, c)| if c > best.1 { (lag, c) } else { best },
                );
        let settled = (self.updates * SEARCH_STRIDE) as f64 >= SEARCH_SPAN;
        let clear = strength > CLEAR * Self::median(lags.iter().copied());
        let alone = strength >= ECHO_ALONE * (self.heard_power * self.sent_power).sqrt();
        (settled && clear && alone).then_some(peak)
    }

    // A delay whose estimate what is left after cancelling matches, far from the current one.
    #[expect(clippy::cast_precision_loss, reason = "counts of a few thousand")]
    fn jumped_to(&self) -> Option<usize> {
        let delay = self.delay?;
        let (low, high) = self.window();
        let last = (high.min(self.reach) + 1)
            .checked_sub(TAPS)
            .filter(|&last| last >= low)?;
        let matched: Vec<(usize, f64)> = (low..=last)
            .map(|at| (at, dot(&self.taps, &self.correlation[at..at + TAPS])))
            .collect();
        let median = Self::median(matched.iter().map(|&(_, m)| m));
        let (at, strength) = matched
            .into_iter()
            .filter(|&(at, _)| at.abs_diff(delay) > GUARD)
            .fold(
                (delay, f64::MIN),
                |best, m| if m.1 > best.1 { m } else { best },
            );
        let settled = self.updates as f64 >= SPAN / 4.0;
        (settled && strength > CLEAR * median && strength > JUMP * self.echo_power).then_some(at)
    }

    fn check(&mut self, search: bool) {
        if self.delay.is_none() {
            if let Some(peak) = self.found().filter(|_| search) {
                self.acquire(peak);
            }
            return;
        }
        let jumped = self.jumped_to();
        let held = match (self.moved_to, jumped) {
            (Some((to, checks)), Some(at)) if to.abs_diff(at) <= GUARD => checks + 1,
            (_, Some(_)) => 1,
            _ => 0,
        };
        self.moved_to = jumped.map(|at| (at, held));
        if let Some(at) = jumped
            && held >= CONFIRM
        {
            self.move_to(at);
            return;
        }
        let agrees = self.agreement >= AGREEMENT * self.echo_power;
        self.disagreed = if agrees { 0 } else { self.disagreed + 1 };
        if self.disagreed >= LOST {
            self.lose();
            return;
        }
        // The frame just heard may hold the far end starting to talk, which the check sees only now.
        let settled = self.heard.saturating_sub(CHECK as u64);
        let alone = self.recent_echo > ALONE * self.recent_input.max(self.onset);
        match (search && alone, self.alone_since, self.delay) {
            (true, Some(since), Some(delay)) => {
                if let Some(taps) = self.solve(delay, since, settled) {
                    self.taps = taps;
                }
            }
            (true, None, _) => self.alone_since = Some(settled),
            _ => self.alone_since = None,
        }
    }

    fn acquire(&mut self, peak: usize) {
        let delay = peak.saturating_sub(LEAD);
        let Some(first) = self
            .history
            .broadband_from
            .map(|from| from + (delay + TAPS) as u64)
        else {
            return;
        };
        let Some(taps) = self.solve(delay, first, self.heard) else {
            return;
        };
        self.taps = taps;
        self.delay = Some(delay);
        self.peak = peak;
        self.restart_tracking();
        self.alone_since = Some(first);
    }

    // The least squares taps at `delay`, from what was heard from `first` to `last` against what was sent.
    fn solve(&self, delay: usize, first: u64, last: u64) -> Option<Vec<f64>> {
        if delay + TAPS > MAX_DELAY + 1 {
            return None;
        }
        let first_sent = self.history.broadband_from? + (delay + TAPS) as u64;
        let first = first.max(first_sent).max(last.saturating_sub(SOLVED));
        if last < first + 4 * TAPS as u64 {
            return None;
        }
        let mut matrix = vec![vec![0.0; TAPS]; TAPS];
        let mut vector = vec![0.0; TAPS];
        for at in first..last {
            let sent = self.history.back_from(at - delay as u64, TAPS, true)?;
            let heard = self.recent[History::at(at)];
            for (i, row) in matrix.iter_mut().enumerate() {
                for (value, s) in row.iter_mut().zip(sent) {
                    *value += sent[i] * s;
                }
                vector[i] += heard * sent[i];
            }
        }
        ridge_solve(matrix, vector)
    }

    // The taps keep their shape at the echo's new delay.
    fn move_to(&mut self, delay: usize) {
        let Some(old) = self.delay.filter(|_| delay + TAPS <= MAX_DELAY + 1) else {
            self.lose();
            return;
        };
        self.peak = delay + (self.peak - old);
        self.delay = Some(delay);
        self.restart_tracking();
    }

    fn restart_tracking(&mut self) {
        self.correlation.fill(0.0);
        self.updates = 0;
        self.moved_to = None;
        self.disagreed = 0;
        self.agreement = self.echo_power;
    }

    fn lose(&mut self) {
        self.delay = None;
        self.taps.fill(0.0);
        self.restart_tracking();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = 8000;

    // A band-limited stand-in for a V.34 signal: noise through a bandpass filter.
    #[expect(clippy::cast_precision_loss, reason = "a few seconds of samples")]
    fn signal(samples: usize, seed: u64, rms: f64) -> Vec<f64> {
        let mut state = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        state ^= state >> 31;
        let mut noise = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            f64::from(u32::try_from(state >> 32).unwrap()) / f64::from(u32::MAX) - 0.5
        };
        let white: Vec<f64> = (0..samples + 4).map(|_| noise()).collect();
        let band: Vec<f64> = white.windows(5).map(|w| w[4] - w[2] + 0.5 * w[0]).collect();
        let power = band.iter().map(|x| x * x).sum::<f64>() / band.len() as f64;
        band.iter().map(|x| x * rms / power.sqrt()).collect()
    }

    const PATH: [f64; 6] = [0.02, -0.05, 0.12, -0.09, 0.04, -0.01];
    // The far signal is 16 dB above the echo, as on the real call.
    const FAR_RMS: f64 = 3000.0 * 0.17 * 6.3;

    fn echo_of(sent: &[f64], delay: impl Fn(usize) -> usize) -> Vec<f64> {
        (0..sent.len())
            .map(|n| {
                PATH.iter()
                    .enumerate()
                    .map(|(k, g)| n.checked_sub(delay(n) + k).map_or(0.0, |t| g * sent[t]))
                    .sum()
            })
            .collect()
    }

    // Silent for the first second, as the far end is while this end trains in phase 3.
    fn far(samples: usize, seed: u64) -> Vec<f64> {
        signal(samples, seed, FAR_RMS)
            .into_iter()
            .enumerate()
            .map(|(k, x)| if k < RATE { 0.0 } else { x })
            .collect()
    }

    #[expect(clippy::cast_precision_loss, reason = "a few seconds of samples")]
    fn power(x: &[f64]) -> f64 {
        x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64
    }

    // Runs a call in 20 ms frames, and gives the residual echo, heard less the far signal.
    fn run(
        sent: &[f64],
        echo: &[f64],
        far: &[f64],
        search: impl Fn(usize) -> bool,
    ) -> (Canceller, Vec<f64>) {
        let mut canceller = Canceller::new();
        let mut residual = Vec::new();
        for start in (0..sent.len()).step_by(160) {
            let end = (start + 160).min(sent.len());
            let out: Vec<i16> = sent[start..end].iter().map(|&x| to_sample(x)).collect();
            canceller.sent(&out, true);
            let heard: Vec<i16> = (start..end).map(|n| to_sample(echo[n] + far[n])).collect();
            let cleaned = canceller.cancel(&heard, search(start));
            residual.extend(
                (start..end)
                    .zip(cleaned)
                    .map(|(n, c)| f64::from(c) - far[n].round()),
            );
        }
        (canceller, residual)
    }

    fn left_db(residual: &[f64], echo: &[f64]) -> f64 {
        10.0 * (power(residual) / power(echo)).log10()
    }

    #[test]
    fn cancels_an_echo_found_while_the_far_end_is_silent() {
        let n = 4 * RATE;
        let sent = signal(n, 1, 3000.0);
        let echo = echo_of(&sent, |_| 1197);
        let (canceller, residual) = run(&sent, &echo, &far(n, 2), |at| at < 2 * RATE);
        assert_eq!(
            canceller.delay().map(|d| d.abs_diff(1199) <= 1),
            Some(true),
            "the echo would be missed"
        );
        let left = left_db(&residual[2 * RATE..], &echo[2 * RATE..]);
        assert!(
            left < -18.0,
            "only {:.1} dB of the echo would be cancelled while the far end talks",
            -left
        );
    }

    #[test]
    fn follows_the_echo_when_its_delay_jumps() {
        let n = 8 * RATE;
        let sent = signal(n, 3, 3000.0);
        let delay = |k: usize| match k {
            _ if k < 3 * RATE => 1197,
            _ if k < 5 * RATE => 1357,
            _ => 1005,
        };
        let echo = echo_of(&sent, delay);
        let (_, residual) = run(&sent, &echo, &far(n, 4), |at| at < 2 * RATE);
        for jump in [3 * RATE, 5 * RATE] {
            let settled = jump + RATE;
            let left = left_db(
                &residual[settled..jump + 2 * RATE],
                &echo[settled..jump + 2 * RATE],
            );
            assert!(
                left < -18.0,
                "a second after the jump at {} s, only {:.1} dB of the echo would be cancelled",
                jump / RATE,
                -left
            );
        }
    }

    #[test]
    fn leaves_a_line_without_echo_as_it_is() {
        let n = 3 * RATE;
        let sent = signal(n, 5, 3000.0);
        let far = signal(n, 6, 3000.0);
        let (canceller, residual) = run(&sent, &vec![0.0; n], &far, |_| true);
        assert_eq!(
            canceller.delay(),
            None,
            "a canceller would chase an echo that is not there"
        );
        assert!(residual.iter().all(|&r| r.abs() <= 1.0));
    }
}
