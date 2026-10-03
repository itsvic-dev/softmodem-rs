// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! How bright each light on the panel is, from the modem's status and the
//! time.

use std::time::{Duration, Instant};

use crate::Status;

/// The lights from left to right, as printed under them.
pub const NAMES: [&str; 12] = [
    "HS", "AA", "CD", "OH", "RD", "SD", "TR", "MR", "RS", "CS", "SYN", "ARQ",
];

const HIGH_SPEED: u32 = 9600;
const FLASH: Duration = Duration::from_millis(50);
const BETWEEN_FLASHES: Duration = Duration::from_millis(30);
const RETRANSMISSION_DARK: Duration = Duration::from_millis(150);
const BLINK: Duration = Duration::from_millis(250);
const RISE: f32 = 0.015;
const FALL: f32 = 0.06;
const SETTLED: f32 = 0.002;

/// The panel's lights, each fading towards what the modem shows.
#[derive(Debug)]
pub struct Lights {
    epoch: Instant,
    last: Instant,
    levels: [f32; 12],
    received: Flash,
    sent: Flash,
    retransmissions: u64,
    arq_dark_until: Instant,
}

impl Lights {
    #[must_use]
    pub fn new(status: &Status, now: Instant) -> Self {
        let mut lights = Self {
            epoch: now,
            last: now,
            levels: [0.0; 12],
            received: Flash::new(status.received),
            sent: Flash::new(status.sent),
            retransmissions: retransmissions(status),
            arq_dark_until: now,
        };
        lights.levels = lights.targets(status, now);
        lights
    }

    /// Brightness from 0 to 1, in the order of [`NAMES`].
    #[must_use]
    pub fn levels(&self) -> [f32; 12] {
        self.levels
    }

    /// Moves the lights on to `now`, and says whether any changed.
    pub fn update(&mut self, status: &Status, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f32();
        self.last = now;
        let targets = self.targets(status, now);
        let mut changed = false;
        for (level, target) in self.levels.iter_mut().zip(targets) {
            let tau = if target > *level { RISE } else { FALL };
            let mut next = *level + (target - *level) * (1.0 - (-elapsed / tau).exp());
            if (target - next).abs() <= SETTLED {
                next = target;
            }
            changed |= (next - *level).abs() > f32::EPSILON;
            *level = next;
        }
        changed
    }

    fn targets(&mut self, status: &Status, now: Instant) -> [f32; 12] {
        let blink = (now.saturating_duration_since(self.epoch).as_millis() / BLINK.as_millis())
            .is_multiple_of(2);
        let retransmissions = retransmissions(status);
        if retransmissions != self.retransmissions {
            self.retransmissions = retransmissions;
            self.arq_dark_until = now + RETRANSMISSION_DARK;
        }
        let connection = status.connection.as_ref();
        [
            connection.is_some_and(|c| c.receive_rate >= HIGH_SPEED),
            if status.ringing {
                blink
            } else {
                status.auto_answer
            },
            connection.is_some(),
            status.off_hook,
            self.received.lit(status.received, now),
            self.sent.lit(status.sent, now),
            status.terminal_ready,
            !status.training() || blink,
            true,
            true,
            false,
            connection.is_some_and(|c| c.error_control) && now >= self.arq_dark_until,
        ]
        .map(|on| if on { 1.0 } else { 0.0 })
    }
}

fn retransmissions(status: &Status) -> u64 {
    status.connection.map_or(0, |c| c.retransmissions)
}

// One flash for each run of data, with a dark gap after it so a stream flickers.
#[derive(Debug)]
struct Flash {
    seen: u64,
    pending: bool,
    on_until: Option<Instant>,
    dark_until: Option<Instant>,
}

impl Flash {
    fn new(count: u64) -> Self {
        Self {
            seen: count,
            pending: false,
            on_until: None,
            dark_until: None,
        }
    }

