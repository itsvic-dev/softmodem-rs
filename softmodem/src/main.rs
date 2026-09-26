// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use softmodem::replay::{Recording, Script};
use softmodem::{Journal, Modem, Role};
use softmodem_terminal::cuse::CusePort;
use softmodem_terminal::port::{Plain, SerialPort};
use softmodem_terminal::pty::Pty;
use softmodem_terminal::settings::Settings;
use softmodem_terminal::tcp::TcpPort;
use softmodem_terminal::tty::TtyPort;
use softmodem_transport::sip::{Account, Protocol, Sip};
use softmodem_transport::speaker::Speaker;
use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{Call, Transport, wav};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

mod password;

/// A modem that places real calls.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Use a direct UDP wire to another softmodem as the phone line.
    Wire(WireArgs),
    /// Register with a SIP registrar and use it as the phone line.
    #[command(
        after_help = "Without --password, --password-env or --password-file, the modem asks for the password on stdin."
    )]
    Sip(SipArgs),
    /// Play a recorded call again into a fresh modem, and log what it does.
    #[command(
        after_help = "With PREFIX, it reads PREFIX.journal, PREFIX-rx.wav and PREFIX-tx.wav, as --dump writes them. Without it, --rx and --role give the call, and the modem sends a frame and then hears one, every 20 ms. RUST_LOG=debug also logs the data."
    )]
    Replay(ReplayArgs),
}

#[derive(Args)]
struct ReplayArgs {
    /// A call recorded with --dump, such as dumps/1790000000-answer.
    #[arg(required_unless_present = "rx", conflicts_with_all = ["rx", "role", "init"])]
    prefix: Option<PathBuf>,
    /// What this end heard, as 16-bit samples at 8 kHz.
    #[arg(long, requires = "role")]
    rx: Option<PathBuf>,
    /// The channel of --rx, from 0.
    #[arg(long, default_value_t = 0)]
    rx_channel: u16,
    /// What this end sent, to check the replay against.
    #[arg(long, conflicts_with = "prefix")]
    tx: Option<PathBuf>,
    /// The channel of --tx, from 0.
    #[arg(long, default_value_t = 0)]
    tx_channel: u16,
    /// The role this end had in the call.
    #[arg(long, value_enum)]
    role: Option<CallRole>,
    /// Commands for the settings the call had, such as "AT+MS=V34".
    #[arg(long, default_value = "")]
    init: String,
    /// Also play what this end sent into a modem of the far end's role.
    #[arg(long)]
    far: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum CallRole {
    Originate,
    Answer,
}

impl From<CallRole> for Role {
    fn from(role: CallRole) -> Self {
        match role {
            CallRole::Originate => Self::Originate,
            CallRole::Answer => Self::Answer,
        }
    }
}

#[derive(Args)]
struct SipArgs {
    /// Host name of the registrar, with a port if not 5060.
    #[arg(long)]
    registrar: String,
    /// User name, which is also the number the modem answers on.
    #[arg(long)]
    user: String,
    /// How SIP messages reach the registrar.
    #[arg(long, value_enum, default_value_t = SipProtocol::Tcp)]
    protocol: SipProtocol,
    #[command(flatten)]
    password: password::Source,
    #[command(flatten)]
    modem: ModemArgs,
}

#[derive(Clone, Copy, ValueEnum)]
enum SipProtocol {
    Tcp,
    Udp,
}

impl From<SipProtocol> for Protocol {
    fn from(protocol: SipProtocol) -> Self {
        match protocol {
            SipProtocol::Tcp => Self::Tcp,
            SipProtocol::Udp => Self::Udp,
        }
    }
}

#[derive(Args)]
struct WireArgs {
    /// Address to take calls on.
    #[arg(long)]
    local: SocketAddr,
    /// Where ATD calls.
    #[arg(long)]
    peer: Option<SocketAddr>,
    /// Chance that an outgoing packet is dropped.
    #[arg(long, default_value_t = 0.0)]
    loss: f64,
    /// Chance that an outgoing packet is sent after the next one.
    #[arg(long, default_value_t = 0.0)]
    reorder: f64,
    /// Chance that an outgoing frame vanishes with no gap the far end could fill.
    #[arg(long, default_value_t = 0.0)]
    slip: f64,
    /// Chance that the stream stops for --stall-for before a frame, as on a busy host.
    #[arg(long, default_value_t = 0.0)]
    stall: f64,
    /// Length of each stall.
    #[arg(long, value_name = "MS", default_value_t = 200)]
    stall_for: u64,
    #[arg(long, default_value_t = 0)]
    seed: u64,
    /// Extra delay each outgoing frame waits, in whole frames of 20 ms.
    #[arg(long, value_name = "MS", default_value_t = 0)]
    delay: u64,
    /// Chance that an outgoing frame goes as silence, as a provider's gateway conceals a lost packet.
    #[arg(long, default_value_t = 0.0)]
    conceal: f64,
    /// Most of the noise added to each outgoing sample before it is coded.
    #[arg(long, value_name = "AMPLITUDE", default_value_t = 0)]
    noise: u16,
    /// Once the far end has been quiet this long, add --gateway-noise from then on, as a gateway that gives up on the modem.
    #[arg(long, value_name = "MS")]
    gateway_after: Option<u64>,
    #[arg(long, value_name = "AMPLITUDE", default_value_t = 100)]
    gateway_noise: u16,
    #[command(flatten)]
    modem: ModemArgs,
}

#[derive(Args)]
struct ModemArgs {
    /// Serial port as a pseudoterminal linked from this path, not stdin and stdout.
    #[arg(long, conflicts_with_all = ["cuse", "tcp", "serial"])]
    pty: Option<PathBuf>,
    /// Serial port as the character device /dev/NAME, with DCD and RI. Linux only.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["tcp", "serial"])]
    cuse: Option<String>,
    /// Serial port as a TCP listener on ADDR, for one computer at a time.
    #[arg(long, value_name = "ADDR", conflicts_with = "serial")]
    tcp: Option<SocketAddr>,
    /// Serial port as the tty device at PATH, such as a UART or a USB gadget's /dev/ttyGS0.
    #[arg(long, value_name = "PATH")]
    serial: Option<PathBuf>,
    /// Speed of the --serial device, in bits per second.
    #[arg(
        long,
        value_name = "BPS",
        default_value_t = 115_200,
        requires = "serial"
    )]
    baud: u32,
    /// Commands for the stored profile that ATZ restores, such as "ATS0=1".
    #[arg(long, default_value = "")]
    init: String,
    /// Directory to record each call into, as one WAV file per direction and a journal for `replay`.
    #[arg(long)]
    dump: Option<PathBuf>,
    /// Play each call on the default sound output, as ATL and ATM set.
    #[arg(long)]
    speaker: bool,
}

