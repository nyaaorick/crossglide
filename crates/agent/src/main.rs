//! `crossglide-agent`: runs the agent from a terminal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crossglide_agent::app::{self, LOG_FILE};
use crossglide_agent::config;
use crossglide_agent::identity::Identity;
use crossglide_agent::link::{self, Status};
use crossglide_audio::device::Source;

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
    Run {
        /// On the PC: send a 440 Hz test tone instead of what the PC plays
        #[arg(long)]
        test_tone: bool,
    },
    /// Print this machine's certificate fingerprint, for the other machine's peer_fingerprint
    Fingerprint,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run { test_tone: false });
    let dir = match cli.config_dir.map_or_else(config::default_dir, Ok) {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let log_file = matches!(command, Command::Run { .. }).then(|| dir.join(LOG_FILE));
    if let Err(e) = app::start_logging(log_file.as_deref()) {
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

async fn run(command: Command, dir: &Path) -> Result<()> {
    let identity = Identity::load_or_create(dir)?;
    match command {
        Command::Fingerprint => {
            println!("{}", identity.fingerprint);
            Ok(())
        }
        Command::Run { test_tone } => {
            let source = if test_tone {
                Source::Tone
            } else {
                Source::Loopback
            };
            let settings = app::load_settings(dir, identity, Some(source))?;
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
    }
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
