//! Crossglide agent. Runs on both machines and keeps the side channel up: one QUIC connection
//! from the PC to the Mac, which audio (M3) and later features share. The `crossglide-agent`
//! command line and the `crossglide` tray app both run it.

pub mod app;
pub mod audio;
pub mod clock;
pub mod config;
pub mod control;
pub mod identity;
pub mod link;
pub mod proto;
pub mod tls;

/// This machine's name, for hellos, logs and the certificate.
pub fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().into_owned()
}