fn main() -> anyhow::Result<()> {
    let command = Cli::parse().command;
    let replaying = matches!(command, Command::Replay(_));
    let default = if replaying {
        "info,softmodem::line=debug"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let log = tracing_subscriber::fmt().with_env_filter(filter);
    if let Command::Replay(args) = command {
        log.with_timer(ReplayClock)
            .with_target(false)
            .with_writer(std::io::stdout)
            .log_internal_errors(false)
            .init();
        return replay(&args);
    }
    log.with_writer(std::io::stderr).init();

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(run(command));
    // A pending stdin read blocks a normal shutdown until the next line of input.
    runtime.shutdown_background();
    result
}

// Seconds into the call being replayed.
struct ReplayClock;

impl FormatTime for ReplayClock {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        write!(w, "{:9.3}", softmodem::replay::clock().as_secs_f64())
    }
}

fn replay(args: &ReplayArgs) -> anyhow::Result<()> {
    let read = |path: &Path, channel| {
        wav::read(path, channel).with_context(|| format!("reading {}", path.display()))
    };
    let (script, rx, tx) = if let Some(prefix) = &args.prefix {
        let prefix = recording_prefix(prefix);
        let path = prefix.with_extension("journal");
        let journal = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let rx = read(&with_suffix(&prefix, "-rx.wav"), 0)?;
        let tx_path = with_suffix(&prefix, "-tx.wav");
        let tx = match read(&tx_path, 0) {
            Ok(tx) => Some(tx),
            Err(error) => {
                warn!(%error, "not checking what the replay sends");
                None
            }
        };
        (Script::Journal(journal), rx, tx)
    } else {
        let (Some(rx), Some(role)) = (&args.rx, args.role) else {
            anyhow::bail!("give a PREFIX, or --rx and --role");
        };
        let settings = softmodem::profile(&args.init).map_err(anyhow::Error::msg)?;
        let script = Script::Assumed {
            role: role.into(),
            settings: Box::new(settings),
        };
        let tx = match &args.tx {
            Some(path) => Some(read(path, args.tx_channel)?),
            None => None,
        };
        (script, read(rx, args.rx_channel)?, tx)
    };
    let recording = Recording {
        rx: &rx,
        tx: tx.as_deref(),
    };
    softmodem::replay::replay(script, recording, args.far).map_err(anyhow::Error::msg)?;
    Ok(())
}

