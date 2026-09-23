use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use softmodem::Role;
use softmodem_terminal::pty::Pty;
use softmodem_transport::wire::{Impairment, Wire};
use softmodem_transport::{Call, Transport, wav};
use tracing::info;

/// A V.21 modem that places real calls.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Talk to another softmodem over UDP, with data on stdin and stdout.
    Wire {
        #[command(subcommand)]
        role: WireRole,
        #[command(flatten)]
        options: WireOptions,
    },
}

#[derive(Subcommand)]
enum WireRole {
    /// Call the far end.
    Originate {
        #[arg(long)]
        peer: SocketAddr,
        #[arg(long, default_value = "0.0.0.0:0")]
        local: SocketAddr,
        #[arg(long, default_value = "0300")]
        number: String,
    },
    /// Wait for one call.
    Answer {
        #[arg(long)]
        local: SocketAddr,
    },
}

#[derive(Args)]
struct WireOptions {
    /// Use a pseudoterminal linked from this path instead of stdin and stdout.
    #[arg(long, global = true)]
    pty: Option<PathBuf>,
    /// Directory to record each call into, as one WAV file per direction.
    #[arg(long, global = true)]
    dump: Option<PathBuf>,
    /// Chance that an outgoing packet is dropped.
    #[arg(long, global = true, default_value_t = 0.0)]
    loss: f64,
    /// Chance that an outgoing packet is sent after the next one.
    #[arg(long, global = true, default_value_t = 0.0)]
    reorder: f64,
    #[arg(long, global = true, default_value_t = 0)]
    seed: u64,
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
    let Command::Wire { role, options } = cli.command;
    let impairment = Impairment {
        loss: options.loss,
        reorder: options.reorder,
        seed: options.seed,
    };
    let pty = options
        .pty
        .as_deref()
        .map(|link| Pty::open(Some(link)))
        .transpose()?;
    if let Some(pty) = &pty {
        info!(path = %pty.path().display(), "serial port ready");
    }
    match role {
        WireRole::Originate {
            peer,
            local,
            number,
        } => {
            let mut wire = Wire::bind(local, Some(peer), impairment).await?;
            let call = wire.dial(&number).await?;
            serve(call, Role::Originate, &options, pty.as_ref()).await
        }
        WireRole::Answer { local } => {
            let mut wire = Wire::bind(local, None, impairment).await?;
            loop {
                let call = wire.accept().await?;
                serve(call, Role::Answer, &options, pty.as_ref()).await?;
                if pty.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

async fn serve(
    call: Call,
    role: Role,
    options: &WireOptions,
    pty: Option<&Pty>,
) -> anyhow::Result<()> {
    let call = match &options.dump {
        Some(directory) => record(call, directory, role)?,
        None => call,
    };
    match pty {
        Some(pty) => softmodem::run(call, role, pty, pty).await?,
        None => softmodem::run(call, role, tokio::io::stdin(), tokio::io::stdout()).await?,
    }
    Ok(())
}

fn record(call: Call, directory: &Path, role: Role) -> anyhow::Result<Call> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    let started = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let name = match role {
        Role::Originate => "originate",
        Role::Answer => "answer",
    };
    Ok(wav::record(
        call,
        &directory.join(format!("{started}-{name}")),
    )?)
}
