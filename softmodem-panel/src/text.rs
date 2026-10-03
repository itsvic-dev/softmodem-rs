// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The words under the panel, about the call the modem is on.

use std::time::Instant;

use crate::{Connection, Status};

/// Short items to show side by side, from the most important.
#[must_use]
pub fn summary(status: &Status, now: Instant) -> Vec<String> {
    if let Some(connection) = &status.connection {
        return connected(connection, status, now);
    }
    let idle = if status.ringing {
        "ringing"
    } else if !status.terminal_ready {
        "no computer"
    } else if status.off_hook {
        "off hook"
    } else {
        "on hook"
    };
    vec![status.stage.clone().unwrap_or_else(|| idle.into())]
}

fn connected(connection: &Connection, status: &Status, now: Instant) -> Vec<String> {
    let rates = if connection.receive_rate == connection.transmit_rate {
        format!("{} bit/s", connection.receive_rate)
    } else {
        format!(
            "↓{} ↑{} bit/s",
            connection.receive_rate, connection.transmit_rate
        )
    };
    let protocol = match (connection.error_control, connection.compression) {
        (true, true) => "LAPM V.42bis",
        (true, false) => "LAPM",
        (false, _) => "no error control",
    };
    let seconds = now.saturating_duration_since(connection.since).as_secs();
    let mut items = vec![
        rates,
        protocol.into(),
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        ),
        format!("↓{} ↑{}", bytes(status.received), bytes(status.sent)),
    ];
    if connection.retransmissions > 0 {
        items.push(format!("{} resent", connection.retransmissions));
    }
    items
}

#[expect(clippy::cast_precision_loss, reason = "shown to one decimal place")]
fn bytes(count: u64) -> String {
    match count {
        0..1_000 => format!("{count} B"),
        1_000..1_000_000 => format!("{:.1} kB", count as f64 / 1e3),
        _ => format!("{:.1} MB", count as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn an_idle_modem_says_whether_it_is_on_hook() {
        let now = Instant::now();
        let mut status = Status {
            terminal_ready: true,
            ..Status::default()
        };
        assert_eq!(summary(&status, now), ["on hook"]);
        status.terminal_ready = false;
        assert_eq!(summary(&status, now), ["no computer"]);
    }

    #[test]
    fn a_training_modem_shows_the_stage() {
        let status = Status {
            off_hook: true,
            stage: Some("V.34 phase 2".into()),
            ..Status::default()
        };
        assert_eq!(summary(&status, Instant::now()), ["V.34 phase 2"]);
    }

    #[test]
    fn a_connected_modem_shows_rates_protocol_time_and_data() {
        let since = Instant::now();
        let status = Status {
            off_hook: true,
            stage: Some("analogue: sending data, hearing data".into()),
            connection: Some(Connection {
                since,
                receive_rate: 52_000,
                transmit_rate: 31_200,
                error_control: true,
                compression: true,
                retransmissions: 3,
            }),
            received: 12_345,
            sent: 678,
            ..Status::default()
        };
        assert_eq!(
            summary(&status, since + Duration::from_secs(3723)),
            [
                "↓52000 ↑31200 bit/s",
                "LAPM V.42bis",
                "01:02:03",
                "↓12.3 kB ↑678 B",
                "3 resent",
            ]
        );
    }
}
