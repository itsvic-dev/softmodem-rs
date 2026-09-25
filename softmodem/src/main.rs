// SPDX-FileCopyrightText: 2026 Wiktor Bryk <contact@itsvic.dev>
//
// SPDX-License-Identifier: GPL-3.0-or-later

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use softmodem::{Modem, Role};
use softmodem_terminal::cuse::CusePort;
use softmodem_terminal::port::{Plain, SerialPort};
use softmodem_terminal::pty::Pty;
use softmodem_terminal::settings::Settings;
use softmodem_terminal::tcp::TcpPort;
use softmodem_transport::sip::{Account, Sip};
use softmodem_transport::speaker::Speaker;
use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{Call, Transport, wav};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

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
    /// Register with a SIP registrar over TCP and use it as the phone line.
    #[command(
        after_help = "Without --password, --password-env or --password-file, the modem asks for the password on stdin."
    )]
    Sip(SipArgs),
}

#[derive(Args)]
struct SipArgs {
    /// Host name of the registrar.
    #[arg(long)]
    registrar: String,
    /// User name, which is also the number the modem answers on.
    #[arg(long)]
    user: String,
    #[command(flatten)]
    password: password::Source,
    #[command(flatten)]
    modem: ModemArgs,
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
    #[arg(long, default_value_t = 0)]
    seed: u64,
    #[command(flatten)]
    modem: ModemArgs,
}

#[derive(Args)]
struct ModemArgs {
    /// Serial port as a pseudoterminal linked from this path, not stdin and stdout.
    #[arg(long, conflicts_with_all = ["cuse", "tcp"])]
    pty: Option<PathBuf>,
    /// Serial port as the character device /dev/NAME, with DCD and RI. Linux only.
    #[arg(long, value_name = "NAME", conflicts_with = "tcp")]
    cuse: Option<String>,
    /// Serial port as a TCP listener on ADDR, for one computer at a time.
    #[arg(long, value_name = "ADDR")]
    tcp: Option<SocketAddr>,
    /// Commands for the stored profile that ATZ restores, such as "ATS0=1".
    #[arg(long, default_value = "")]
    init: String,
    /// Directory to record each call into, as one WAV file per direction.
    #[arg(long)]
    dump: Option<PathBuf>,
    /// Play each call on the default sound output, as ATL and ATM set.
    #[arg(long)]
    speaker: bool,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(run(Cli::parse()));
    // A pending stdin read blocks a normal shutdown until the next line of input.
    runtime.shutdown_background();
    result
}

async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Wire(args) => {
            let impairment = Impairment {
                loss: args.loss,
                reorder: args.reorder,
                seed: args.seed,
            };
            let wire = Wire::bind(args.local, args.peer, impairment).await?;
            serve(wire, args.modem).await
        }
        Command::Sip(args) => {
            let sip = Sip::register(Account {
                registrar: args.registrar,
                user: args.user,
                password: args.password.read()?,
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
            let call = match &dump {
                Some(directory) => record(call, directory, role),
                None => call,
            };
            match &speaker {
                Some(speaker) => speaker.play(call),
                None => call,
            }
        };
        let mut modem = Modem::new(self.transport, port, self.profile, on_call);
        if let Some(speaker) = self.speaker {
            modem = modem.with_speaker(move |gain| speaker.set_gain(gain));
        }
        modem.run(shutdown_signal()).await?;
        Ok(())
    }
}

fn record(call: Call, directory: &Path, role: Role) -> Call {
    let started = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let name = match role {
        Role::Originate => "originate",
        Role::Answer => "answer",
    };
    let prefix = directory.join(format!("{started}-{name}"));
    match wav::Recorder::create(&prefix) {
        Ok(recorder) => recorder.record(call),
        Err(error) => {
            warn!(%error, prefix = %prefix.display(), "not recording this call");
            call
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
