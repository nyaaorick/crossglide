//! Checks the virtual touchpad on the PC without the Mac: slides one finger to the right, then
//! prints where the pointer went. Run it in the desktop session (not over SSH, which has no
//! pointer): `cargo run -p crossglide-touch --example inject`.

#[cfg(windows)]
fn main() {
    use std::thread::sleep;
    use std::time::Duration;

    use crossglide_touch::frame::{Contact, Frame};
    use crossglide_touch::{report, win};

    win::per_monitor_dpi();
    let pad = win::Touchpad::open().unwrap_or_else(|e| panic!("{e}"));
    let before = win::pointer();
    let mut frame = Frame::default();
    // A third of the pad's width in 40 frames of 8 ms, like a quick swipe of one finger.
    for i in 0..=40u16 {
        frame.seq = i;
        frame.time = u32::from(i) * 80;
        frame.contacts = vec![Contact {
            id: 0,
            tip: true,
            x: 20000 + i * 500,
            y: 30000,
        }];
        pad.send(&report::touchpad(&frame)).expect("write a report");
        sleep(Duration::from_millis(8));
    }
    pad.send(&report::touchpad(&frame.lifted()))
        .expect("write a report");
    sleep(Duration::from_millis(100));
    println!("pointer {before:?} -> {:?}", win::pointer());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the inject example drives the Windows virtual touchpad; run it on the PC");
}
