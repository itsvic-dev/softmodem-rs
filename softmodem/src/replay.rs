// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! Plays a recorded call again into a fresh line, and logs what the line
//! does, on the call's own clock.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use softmodem_dsp::pump::Role;
use softmodem_terminal::settings::Settings;
use softmodem_transport::FRAME_SAMPLES;
use tracing::{debug, info, info_span, warn};

use crate::journal::{Event, Header, Recorded};
use crate::line::{Line, Received};
use crate::modem::{Hex, header};

static CLOCK: AtomicU64 = AtomicU64::new(0);

/// How far into the call the replay is, for a log to show.
#[must_use]
pub fn clock() -> Duration {
    Duration::from_nanos(CLOCK.load(Ordering::Relaxed))
}

fn set_clock(at: Duration) {
    CLOCK.store(
        u64::try_from(at.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// What a replay gives the line.
#[derive(Debug, Clone)]
pub enum Script {
    /// The journal of a call recorded with `--dump`.
    Journal(String),
    /// A line in `role` under `settings`, which sends a frame and then
    /// hears one, for a recording with no journal.
    Assumed { role: Role, settings: Box<Settings> },
}

/// A recorded call.
#[derive(Debug, Clone, Copy)]
pub struct Recording<'a> {
    /// What this end heard.
    pub rx: &'a [i16],
    /// What this end sent, to check the replay against.
    pub tx: Option<&'a [i16]>,
}

/// What a replay found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replayed {
    /// When the line reported CONNECT, if it did.
    pub connected_at: Option<Duration>,
    /// When the replay first sent other samples than the recording has.
    pub differs_at: Option<Duration>,
    /// The frames compared with the recording.
    pub compared: usize,
}

/// Plays `recording` into a fresh line as `script` says, and with `far`,
/// into a line of the far end's role too, which hears what this end sent.
///
/// # Errors
///
/// Fails if the journal cannot be read.
pub fn replay(script: Script, recording: Recording<'_>, far: bool) -> Result<Replayed, String> {
    let events = match script {
        Script::Journal(text) => {
            let recorded: Recorded = text.parse()?;
            let ours = env!("CARGO_PKG_VERSION");
            if recorded.version != ours {
                warn!(
                    recorded = recorded.version,
                    ours, "the journal is from another version"
                );
            }
            Timeline {
                header: recorded.header,
                events: recorded.events,
            }
        }
        Script::Assumed { role, settings } => assumed(role, &settings, recording.rx.len()),
    };
    Ok(Replay::new(events.header, recording, far).run(&events.events))
}

struct Timeline {
    header: Header,
    events: Vec<(Duration, Event)>,
}

// One frame sent, then one heard, every 20 ms.
fn assumed(role: Role, settings: &Settings, samples: usize) -> Timeline {
    let frame = Duration::from_millis(20);
    let events = (0..samples.div_ceil(FRAME_SAMPLES))
        .flat_map(|n| {
            let at = frame * u32::try_from(n).unwrap_or(u32::MAX);
            let heard = (samples - n * FRAME_SAMPLES).min(FRAME_SAMPLES);
            [(at, Event::Tx), (at, Event::Rx(heard))]
        })
        .collect();
    Timeline {
        header: header(settings, Some(role)),
        events,
    }
}

fn other(role: Role) -> Role {
    match role {
        Role::Originate => Role::Answer,
        Role::Answer => Role::Originate,
    }
}

// A line, and what it last showed.
struct Watched {
    line: Line<()>,
    span: tracing::Span,
    carrier: bool,
    released: bool,
    cleared: bool,
}

impl Watched {
    fn new(header: Header, name: &'static str) -> Self {
        let span = info_span!("", end = name);
        let line = span.in_scope(|| Line::new((), header, None, None));
        Self {
            line,
            span,
            carrier: false,
            released: false,
            cleared: false,
        }
    }

    fn receive(&mut self, samples: &[i16], now: Instant) -> Received {
        let _entered = self.span.clone().entered();
        let received = self.line.receive(samples, now);
        if received.connected {
            info!("{}", self.line.connect_log(&received));
        }
        if !received.bytes.is_empty() {
            debug!(
                bytes = %Hex(&received.bytes),
                text = ?String::from_utf8_lossy(&received.bytes),
                "to the computer"
            );
        }
        self.watch();
        received
    }

    fn transmit(&mut self, now: Instant) -> Vec<i16> {
        let _entered = self.span.clone().entered();
        let samples = self.line.transmit(now);
        self.watch();
        samples
    }

    fn watch(&mut self) {
        let carrier = self.line.carrier();
        if carrier != self.carrier {
            info!("carrier {}", if carrier { "on" } else { "off" });
            self.carrier = carrier;
        }
        if self.line.released() && !self.released {
            info!("error control ended the call");
            self.released = true;
        }
        if self.line.cleared() && !self.cleared {
            info!("cleared down");
            self.cleared = true;
        }
    }
}

struct Replay<'a> {
    near: Watched,
    far: Option<Watched>,
    far_header: Header,
    recording: Recording<'a>,
    start: Instant,
    heard: usize,
    sent: usize,
    replayed: Replayed,
}

