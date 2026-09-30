//! Touch: while the PC has control, the Mac's trackpad drives a virtual Windows Precision
//! Touchpad, so Windows' own gestures work, and the Mac's keyboard types on the PC.
//!
//! The Mac captures raw finger contacts (`mac::Capture`) and sends them as [`frame::Frame`]s in
//! QUIC datagrams; the PC turns each into a touchpad report ([`report`]) and writes it to the
//! driver in `drivers/touchpad` (`win::Touchpad`). Control moves by pushing the pointer through
//! a screen edge ([`edge`]) or by a hotkey, both seen by the Mac's event tap (`mac::Tap`).

pub mod edge;
pub mod frame;
pub mod keymap;
pub mod report;

#[cfg(windows)]
pub mod install;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(windows)]
pub mod win;