// The prefix of a recording, also from the path of one of its files.
fn recording_prefix(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    [".journal", "-rx.wav", "-tx.wav"]
        .iter()
        .find_map(|suffix| text.strip_suffix(suffix))
        .map_or_else(|| path.to_owned(), PathBuf::from)
}

fn with_suffix(prefix: &Path, suffix: &str) -> PathBuf {
    let mut name = prefix.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Replay(args) => replay(&args),
        Command::Wire(args) => {
            let impairment = Impairment {
                loss: args.loss,
                reorder: args.reorder,
                slip: args.slip,
                stall: args.stall,
                stall_for: Duration::from_millis(args.stall_for),
                seed: args.seed,
                delay: Duration::from_millis(args.delay),
                conceal: args.conceal,
                noise: args.noise,
                gateway_after: args.gateway_after.map(Duration::from_millis),
                gateway_noise: args.gateway_noise,
            };
            let wire = Wire::bind(args.local, args.peer, impairment).await?;
            serve(wire, args.modem).await
        }
        Command::Sip(args) => {
            let sip = Sip::register(Account {
                registrar: args.registrar,
                user: args.user,
                password: args.password.read()?,
                protocol: args.protocol.into(),
            })
            .await
            .context("registering")?;
            Box::pin(serve(sip, args.modem)).await
        }
    }
}

async fn serve(transport: impl Transport, args: ModemArgs) -> anyhow::Result<()> {
    let profile = softmodem::profile(&args.init).map_err(anyhow::Error::msg)?;
    if let Some(directory) = &args.dump {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("creating {}", directory.display()))?;
    }
    let speaker = if args.speaker {
        Some(Arc::new(Speaker::open().context("opening the speaker")?))
    } else {
        None
    };
    let station = Station {
        transport,
        profile,
        dump: args.dump,
        speaker,
    };

    if let Some(link) = args.pty {
        let pty = Pty::open(Some(&link))?;
        info!(path = %pty.path().display(), link = %link.display(), "serial port ready");
        station.run(pty).await
    } else if let Some(name) = args.cuse {
        let port = CusePort::open(&name).context("opening /dev/cuse")?;
        info!(device = %format!("/dev/{name}"), "serial port ready");
        station.run(port).await
    } else if let Some(address) = args.tcp {
        let port = TcpPort::bind(address)
            .await
            .with_context(|| format!("listening on {address}"))?;
        info!(address = %port.local_addr()?, "serial port ready");
        station.run(port).await
    } else if let Some(path) = args.serial {
        let port = TtyPort::open(&path, args.baud)
            .with_context(|| format!("opening {}", path.display()))?;
        info!(path = %path.display(), baud = args.baud, "serial port ready");
        station.run(port).await
    } else {
        station
            .run(Plain {
                input: tokio::io::stdin(),
                output: tokio::io::stdout(),
            })
            .await
    }
}

struct Station<T> {
    transport: T,
    profile: Settings,
    dump: Option<PathBuf>,
    speaker: Option<Arc<Speaker>>,
}

impl<T: Transport> Station<T> {
    async fn run(self, port: impl SerialPort) -> anyhow::Result<()> {
        let dump = self.dump;
        let speaker = self.speaker.clone();
        let on_call = move |call: Call, role: Role| {
            let (call, journal) = match &dump {
                Some(directory) => record(call, directory, role),
                None => (call, None),
            };
            let call = match &speaker {
                Some(speaker) => speaker.play(call),
                None => call,
            };
            (call, journal)
        };
        let mut modem = Modem::new(self.transport, port, self.profile, on_call);
        if let Some(speaker) = self.speaker {
            modem = modem.with_speaker(move |gain| speaker.set_gain(gain));
        }
        modem.run(shutdown_signal()).await?;
        Ok(())
    }
}

fn record(call: Call, directory: &Path, role: Role) -> (Call, Option<Journal>) {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let name = match role {
        Role::Originate => "originate",
        Role::Answer => "answer",
    };
    let prefix = directory.join(format!("{started}-{name}"));
    let call = match wav::Recorder::create(&prefix) {
        Ok(recorder) => recorder.record(call),
        Err(error) => {
            warn!(%error, prefix = %prefix.display(), "not recording this call");
            return (call, None);
        }
    };
    let path = prefix.with_extension("journal");
    match Journal::create(&path) {
        Ok(journal) => (call, Some(journal)),
        Err(error) => {
            warn!(%error, path = %path.display(), "no journal for this call");
            (call, None)
        }
    }
}

async fn shutdown_signal() {
    let Ok(mut terminate) = signal(SignalKind::terminate()) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}
