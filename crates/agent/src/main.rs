//! Crossglide agent. Runs on both machines and keeps the side channel up: one QUIC connection
//! from the PC to the Mac, which audio (M3) and later features share.

mod clock;
mod config;
mod control;
mod identity;
mod link;
mod proto;
mod tls;

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::Config;
use crate::identity::Identity;
use crate::link::{Settings, Status};

#[derive(Parser)]
#[command(
    version,
    about = "Crossglide agent: keeps the side channel between the Mac and the PC up"
)]
struct Cli {
    /// Directory with agent.toml and this machine's certificate [default: `crossglide` in the
    /// user's config directory]
    #[arg(long, global = true, value_name = "DIR")]
    config_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Connect to the other machine and stay connected (the default)
    Run,
    /// Print this machine's certificate fingerprint, for the other machine's peer_fingerprint
    Fingerprint,
}

/// `run` also appends its log here, in the config directory.
const LOG_FILE: &str = "agent.log";

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);
    let dir = match cli.config_dir.map_or_else(config::default_dir, Ok) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let log_file = matches!(command, Command::Run).then(|| dir.join(LOG_FILE));
    if let Err(e) = start_logging(log_file.as_deref()) {
        eprintln!("error: {e:#}");
        return ExitCode::FAILURE;
    }
    match run(command, &dir).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Logs to stderr, and also to `file` if given, so a long run can be read back afterwards.
fn start_logging(file: Option<&Path>) -> Result<()> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let terminal = fmt::layer().with_target(false).with_writer(std::io::stderr);
    let file = match file {
        Some(path) => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir)?;
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("can't open {}", path.display()))?;
            Some(
                fmt::layer()
                    .with_target(false)
                    .with_ansi(false)
                    .with_writer(Mutex::new(file)),
            )
        }
        None => None,
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(terminal)
        .with(file)
        .init();
    Ok(())
}

async fn run(command: Command, dir: &Path) -> Result<()> {
    let identity = Identity::load_or_create(dir)?;
    match command {
        Command::Fingerprint => {
            println!("{}", identity.fingerprint);
            Ok(())
        }
        Command::Run => run_agent(dir, identity).await,
    }
}

async fn run_agent(dir: &Path, identity: Identity) -> Result<()> {
    let path = dir.join(config::FILE);
    let Some(config) = Config::load_or_init(&path)? else {
        bail!(
            "created {}: fill in peer_fingerprint (and server, on the PC), then run again. This \
             machine's fingerprint is {}",
            path.display(),
            identity.fingerprint
        );
    };
    let context = || format!("in {}", path.display());
    let settings = Settings {
        mode: config.mode().with_context(context)?,
        peer: config.peer_fingerprint().with_context(context)?,
        identity,
    };
    info!(
        "crossglide-agent {} on {}; this machine's fingerprint is {}",
        env!("CARGO_PKG_VERSION"),
        hostname(),
        settings.identity.fingerprint
    );

    let stop = CancellationToken::new();
    tokio::spawn({
        let stop = stop.clone();
        async move {
            stop_signal().await;
            info!("stopping");
            stop.cancel();
        }
    });
    let (status, _) = watch::channel(Status::default());
    link::run(settings, status, stop).await
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix, so the peer is told the agent is stopping.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("can handle SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// This machine's name, for hellos, logs and the certificate.
fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}
