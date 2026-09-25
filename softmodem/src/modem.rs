// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

//! The modem as the computer sees it: AT commands, ringing, calls and data.

use std::io;
use std::pin::pin;
use std::time::Duration;

use softmodem_dsp::pump::{Modulation, Offer, Role};
use softmodem_link::v42bis::{Directions, Parameters};
use softmodem_link::{CompressionSetup, Setup};
use softmodem_terminal::command::{self, Command, Dial};
use softmodem_terminal::escape::{EscapeDetector, Timeout};
use softmodem_terminal::line::{Input, LineEditor};
use softmodem_terminal::port::SerialPort;
use softmodem_terminal::settings::{Carrier, Dcd, ResultCode, Settings};
use softmodem_transport::{Call, DialError, Incoming, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::error::TrySendError;
use tokio::time::{Instant, Interval, MissedTickBehavior, interval, sleep, sleep_until};
use tracing::{debug, info, warn};

use crate::line::{Line, Setups};

const FRAME_INTERVAL: Duration = Duration::from_millis(20);
const RING_INTERVAL: Duration = Duration::from_secs(6);
const RING_ON: Duration = Duration::from_secs(2);
const COMMAND_READ: usize = 64;
const DATA_READ: usize = 4;
const DCD_DROP_DELAY: Duration = Duration::from_millis(200);
// V.34's cleardown is S, S̄ and MP from each end, with whatever TRN the far end sends.
const CLEARDOWN_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug)]
enum Mode {
    Command,
    Handshake { deadline: Instant },
    Data { escape: EscapeDetector },
    OnlineCommand,
}

#[derive(Debug)]
struct Ringing<C> {
    caller: C,
    next_ring: Instant,
}

/// A modem on one serial port and one line.
pub struct Modem<T: Transport, P, F> {
    transport: T,
    port: P,
    on_call: F,
    profile: Settings,
    settings: Settings,
    editor: LineEditor,
    last_number: Option<String>,
    off_hook: bool,
    ringing: Option<Ringing<T::Caller>>,
    line: Option<Line>,
    mode: Mode,
    carrier_lost_at: Option<Instant>,
    ticker: Interval,
    dcd: bool,
    ring_off: Option<Instant>,
    speaker: Option<Box<dyn FnMut(f32) + Send>>,
    speaker_gain: Option<f32>,
}

fn frame_clock() -> Interval {
    let mut ticker = interval(FRAME_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Burst);
    ticker
}

