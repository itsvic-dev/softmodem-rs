// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Finds the `+++` escape in data mode: a guard time with no data, three
//! escape characters each within the guard time of the last, and another
//! guard time with no data.

use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct EscapeDetector {
    last_data: Instant,
    held: Vec<u8>,
    last_held: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Timeout {
    /// The escape is complete. The held characters are not data.
    Escaped,
    /// What was held turned out to be data, to be sent now.
    Release(Vec<u8>),
}

impl EscapeDetector {
    #[must_use]
    pub fn new(now: Instant) -> Self {
        Self {
            last_data: now,
            held: Vec::new(),
            last_held: now,
        }
    }

    /// Takes one byte of data and appends what may be sent now to `out`.
    pub fn push(
        &mut self,
        byte: u8,
        escape: Option<u8>,
        guard: Duration,
        now: Instant,
        out: &mut Vec<u8>,
    ) {
        let is_escape = Some(byte) == escape;
        let continues = !self.held.is_empty()
            && self.held.len() < 3
            && now.duration_since(self.last_held) < guard;
        let starts = self.held.is_empty() && now.duration_since(self.last_data) >= guard;
        if is_escape && (continues || starts) {
            self.held.push(byte);
            self.last_held = now;
            return;
        }
        out.append(&mut self.held);
        out.push(byte);
        self.last_data = now;
    }

    /// When [`EscapeDetector::poll`] next has something to do.
    #[must_use]
    pub fn deadline(&self, guard: Duration) -> Option<Instant> {
        (!self.held.is_empty()).then(|| self.last_held + guard)
    }

    pub fn poll(&mut self, guard: Duration, now: Instant) -> Option<Timeout> {
        if self.held.is_empty() || now.duration_since(self.last_held) < guard {
            return None;
        }
        self.last_data = self.last_held;
        if self.held.len() == 3 {
            self.held.clear();
            Some(Timeout::Escaped)
        } else {
            Some(Timeout::Release(std::mem::take(&mut self.held)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUARD: Duration = Duration::from_secs(1);
    const ESCAPE: Option<u8> = Some(b'+');

    struct Line {
        detector: EscapeDetector,
        start: Instant,
        sent: Vec<u8>,
    }

    impl Line {
        fn new() -> Self {
            let start = Instant::now();
            Self {
                detector: EscapeDetector::new(start),
                start,
                sent: Vec::new(),
            }
        }

        fn at(&self, ms: u64) -> Instant {
            self.start + Duration::from_millis(ms)
        }

        fn type_at(&mut self, ms: u64, bytes: &[u8]) {
            for &byte in bytes {
                let now = self.at(ms);
                self.detector.push(byte, ESCAPE, GUARD, now, &mut self.sent);
            }
        }

        fn wait_until(&mut self, ms: u64) -> Option<Timeout> {
            let now = self.at(ms);
            let timeout = self.detector.poll(GUARD, now);
            if let Some(Timeout::Release(bytes)) = &timeout {
                self.sent.extend(bytes);
            }
            timeout
        }
    }

    #[test]
    fn escapes_after_guard_plus_plus_plus_guard() {
        let mut line = Line::new();
        line.type_at(1000, b"+");
        line.type_at(1100, b"+");
        line.type_at(1200, b"+");
        assert_eq!(line.wait_until(2100), None);
        assert_eq!(line.wait_until(2200), Some(Timeout::Escaped));
        assert!(line.sent.is_empty());
    }

    #[test]
    fn pluses_inside_data_are_data() {
        let mut line = Line::new();
        line.type_at(1500, b"a+++b");
        assert_eq!(line.wait_until(5000), None);
        assert_eq!(line.sent, b"a+++b");
    }

    #[test]
    fn data_right_after_the_pluses_cancels_the_escape() {
        let mut line = Line::new();
        line.type_at(1000, b"+++");
        line.type_at(1500, b"x");
        assert_eq!(line.wait_until(3000), None);
        assert_eq!(line.sent, b"+++x");
    }

    #[test]
    fn a_fourth_plus_cancels_the_escape() {
        let mut line = Line::new();
        line.type_at(1000, b"++++");
        assert_eq!(line.wait_until(3000), None);
        assert_eq!(line.sent, b"++++");
    }

    #[test]
    fn too_few_pluses_are_released_as_data() {
        let mut line = Line::new();
        line.type_at(1000, b"++");
        assert_eq!(
            line.wait_until(2000),
            Some(Timeout::Release(b"++".to_vec()))
        );
        assert_eq!(line.sent, b"++");
    }

    #[test]
    fn pluses_too_far_apart_are_data() {
        let mut line = Line::new();
        line.type_at(1000, b"+");
        assert_eq!(line.wait_until(2000), Some(Timeout::Release(b"+".to_vec())));
        line.type_at(2500, b"+");
        assert_eq!(line.wait_until(3500), Some(Timeout::Release(b"+".to_vec())));
        assert_eq!(line.sent, b"++");
    }

    #[test]
    fn no_escape_without_the_leading_guard() {
        let mut line = Line::new();
        line.type_at(500, b"+++");
        assert_eq!(line.wait_until(3000), None);
        assert_eq!(line.sent, b"+++");
    }

    #[test]
    fn a_disabled_escape_character_never_escapes() {
        let mut detector = EscapeDetector::new(Instant::now());
        let mut sent = Vec::new();
        let later = Instant::now() + GUARD * 2;
        for byte in *b"+++" {
            detector.push(byte, None, GUARD, later, &mut sent);
        }
        assert_eq!(sent, b"+++");
    }
}
