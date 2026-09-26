// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! A record of all that a call's line was given, so that a replay can give a
//! fresh line the same, in the same order and at the same times.
//!
//! It is text, one line each: the version, the offer, the V.42 setup of each
//! role and the role the call started in, then one event a line, each after
//! the nanoseconds since the call was attached. The received samples are
//! those of the `-rx.wav` beside it, in order.

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::str::FromStr;
use std::time::{Duration, Instant};

use softmodem_dsp::pump::{Modulation, Offer, Role};
use softmodem_link::v42bis::{Directions, Parameters};
use softmodem_link::{CompressionSetup, Setup};
use tracing::warn;

use crate::line::Setups;
use crate::modem::Hex;

/// Where a call's journal goes, once the call is attached.
pub struct Journal {
    out: Box<dyn Write + Send>,
    start: Option<Instant>,
    failed: bool,
}

impl fmt::Debug for Journal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Journal")
            .field("start", &self.start)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl Journal {
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self {
            out: Box::new(out),
            start: None,
            failed: false,
        }
    }

    /// A journal in a new file at `path`.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be created.
    pub fn create(path: &Path) -> io::Result<Self> {
        Ok(Self::new(BufWriter::new(File::create(path)?)))
    }

    pub(crate) fn header(&mut self, header: &Header) {
        self.write(format_args!("{header}"));
    }

    pub(crate) fn event(&mut self, now: Instant, event: &Event) {
        let start = *self.start.get_or_insert(now);
        let at = now.saturating_duration_since(start).as_nanos();
        self.write(format_args!("{at} {event}\n"));
    }

    fn write(&mut self, text: fmt::Arguments<'_>) {
        if self.failed {
            return;
        }
        if let Err(error) = self.out.write_fmt(text) {
            warn!(%error, "journal not written");
            self.failed = true;
        }
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        if !self.failed
            && let Err(error) = self.out.flush()
        {
            warn!(%error, "journal not written");
        }
    }
}

/// What a call's line started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) offer: Offer,
    pub(crate) setups: Setups,
    pub(crate) role: Option<Role>,
}

/// One thing the line was given, or asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// Samples received, the next of the `-rx.wav`.
    Rx(usize),
    /// A frame made to send.
    Tx,
    /// The frame made last was not sent after all.
    Dropped,
    /// Bytes from the computer.
    Send(Vec<u8>),
    Retrain,
    ClearDown,
    Start(Role),
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let offer = self.offer;
        writeln!(f, "softmodem {VERSION}")?;
        writeln!(
            f,
            "offer top={:?} automode={} max_transmit={}",
            offer.top,
            u8::from(offer.automode),
            offer.max_transmit.map_or("-".into(), |max| max.to_string())
        )?;
        write_setup(f, "originate", &self.setups.originate)?;
        write_setup(f, "answer", &self.setups.answer)?;
        writeln!(f, "role {}", self.role.map_or("-", role_name))
    }
}

fn write_setup(f: &mut fmt::Formatter<'_>, name: &str, setup: &Setup) -> fmt::Result {
    write!(
        f,
        "setup {name} lapm={} detection={} required={} compression=",
        u8::from(setup.lapm),
        u8::from(setup.detection),
        u8::from(setup.required)
    )?;
    match setup.compression {
        None => writeln!(f, "-"),
        Some(compression) => {
            let offer = compression.offer;
            writeln!(
                f,
                "{},{},{},{},{}",
                u8::from(offer.transmit),
                u8::from(offer.receive),
                offer.parameters.codewords,
                offer.parameters.max_string,
                u8::from(compression.required)
            )
        }
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Originate => "originate",
        Role::Answer => "answer",
    }
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rx(samples) => write!(f, "rx {samples}"),
            Self::Tx => f.write_str("tx"),
            Self::Dropped => f.write_str("dropped"),
            Self::Send(bytes) if bytes.is_empty() => f.write_str("send"),
            Self::Send(bytes) => write!(f, "send {}", Hex(bytes)),
            Self::Retrain => f.write_str("retrain"),
            Self::ClearDown => f.write_str("cleardown"),
            Self::Start(role) => write!(f, "start {}", role_name(*role)),
        }
    }
}

/// A journal read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub(crate) version: String,
    pub(crate) header: Header,
    pub(crate) events: Vec<(Duration, Event)>,
}

impl FromStr for Recorded {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let mut lines = text.lines().enumerate().map(|(n, line)| (n + 1, line));
        let mut next = |what: &str| {
            lines
                .next()
                .ok_or_else(|| format!("the journal ends before its {what}"))
        };
        let (n, line) = next("version")?;
        let version = line
            .strip_prefix("softmodem ")
            .ok_or_else(|| format!("line {n}: not a softmodem journal"))?
            .to_owned();
        let (n, line) = next("offer")?;
        let offer = parse_offer(line).map_err(|e| format!("line {n}: {e}"))?;
        let (n, line) = next("originate setup")?;
        let originate = parse_setup(line, "originate").map_err(|e| format!("line {n}: {e}"))?;
        let (n, line) = next("answer setup")?;
        let answer = parse_setup(line, "answer").map_err(|e| format!("line {n}: {e}"))?;
        let (n, line) = next("role")?;
        let role = match line.strip_prefix("role ") {
            Some("-") => None,
            Some(name) => Some(parse_role(name).map_err(|e| format!("line {n}: {e}"))?),
            None => return Err(format!("line {n}: no role")),
        };
        let header = Header {
            offer,
            setups: Setups { originate, answer },
            role,
        };
        let events = lines
            .filter(|(_, line)| !line.is_empty())
            .map(|(n, line)| parse_event(line).map_err(|e| format!("line {n}: {e}")))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            version,
            header,
            events,
        })
    }
}

