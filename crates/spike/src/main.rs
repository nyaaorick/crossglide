//! M1 audio spike (see ROADMAP.md): small, throwaway commands that check loopback
//! capture, Opus and playback work on the real machines before M3 builds on them.

mod audio_io;
mod capture;
mod codec;
mod net;
mod play;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(about = "M1 audio spike: loopback capture, Opus, playback and a naive UDP stream")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Record the default output device (loopback) into a WAV file
    Capture {
        #[arg(long, default_value_t = 60)]
        seconds: u64,
        #[arg(long, default_value = "capture.wav")]
        out: PathBuf,
    },
    /// Play a sine or a 48 kHz WAV file on the default output device
    Play {
        #[command(flatten)]
        source: SourceArgs,
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long, default_value_t = 0.2)]
        volume: f32,
    },
    /// Round-trip a 48 kHz WAV file through Opus; report size, CPU time and SNR
    Opus {
        wav: PathBuf,
        /// Also write the decoded audio here
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Stream audio to `recv` over plain UDP
    Send {
        /// Receiver address, e.g. 192.168.1.20:5004
        #[arg(long)]
        to: SocketAddr,
        /// Stream the default output device (the real PC → Mac case)
        #[arg(long, conflicts_with = "wav")]
        loopback: bool,
        #[command(flatten)]
        source: SourceArgs,
        /// Stop after this many seconds (default: run until Ctrl-C)
        #[arg(long)]
        seconds: Option<u64>,
        /// Simulate loss: skip sending every Nth packet
        #[arg(long)]
        drop_every: Option<u64>,
    },
    /// Receive a `send` stream and play it on the default output device
    Recv {
        #[arg(long, default_value = "0.0.0.0:5004")]
        listen: SocketAddr,
        /// Audio buffered before playback starts, and again after an underrun
        #[arg(long, default_value_t = 40)]
        buffer_ms: u32,
        #[arg(long, default_value_t = 1.0)]
        volume: f32,
        /// Stop after this many seconds (default: run until Ctrl-C)
        #[arg(long)]
        seconds: Option<u64>,
    },
}

/// A test signal: a sine by default, or a WAV file played in a loop.
#[derive(Args)]
struct SourceArgs {
    /// 48 kHz WAV file to use instead of a sine
    #[arg(long)]
    wav: Option<PathBuf>,
    /// Sine frequency in Hz
    #[arg(long, default_value_t = 440.0)]
    freq: f32,
}

impl SourceArgs {
    fn open(&self) -> Result<audio_io::Source> {
        audio_io::Source::open(self.wav.as_deref(), self.freq)
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Capture { seconds, out } => capture::run(seconds, &out),
        Command::Play {
            source,
            seconds,
            volume,
        } => play::run(source.open()?, seconds, volume),
        Command::Opus { wav, out } => codec::run(&wav, out.as_deref()),
        Command::Send {
            to,
            loopback,
            source,
            seconds,
            drop_every,
        } => {
            let source = if loopback { None } else { Some(source.open()?) };
            net::send(to, source, seconds, drop_every)
        }
        Command::Recv {
            listen,
            buffer_ms,
            volume,
            seconds,
        } => net::recv(listen, buffer_ms, volume, seconds),
    }
}