    fn lit(&mut self, count: u64, now: Instant) -> bool {
        if count != self.seen {
            self.seen = count;
            self.pending = true;
        }
        if let Some(until) = self.on_until {
            if now < until {
                return true;
            }
            self.on_until = None;
            self.dark_until = Some(until + BETWEEN_FLASHES);
        }
        if self.dark_until.is_some_and(|until| now < until) {
            return false;
        }
        if std::mem::take(&mut self.pending) {
            self.on_until = Some(now + FLASH);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Connection;

    const LONG: Duration = Duration::from_secs(1);

    fn connected(start: Instant) -> Status {
        Status {
            off_hook: true,
            connection: Some(Connection {
                since: start,
                receive_rate: 31_200,
                transmit_rate: 33_600,
                error_control: true,
                compression: true,
                retransmissions: 0,
            }),
            ..Status::default()
        }
    }

    fn lit(lights: &Lights, name: &str) -> bool {
        let index = NAMES.iter().position(|n| *n == name).unwrap();
        lights.levels()[index] > 0.5
    }

    fn lit_names(lights: &Lights) -> Vec<&'static str> {
        NAMES.into_iter().filter(|name| lit(lights, name)).collect()
    }

    #[test]
    fn a_fast_connection_with_lapm_lights_hs_cd_oh_and_arq() {
        let start = Instant::now();
        let mut lights = Lights::new(&Status::default(), start);
        lights.update(&connected(start), start + LONG);
        assert_eq!(
            lit_names(&lights),
            ["HS", "CD", "OH", "MR", "RS", "CS", "ARQ"]
        );
    }

    #[test]
    fn rd_flashes_when_data_arrives_and_then_goes_dark() {
        let start = Instant::now();
        let mut status = connected(start);
        let mut lights = Lights::new(&status, start);
        status.received += 5;
        lights.update(&status, start + Duration::from_millis(1));
        lights.update(&status, start + Duration::from_millis(40));
        assert!(lit(&lights, "RD"));
        assert!(!lit(&lights, "SD"));
        lights.update(&status, start + LONG);
        assert!(!lit(&lights, "RD"));
    }

    #[test]
    fn steady_data_makes_rd_flicker() {
        let start = Instant::now();
        let mut status = connected(start);
        let mut lights = Lights::new(&status, start);
        let mut seen = Vec::new();
        for ms in (0..400).step_by(5) {
            status.received += 1;
            lights.update(&status, start + Duration::from_millis(ms));
            seen.push(lit(&lights, "RD"));
        }
        assert!(seen.contains(&true) && seen.contains(&false));
    }

    #[test]
    fn arq_goes_dark_for_a_retransmission() {
        let start = Instant::now();
        let mut status = connected(start);
        let mut lights = Lights::new(&status, start);
        if let Some(connection) = &mut status.connection {
            connection.retransmissions = 1;
        }
        lights.update(&status, start + Duration::from_millis(1));
        lights.update(&status, start + Duration::from_millis(100));
        assert!(!lit(&lights, "ARQ"));
        lights.update(&status, start + LONG);
        assert!(lit(&lights, "ARQ"));
    }

    #[test]
    fn mr_blinks_while_the_modems_train() {
        let start = Instant::now();
        let status = Status {
            off_hook: true,
            stage: Some("V.34 phase 2".into()),
            ..Status::default()
        };
        let mut lights = Lights::new(&status, start);
        let mut seen = Vec::new();
        for ms in (0..1000).step_by(20) {
            lights.update(&status, start + Duration::from_millis(ms));
            seen.push(lit(&lights, "MR"));
        }
        assert!(seen.contains(&true) && seen.contains(&false));
    }

    #[test]
    fn the_lights_stop_changing_once_they_settle() {
        let start = Instant::now();
        let status = connected(start);
        let mut lights = Lights::new(&Status::default(), start);
        assert!(lights.update(&status, start + Duration::from_millis(10)));
        lights.update(&status, start + LONG);
        assert!(!lights.update(&status, start + LONG + Duration::from_millis(16)));
    }
}