fn fields<'a>(line: &'a str, prefix: &str) -> Result<Vec<(&'a str, &'a str)>, String> {
    let rest = line
        .strip_prefix(prefix)
        .ok_or_else(|| format!("expected {prefix:?}"))?;
    rest.split_whitespace()
        .map(|field| {
            field
                .split_once('=')
                .ok_or_else(|| format!("{field:?} is not key=value"))
        })
        .collect()
}

fn field<'a>(fields: &[(&str, &'a str)], key: &str) -> Result<&'a str, String> {
    fields
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .ok_or_else(|| format!("no {key}"))
}

fn flag(text: &str) -> Result<bool, String> {
    match text {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("{text:?} is not 0 or 1")),
    }
}

fn number<N: FromStr>(text: &str) -> Result<N, String> {
    text.parse()
        .map_err(|_| format!("{text:?} is not a number"))
}

fn parse_offer(line: &str) -> Result<Offer, String> {
    let fields = fields(line, "offer ")?;
    let top = match field(&fields, "top")? {
        "V21" => Modulation::V21,
        "V22" => Modulation::V22,
        "V22bis" => Modulation::V22bis,
        "V34" => Modulation::V34,
        "V90" => Modulation::V90,
        other => return Err(format!("no modulation {other:?}")),
    };
    let max_transmit = match field(&fields, "max_transmit")? {
        "-" => None,
        max => Some(number(max)?),
    };
    Ok(Offer {
        top,
        automode: flag(field(&fields, "automode")?)?,
        max_transmit,
    })
}

fn parse_setup(line: &str, role: &str) -> Result<Setup, String> {
    let fields = fields(line, &format!("setup {role} "))?;
    let compression = match field(&fields, "compression")? {
        "-" => None,
        text => {
            let parts: Vec<&str> = text.split(',').collect();
            let [transmit, receive, codewords, max_string, required] = parts[..] else {
                return Err(format!("compression {text:?} is not five values"));
            };
            Some(CompressionSetup {
                offer: Directions {
                    transmit: flag(transmit)?,
                    receive: flag(receive)?,
                    parameters: Parameters {
                        codewords: number(codewords)?,
                        max_string: number(max_string)?,
                    },
                },
                required: flag(required)?,
            })
        }
    };
    Ok(Setup {
        lapm: flag(field(&fields, "lapm")?)?,
        detection: flag(field(&fields, "detection")?)?,
        required: flag(field(&fields, "required")?)?,
        compression,
    })
}

fn parse_role(name: &str) -> Result<Role, String> {
    match name {
        "originate" => Ok(Role::Originate),
        "answer" => Ok(Role::Answer),
        _ => Err(format!("no role {name:?}")),
    }
}

fn parse_event(line: &str) -> Result<(Duration, Event), String> {
    let mut words = line.split_whitespace();
    let at = number::<u64>(words.next().unwrap_or_default())?;
    let event = match words.next() {
        Some("rx") => Event::Rx(number(words.next().unwrap_or_default())?),
        Some("tx") => Event::Tx,
        Some("dropped") => Event::Dropped,
        Some("send") => Event::Send(
            words
                .by_ref()
                .map(|byte| {
                    u8::from_str_radix(byte, 16).map_err(|_| format!("{byte:?} is not hex"))
                })
                .collect::<Result<_, _>>()?,
        ),
        Some("retrain") => Event::Retrain,
        Some("cleardown") => Event::ClearDown,
        Some("start") => Event::Start(parse_role(words.next().unwrap_or_default())?),
        Some(other) => return Err(format!("no event {other:?}")),
        None => return Err("no event".into()),
    };
    if let Some(extra) = words.next() {
        return Err(format!("{extra:?} after the event"));
    }
    Ok((Duration::from_nanos(at), event))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let header = Header {
            offer: Offer {
                top: Modulation::V90,
                automode: true,
                max_transmit: Some(28_800),
            },
            setups: Setups {
                originate: Setup {
                    lapm: true,
                    detection: false,
                    required: false,
                    compression: Some(CompressionSetup {
                        offer: Directions {
                            transmit: true,
                            receive: false,
                            parameters: Parameters {
                                codewords: 2048,
                                max_string: 32,
                            },
                        },
                        required: true,
                    }),
                },
                answer: Setup {
                    lapm: false,
                    detection: true,
                    required: true,
                    compression: None,
                },
            },
            role: None,
        };
        let events = [
            (Duration::ZERO, Event::Start(Role::Originate)),
            (Duration::from_nanos(20_000_001), Event::Rx(160)),
            (Duration::from_millis(40), Event::Tx),
            (Duration::from_millis(40), Event::Dropped),
            (Duration::from_millis(41), Event::Send(vec![0x41, 0x0d])),
            (Duration::from_millis(42), Event::Send(Vec::new())),
            (Duration::from_millis(43), Event::Retrain),
            (Duration::from_millis(44), Event::ClearDown),
        ];
        let out = Shared::default();
        let mut journal = Journal::new(out.clone());
        journal.header(&header);
        let start = Instant::now();
        for (at, event) in &events {
            journal.event(start + *at, event);
        }
        drop(journal);

        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let recorded: Recorded = text.parse().unwrap();
        assert_eq!(recorded.version, VERSION);
        assert_eq!(recorded.header, header);
        assert_eq!(recorded.events, events);
    }

    #[test]
    fn says_which_line_it_cannot_read() {
        let text = "softmodem 0.1.0\noffer top=V99 automode=1 max_transmit=-\n";
        assert_eq!(
            text.parse::<Recorded>(),
            Err("line 2: no modulation \"V99\"".into())
        );
    }
}