impl<T, P, F> Modem<T, P, F>
where
    T: Transport,
    P: SerialPort,
    F: FnMut(Call, Role) -> Call,
{
    /// A modem that starts from `profile`, the settings `ATZ` returns to.
    /// `on_call` sees every call as it is placed or answered, to record it.
    pub fn new(transport: T, port: P, profile: Settings, on_call: F) -> Self {
        Self {
            transport,
            port,
            on_call,
            settings: profile.clone(),
            profile,
            editor: LineEditor::new(),
            last_number: None,
            off_hook: false,
            ringing: None,
            line: None,
            mode: Mode::Command,
            carrier_lost_at: None,
            ticker: frame_clock(),
            dcd: false,
            ring_off: None,
            speaker: None,
            speaker_gain: None,
        }
    }

    /// Calls `set_gain` with the speaker volume each time `L`, `M` or the
    /// call change it, from 0 for off to 1 for full.
    #[must_use]
    pub fn with_speaker(mut self, set_gain: impl FnMut(f32) + Send + 'static) -> Self {
        self.speaker = Some(Box::new(set_gain));
        self
    }

    /// Serves the computer until `stop` completes, or until its input ends
    /// on a port that takes no other computer, and hangs up cleanly either
    /// way. On one that does, the end of input hangs up and waits for the
    /// next computer.
    ///
    /// # Errors
    ///
    /// Fails if the serial port or the transport fails.
    pub async fn run(mut self, stop: impl Future<Output = ()>) -> io::Result<()> {
        let mut stop = pin!(stop);
        let mut buf = [0; COMMAND_READ];
        self.sync_dcd().await?;

        loop {
            self.sync_speaker();
            let in_data = matches!(self.mode, Mode::Data { .. });
            let want_input = !in_data || self.line.as_ref().is_some_and(Line::wants_input);
            let read_size = if in_data { DATA_READ } else { COMMAND_READ };
            let listening = self.line.is_none();
            let next_ring = self.ringing.as_ref().map(|r| r.next_ring);
            let ring_off = self.ring_off;
            let on_line = self.line.is_some();

            tokio::select! {
                () = &mut stop => {
                    self.put_down().await;
                    return Ok(());
                }
                read = self.port.read(&mut buf[..read_size]), if want_input => {
                    let n = read?;
                    if n == 0 {
                        self.put_down().await;
                        if !self.port.takes_another() {
                            return Ok(());
                        }
                        self.off_hook = false;
                        self.editor = LineEditor::new();
                        continue;
                    }
                    self.computer_sent(&buf[..n]).await?;
                }
                incoming = self.transport.incoming(), if listening => {
                    self.incoming(incoming?).await?;
                }
                () = sleep_until(next_ring.unwrap_or_else(Instant::now)), if next_ring.is_some() => {
                    self.ring().await?;
                }
                () = sleep_until(ring_off.unwrap_or_else(Instant::now)), if ring_off.is_some() => {
                    self.stop_ringing()?;
                }
                _ = self.ticker.tick(), if on_line => self.tick().await?,
                samples = receive(&mut self.line) => self.line_sent(samples).await?,
            }
        }
    }

    async fn computer_sent(&mut self, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            match &mut self.mode {
                Mode::Data { escape } => {
                    let mut data = Vec::new();
                    escape.push(
                        byte,
                        self.settings.escape(),
                        self.settings.escape_guard(),
                        Instant::now().into_std(),
                        &mut data,
                    );
                    if let Some(line) = &mut self.line {
                        line.send(&data, Instant::now().into_std());
                    }
                }
                Mode::Handshake { .. } => {
                    info!("handshake aborted from the serial port");
                    return self.hang_up_with(ResultCode::NoCarrier).await;
                }
                Mode::Command | Mode::OnlineCommand => {
                    let mut echo = Vec::new();
                    let input = self.editor.push(byte, &self.settings, &mut echo);
                    self.write(&echo).await?;
                    let Some(Input::Line(text) | Input::Repeat(text)) = input else {
                        continue;
                    };
                    self.execute(&text).await?;
                    if !matches!(self.mode, Mode::Command | Mode::OnlineCommand) {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    async fn execute(&mut self, text: &[u8]) -> io::Result<()> {
        let Ok(commands) = command::parse(text) else {
            return self.report(ResultCode::Error).await;
        };
        for command in commands {
            match command {
                Command::Answer => return self.answer().await,
                Command::Dial(dial) => return self.dial(dial).await,
                Command::Online { retrain } => return self.online(retrain).await,
                Command::Reset => {
                    self.put_down().await;
                    self.settings = self.profile.clone();
                }
                Command::FactoryReset => {
                    self.put_down().await;
                    self.settings = Settings::default();
                }
                Command::OffHook(true) => {
                    self.off_hook = true;
                    if let Some(ringing) = self.ringing.take() {
                        self.transport.reject(&ringing.caller).await?;
                    }
                }
                Command::OffHook(false) => {
                    self.put_down().await;
                    self.off_hook = false;
                }
                Command::Identify(n) => {
                    if let Some(text) = identify(n) {
                        self.write(&self.settings.line(&text)).await?;
                    }
                }
                Command::ReadRegister(register) => {
                    let value = self.settings.register(register);
                    self.write(&self.settings.line(&format!("{value:03}")))
                        .await?;
                }
                Command::ReadCarrier => {
                    let modulation = self.settings.modulation;
                    let text = format!(
                        "+MS: {},{}",
                        modulation.carrier.name(),
                        u8::from(modulation.automode)
                    );
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ListCarriers => {
                    let names: Vec<&str> = Carrier::ALL.iter().map(|c| c.name()).collect();
                    let text = format!("+MS: ({}),(0,1)", names.join(","));
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ReadErrorControl => {
                    let control = self.settings.error_control;
                    let text = format!(
                        "+ES: {},{},{}",
                        control.orig_rqst, control.orig_fbk, control.ans_fbk
                    );
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ListErrorControl => {
                    self.write(&self.settings.line("+ES: (0-3),(0-3),(0-5)"))
                        .await?;
                }
                Command::ReadErrorReport => {
                    let text = format!("+ER: {}", u8::from(self.settings.error_control.report));
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ListErrorReport => {
                    self.write(&self.settings.line("+ER: (0,1)")).await?;
                }
                Command::ReadCompression => {
                    let asked = self.settings.compression;
                    let text = format!(
                        "+DS: {},{},{},{}",
                        asked.direction,
                        u8::from(asked.required),
                        asked.max_dict,
                        asked.max_string
                    );
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ListCompression => {
                    let text = "+DS: (0-3),(0,1),(512-65535),(6-250)";
                    self.write(&self.settings.line(text)).await?;
                }
                Command::ReadCompressionReport => {
                    let text = format!("+DR: {}", u8::from(self.settings.compression.report));
                    self.write(&self.settings.line(&text)).await?;
                }
                Command::ListCompressionReport => {
                    self.write(&self.settings.line("+DR: (0,1)")).await?;
                }
                other => {
                    self.settings.apply(&other);
                }
            }
        }
        self.report(ResultCode::Ok).await?;
        self.sync_dcd().await
    }

    async fn answer(&mut self) -> io::Result<()> {
        self.stop_ringing()?;
        let Some(ringing) = self.ringing.take() else {
            return self.report(ResultCode::NoCarrier).await;
        };
        self.settings.registers[1] = 0;
        match self.transport.answer(&ringing.caller).await {
            Ok(call) => {
                info!("answered");
                self.attach(
                    call,
                    Some(Role::Answer),
                    Instant::now() + self.settings.carrier_wait(),
                );
                Ok(())
            }
            Err(error) => {
                warn!(%error, "answer failed");
                self.report(ResultCode::NoCarrier).await
            }
        }
    }

    async fn dial(&mut self, dial: Dial) -> io::Result<()> {
        if self.line.is_some() {
            return self.report(ResultCode::Error).await;
        }
        let number = if dial.redial {
            self.last_number.clone()
        } else {
            Some(dial.number.clone())
        };
        let Some(number) = number.filter(|n| !n.is_empty()) else {
            return self.report(ResultCode::NoCarrier).await;
        };
        self.last_number = Some(number.clone());
        if let Some(ringing) = self.ringing.take() {
            self.transport.reject(&ringing.caller).await?;
        }

        let deadline = Instant::now() + self.settings.carrier_wait();
        let wait = self.settings.blind_dial_wait() + self.settings.comma_pause() * dial.pauses;
        info!(number, "dialling");
        let transport = &mut self.transport;
        let input = &mut self.port;
        let mut abort = [0];
        let outcome = tokio::select! {
            result = async {
                sleep(wait).await;
                transport.dial(&number).await
            } => Some(result),
            _ = input.read(&mut abort) => None,
            () = sleep_until(deadline) => None,
        };

        match outcome {
            Some(Ok(call)) => {
                let role = if dial.reverse {
                    Role::Answer
                } else {
                    Role::Originate
                };
                if dial.stay_in_command_mode {
                    self.attach(call, None, deadline);
                    self.mode = Mode::OnlineCommand;
                    self.report(ResultCode::Ok).await
                } else {
                    self.attach(call, Some(role), deadline);
                    Ok(())
                }
            }
            Some(Err(DialError::Busy)) => self.report(ResultCode::Busy).await,
            Some(Err(DialError::Io(error))) => {
                warn!(%error, "dial failed");
                self.report(ResultCode::NoCarrier).await
            }
            None => self.report(ResultCode::NoCarrier).await,
        }
    }

    fn attach(&mut self, call: Call, role: Option<Role>, deadline: Instant) {
        let call = (self.on_call)(call, role.unwrap_or(Role::Originate));
        let chosen = self.settings.modulation;
        let offer = Offer {
            top: match chosen.carrier {
                Carrier::V21 => Modulation::V21,
                Carrier::V22 => Modulation::V22,
                Carrier::V22bis => Modulation::V22bis,
                Carrier::V34 => Modulation::V34,
                Carrier::V90 => Modulation::V90,
            },
            automode: chosen.automode,
        };
        let control = self.settings.error_control;
        let asked = self.settings.compression;
        let compression = (asked.direction != 0).then_some(CompressionSetup {
            offer: Directions {
                transmit: asked.transmit(),
                receive: asked.receive(),
                parameters: Parameters {
                    codewords: asked.max_dict,
                    max_string: asked.max_string,
                },
            },
            required: asked.required,
        });
        let setups = Setups {
            originate: Setup {
                lapm: control.originator_tries(),
                detection: control.originator_detects(),
                required: control.originator_requires(),
                compression,
            },
            answer: Setup {
                lapm: control.answerer_tries(),
                detection: true,
                required: control.answerer_requires(),
                compression,
            },
        };
        self.line = Some(Line::new(call, offer, setups, role));
        self.mode = Mode::Handshake { deadline };
        self.carrier_lost_at = None;
        self.ticker.reset();
    }

    async fn online(&mut self, retrain: bool) -> io::Result<()> {
        let Some(line) = &mut self.line else {
            return self.report(ResultCode::NoCarrier).await;
        };
        if !line.has_handshake() {
            line.start(Role::Originate);
            self.mode = Mode::Handshake {
                deadline: Instant::now() + self.settings.carrier_wait(),
            };
            return Ok(());
        }
        if retrain {
            line.retrain();
        }
        let code = ResultCode::connect(line.bit_rate().unwrap_or_default());
        self.mode = Mode::Data {
            escape: EscapeDetector::new(Instant::now().into_std()),
        };
        self.report(code).await
    }

    async fn incoming(&mut self, event: Incoming<T::Caller>) -> io::Result<()> {
        match event {
            Incoming::Ringing { caller, number } => {
                if self.off_hook || self.line.is_some() || self.ringing.is_some() {
                    return self.transport.reject(&caller).await;
                }
                info!(number, "ringing");
                self.settings.registers[1] = 0;
                self.ringing = Some(Ringing {
                    caller,
                    next_ring: Instant::now(),
                });
            }
            Incoming::Gone(caller) => {
                if self.ringing.as_ref().is_some_and(|r| r.caller == caller) {
                    info!("caller gave up");
                    self.stop_ringing()?;
                    self.ringing = None;
                    self.settings.registers[1] = 0;
                }
            }
        }
        Ok(())
    }

    async fn ring(&mut self) -> io::Result<()> {
        let Some(ringing) = &mut self.ringing else {
            return Ok(());
        };
        ringing.next_ring += RING_INTERVAL;
        let rings = self.settings.register(1).saturating_add(1);
        self.settings.registers[1] = rings;
        self.port.set_ring(true)?;
        self.ring_off = Some(Instant::now() + RING_ON);
        self.report(ResultCode::Ring).await?;
        let auto_answer = self.settings.auto_answer_rings();
        if auto_answer != 0 && rings >= auto_answer {
            self.answer().await?;
        }
        Ok(())
    }

    fn stop_ringing(&mut self) -> io::Result<()> {
        if self.ring_off.take().is_some() {
            self.port.set_ring(false)?;
        }
        Ok(())
    }

    async fn tick(&mut self) -> io::Result<()> {
        let Some(line) = &mut self.line else {
            return Ok(());
        };
        let samples = line.transmit(Instant::now().into_std());
        if line.released() {
            info!("error control ended the call");
            return self.hang_up_with(ResultCode::NoCarrier).await;
        }
        match line.call.audio_out.try_send(samples) {
            Ok(()) => {}
            // Blocking here would stop this modem draining its own receive queue.
            Err(TrySendError::Full(_)) => debug!("audio queue full, frame dropped"),
            Err(TrySendError::Closed(_)) => {
                info!("far end hung up");
                return self.hang_up_with(ResultCode::NoCarrier).await;
            }
        }
        if line.cleared() {
            info!("far end cleared down");
            return self.hang_up_with(ResultCode::NoCarrier).await;
        }
        let has_handshake = line.has_handshake();
        let carrier = line.carrier();
        let now = Instant::now();

        match &mut self.mode {
            Mode::Handshake { deadline } if now >= *deadline => {
                info!("no carrier in time");
                return self.hang_up_with(ResultCode::NoCarrier).await;
            }
            Mode::Data { escape } => {
                match escape.poll(self.settings.escape_guard(), now.into_std()) {
                    Some(Timeout::Escaped) => {
                        self.mode = Mode::OnlineCommand;
                        self.report(ResultCode::Ok).await?;
                    }
                    Some(Timeout::Release(bytes)) => line.send(&bytes, now.into_std()),
                    None => {}
                }
            }
            _ => {}
        }

        let connected = matches!(self.mode, Mode::Data { .. } | Mode::OnlineCommand);
        if !(connected && has_handshake) {
            return Ok(());
        }
        if carrier {
            self.carrier_lost_at = None;
            return Ok(());
        }
        let lost_at = *self.carrier_lost_at.get_or_insert(now);
        if now - lost_at >= self.settings.carrier_loss_hang_up() {
            info!("carrier lost");
            return self.hang_up_with(ResultCode::NoCarrier).await;
        }
        Ok(())
    }

    async fn line_sent(&mut self, samples: Option<Vec<i16>>) -> io::Result<()> {
        let Some(samples) = samples else {
            info!("far end hung up");
            return self.hang_up_with(ResultCode::NoCarrier).await;
        };
        let Some(line) = &mut self.line else {
            return Ok(());
        };
        let received = line.receive(&samples, Instant::now().into_std());
        if line.released() {
            info!("error control ended the call");
            return self.hang_up_with(ResultCode::NoCarrier).await;
        }
        if received.connected && matches!(self.mode, Mode::Handshake { .. }) {
            let bit_rate = line.bit_rate().unwrap_or_default();
            let protocol = if received.reliable { "LAPM" } else { "NONE" };
            let compression = match received.compression.map(|c| (c.transmit, c.receive)) {
                Some((true, true)) => "V42B",
                Some((false, true)) => "V42B RD",
                Some((true, false)) => "V42B TD",
                _ => "NONE",
            };
            info!("CONNECT {bit_rate}, error control {protocol}, compression {compression}");
            self.mode = Mode::Data {
                escape: EscapeDetector::new(Instant::now().into_std()),
            };
            self.carrier_lost_at = None;
            if self.settings.error_control.report && !self.settings.quiet {
                let text = format!("+ER: {protocol}");
                self.write(&self.settings.line(&text)).await?;
            }
            if self.settings.compression.report && !self.settings.quiet {
                let text = format!("+DR: {compression}");
                self.write(&self.settings.line(&text)).await?;
            }
            self.report(ResultCode::connect(bit_rate)).await?;
            self.sync_dcd().await?;
        }
        if matches!(self.mode, Mode::Data { .. }) && !received.bytes.is_empty() {
            self.write(&received.bytes).await?;
        }
        Ok(())
    }

    async fn clear_down(&mut self) {
        let Some(line) = &mut self.line else {
            return;
        };
        if !line.clear_down() {
            return;
        }
        let deadline = Instant::now() + CLEARDOWN_WAIT;
        let mut ticker = frame_clock();
        while !line.cleared() {
            tokio::select! {
                _ = ticker.tick() => {
                    let samples = line.transmit(Instant::now().into_std());
                    if line.call.audio_out.try_send(samples).is_err() {
                        break;
                    }
                }
                samples = line.call.audio_in.recv() => {
                    let Some(samples) = samples else {
                        break;
                    };
                    line.receive(&samples, Instant::now().into_std());
                }
                () = sleep_until(deadline) => break,
            }
        }
        info!(cleared = line.cleared(), "cleardown");
    }

    // A hang-up this end chose, while the far end may still be there to clear down with.
    async fn put_down(&mut self) {
        self.clear_down().await;
        self.hang_up().await;
    }

    async fn hang_up(&mut self) {
        if let Some(line) = self.line.take() {
            line.call.hang_up().await;
            info!("call ended");
        }
        self.mode = Mode::Command;
        self.carrier_lost_at = None;
    }

    async fn hang_up_with(&mut self, code: ResultCode) -> io::Result<()> {
        self.hang_up().await;
        self.report(code).await?;
        self.sync_dcd().await
    }

    async fn sync_dcd(&mut self) -> io::Result<()> {
        let connected = self.line.as_ref().is_some_and(Line::has_handshake)
            && matches!(self.mode, Mode::Data { .. } | Mode::OnlineCommand);
        let dcd = match self.settings.dcd {
            Dcd::AlwaysOn => true,
            Dcd::FollowsCarrier => connected,
        };
        if dcd == self.dcd {
            return Ok(());
        }
        if !dcd {
            // Lets the computer read the result code before a hang-up discards it.
            sleep(DCD_DROP_DELAY).await;
        }
        self.dcd = dcd;
        self.port.set_carrier(dcd)
    }

    fn sync_speaker(&mut self) {
        let Some(set_gain) = &mut self.speaker else {
            return;
        };
        let connected = matches!(self.mode, Mode::Data { .. } | Mode::OnlineCommand);
        let gain = self.settings.speaker_gain(connected);
        if self.speaker_gain != Some(gain) {
            self.speaker_gain = Some(gain);
            set_gain(gain);
        }
    }

    async fn report(&mut self, code: ResultCode) -> io::Result<()> {
        let bytes = self.settings.report(code);
        self.write(&bytes).await
    }

    async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        self.port.write_all(bytes).await?;
        self.port.flush().await
    }
}

async fn receive(line: &mut Option<Line>) -> Option<Vec<i16>> {
    match line {
        Some(line) => line.call.audio_in.recv().await,
        None => std::future::pending().await,
    }
}

fn identify(n: u8) -> Option<String> {
    match n {
        0 => Some("softmodem".into()),
        3 => Some(format!("softmodem {}", env!("CARGO_PKG_VERSION"))),
        4 => Some(
            "V.21 300 bit/s, V.22 1200 bit/s, V.22bis 2400 bit/s, V.34 33600 bit/s, V.90 56000 bit/s, V.42 LAPM, V.42bis"
                .into(),
        ),
        _ => None,
    }
}

/// The stored profile: Hayes defaults with `init` applied, as `ATZ` restores.
///
/// # Errors
///
/// Fails if `init` does not parse, or holds a command that does more than
/// change a setting.
pub fn profile(init: &str) -> Result<Settings, String> {
    let text = init.trim();
    let text = if text.len() >= 2 && text[..2].eq_ignore_ascii_case("AT") {
        &text[2..]
    } else {
        text
    };
    let commands = command::parse(text.as_bytes()).map_err(|_| format!("cannot parse {init:?}"))?;
    let mut settings = Settings::default();
    for command in &commands {
        if *command == Command::FactoryReset {
            settings = Settings::default();
        } else if !settings.apply(command) {
            return Err(format!("{command:?} does not belong in a stored profile"));
        }
    }
    Ok(settings)
}
