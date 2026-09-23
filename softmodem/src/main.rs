use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use softmodem::{Modem, Role};
use softmodem_terminal::cuse::CusePort;
use softmodem_terminal::port::Plain;
use softmodem_terminal::pty::Pty;
use softmodem_transport::sip::{Account, Sip};
use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{Call, Transport, wav};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{info, warn};

/// A V.21 modem that places real calls.
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
    /// Environment variable that holds the password.
    #[arg(long, value_name = "VARIABLE")]
    password_env: String,
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
    #[arg(long, conflicts_with = "cuse")]
    pty: Option<PathBuf>,
    /// Serial port as the character device /dev/NAME, with DCD and RI. Linux only.
    #[arg(long, value_name = "NAME")]
    cuse: Option<String>,
    /// Commands for the stored profile that ATZ restores, such as "ATS0=1".
    #[arg(long, default_value = "")]
    init: String,
    /// Directory to record each call into, as one WAV file per direction.
    #[arg(long)]
    dump: Option<PathBuf>,
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
            let password = std::env::var(&args.password_env)
                .with_context(|| format!("reading the password from ${}", args.password_env))?;
            let sip = Sip::register(Account {
                registrar: args.registrar,
                user: args.user,
                password,
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
    let dump = args.dump;
    let on_call = move |call: Call, role: Role| match &dump {
        Some(directory) => record(call, directory, role),
        None => call,
    };

    if let Some(link) = args.pty {
        let pty = Pty::open(Some(&link))?;
        info!(path = %pty.path().display(), link = %link.display(), "serial port ready");
        Modem::new(transport, pty, profile, on_call)
            .run(shutdown_signal())
            .await?;
    } else if let Some(name) = args.cuse {
        let port = CusePort::open(&name).context("opening /dev/cuse")?;
        info!(device = %format!("/dev/{name}"), "serial port ready");
        Modem::new(transport, port, profile, on_call)
            .run(shutdown_signal())
            .await?;
    } else {
        let port = Plain {
            input: tokio::io::stdin(),
            output: tokio::io::stdout(),
        };
        Modem::new(transport, port, profile, on_call)
            .run(shutdown_signal())
            .await?;
    }
    Ok(())
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