impl<'a> Replay<'a> {
    fn new(header: Header, recording: Recording<'a>, far: bool) -> Self {
        let far_header = Header {
            role: header.role.map(other),
            ..header
        };
        Self {
            near: Watched::new(header, "near"),
            far: far.then(|| Watched::new(far_header, "far")),
            far_header,
            recording,
            start: Instant::now(),
            heard: 0,
            sent: 0,
            replayed: Replayed {
                connected_at: None,
                differs_at: None,
                compared: 0,
            },
        }
    }

    fn run(mut self, events: &[(Duration, Event)]) -> Replayed {
        let mut events = events.iter().peekable();
        while let Some((at, event)) = events.next() {
            set_clock(*at);
            let now = self.start + *at;
            let dropped = matches!(events.peek(), Some((_, Event::Dropped)));
            if !self.play(*at, now, event, dropped) {
                break;
            }
        }
        let _entered = self.near.span.clone().entered();
        match self.replayed.differs_at {
            None if self.recording.tx.is_some() => {
                info!(frames = self.replayed.compared, "sent all as recorded");
            }
            _ => {}
        }
        self.replayed
    }

    // Gives the line `event`, and says whether the recording goes on.
    fn play(&mut self, at: Duration, now: Instant, event: &Event, dropped: bool) -> bool {
        match event {
            Event::Rx(count) => {
                let Some(samples) = self.recording.rx.get(self.heard..self.heard + count) else {
                    warn!(samples = self.heard, "the received recording ends here");
                    return false;
                };
                self.heard += count;
                let received = self.near.receive(samples, now);
                if received.connected {
                    self.replayed.connected_at.get_or_insert(at);
                }
            }
            Event::Tx => {
                let made = self.near.transmit(now);
                if !dropped {
                    self.check(at, &made);
                    self.hear_far(now, &made);
                }
            }
            Event::Dropped => {}
            Event::Send(bytes) => {
                let _entered = self.near.span.clone().entered();
                if !bytes.is_empty() {
                    debug!(
                        bytes = %Hex(bytes),
                        text = ?String::from_utf8_lossy(bytes),
                        "from the computer"
                    );
                }
                self.near.line.send(bytes, now);
            }
            Event::Retrain => {
                let _entered = self.near.span.clone().entered();
                info!("retrain from ATO1");
                self.near.line.retrain(now);
            }
            Event::ClearDown => {
                let _entered = self.near.span.clone().entered();
                info!("cleardown from a hang-up");
                self.near.line.clear_down(now);
            }
            Event::Start(role) => {
                let _entered = self.near.span.clone().entered();
                info!(?role, "handshake from ATO");
                self.near.line.start(*role, now);
                if let Some(far) = &mut self.far {
                    far.line.start(other(*role), now);
                }
            }
        }
        true
    }

    fn check(&mut self, at: Duration, made: &[i16]) {
        let Some(tx) = self.recording.tx else {
            return;
        };
        let Some(recorded) = tx.get(self.sent..self.sent + made.len()) else {
            return;
        };
        self.replayed.compared += 1;
        if self.replayed.differs_at.is_some() {
            return;
        }
        if let Some(n) = recorded.iter().zip(made).position(|(a, b)| a != b) {
            let _entered = self.near.span.clone().entered();
            warn!(
                sample = self.sent + n,
                recorded = recorded[n],
                replayed = made[n],
                "sends other samples than the recording from here"
            );
            self.replayed.differs_at = Some(at);
        }
    }

    // The far end hears what this end sent: the recording where there is one.
    fn hear_far(&mut self, now: Instant, made: &[i16]) {
        let from = self.sent;
        self.sent += made.len();
        let Some(far) = &mut self.far else {
            return;
        };
        let heard = self
            .recording
            .tx
            .and_then(|tx| tx.get(from..self.sent))
            .unwrap_or(made);
        if self.far_header.role.is_some() || far.line.has_handshake() {
            far.transmit(now);
            far.receive(heard, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assumes_a_frame_sent_then_one_heard_every_20_ms() {
        let timeline = assumed(Role::Answer, &Settings::default(), 400);
        assert_eq!(
            timeline.events,
            [
                (Duration::ZERO, Event::Tx),
                (Duration::ZERO, Event::Rx(160)),
                (Duration::from_millis(20), Event::Tx),
                (Duration::from_millis(20), Event::Rx(160)),
                (Duration::from_millis(40), Event::Tx),
                (Duration::from_millis(40), Event::Rx(80)),
            ]
        );
        assert_eq!(timeline.header.role, Some(Role::Answer));
    }
}
