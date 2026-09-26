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
use std::time::Instant;

use softmodem_dsp::pump::{Offer, Role};
use softmodem_link::Setup;
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
