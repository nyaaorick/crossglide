//! Starting the agent: logging and settings, shared by the command line and the tray app.

use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use crossglide_audio::device::Source;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{self, Config};
use crate::identity::Identity;
use crate::link::Settings;

/// The agent appends its log here, in the config directory.
pub const LOG_FILE: &str = "agent.log";

/// Logs to stderr, and also to `file` if given, so a long run can be read back afterwards.
pub fn start_logging(file: Option<&Path>) -> Result<()> {
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

/// Reads `agent.toml` in `dir` (writing a template there the first time) into the settings for
/// `link::run`. `audio` is `None` to leave audio off.
pub fn load_settings(dir: &Path, identity: Identity, audio: Option<Source>) -> Result<Settings> {
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
        audio,
    };
    info!(
        "crossglide-agent {} on {}; this machine's fingerprint is {}",
        env!("CARGO_PKG_VERSION"),
        crate::hostname(),
        settings.identity.fingerprint
    );
    Ok(settings)
}
