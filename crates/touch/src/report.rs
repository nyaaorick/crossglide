//! The reports the PC writes to the virtual touchpad's feed collection. Their layout must match
//! the report descriptor in `drivers/touchpad/touchpad.c`; the constants here mirror its
//! `#define`s.

use crate::frame::{Frame, MAX_CONTACTS};

/// Report ID of the feed collection, which the agent writes to.
pub const FEED_REPORT_ID: u8 = 0x07;
/// Report ID of the precision touchpad's input report.
pub const TOUCHPAD_REPORT_ID: u8 = 0x01;
/// Bytes after the feed report's ID: the input report, ID first, padded with zeros.
pub const FEED_LEN: usize = 32;
/// A whole feed report, as written to the device.
pub const FEED_REPORT_LEN: usize = 1 + FEED_LEN;

/// Logical maximum of X and Y, in 0.05 mm on a 121.9 x 74.1 mm surface.
pub const X_MAX: u32 = 2438;
pub const Y_MAX: u32 = 1482;

const CONTACT_LEN: usize = 5;
const CONFIDENCE: u8 = 1;
const TIP: u8 = 2;

/// The feed report that makes the touchpad report `frame`.
pub fn touchpad(frame: &Frame) -> [u8; FEED_REPORT_LEN] {
    let mut out = [0; FEED_REPORT_LEN];
    out[0] = FEED_REPORT_ID;
    out[1] = TOUCHPAD_REPORT_ID;
    let contacts = &frame.contacts[..frame.contacts.len().min(MAX_CONTACTS)];
    for (i, c) in contacts.iter().enumerate() {
        let at = 2 + i * CONTACT_LEN;
        // Every contact counts as a finger; Windows does its own palm rejection on top.
        out[at] = CONFIDENCE | if c.tip { TIP } else { 0 } | (c.id & 0x07) << 2;
        out[at + 1..at + 3].copy_from_slice(&scale(c.x, X_MAX).to_le_bytes());
        out[at + 3..at + 5].copy_from_slice(&scale(c.y, Y_MAX).to_le_bytes());
    }
    let tail = 2 + MAX_CONTACTS * CONTACT_LEN;
    // Scan time, in 100 µs, wraps at 16 bits as the spec expects.
    out[tail..tail + 2].copy_from_slice(&(frame.time as u16).to_le_bytes());
    out[tail + 2] = contacts.len() as u8;
    out[tail + 3] = u8::from(frame.button);
    out
}

fn scale(fraction: u16, max: u32) -> u16 {
    ((u32::from(fraction) * max + u32::from(u16::MAX) / 2) / u32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Contact;

    #[test]
    fn reports_match_the_descriptor_layout() {
        let frame = Frame {
            seq: 1,
            time: 0x0012_3456,
            button: true,
            contacts: vec![
                Contact {
                    id: 3,
                    tip: true,
                    x: 65535,
                    y: 0,
                },
                Contact {
                    id: 1,
                    tip: false,
                    x: 32768,
                    y: 65535,
                },
            ],
        };
        let r = touchpad(&frame);
        assert_eq!(r[..2], [FEED_REPORT_ID, TOUCHPAD_REPORT_ID]);
        // Contact 0: confidence, tip, slot 3; x at the far right, y at the top.
        assert_eq!(r[2], 0b0000_1111);
        assert_eq!(u16::from_le_bytes([r[3], r[4]]), 2438);
        assert_eq!(u16::from_le_bytes([r[5], r[6]]), 0);
        // Contact 1: lifted, slot 1; x in the middle, y at the bottom.
        assert_eq!(r[7], 0b0000_0101);
        assert_eq!(u16::from_le_bytes([r[8], r[9]]), 1219);
        assert_eq!(u16::from_le_bytes([r[10], r[11]]), 1482);
        // Unused contacts stay zero, then scan time, count and button.
        assert!(r[12..27].iter().all(|&b| b == 0));
        assert_eq!(u16::from_le_bytes([r[27], r[28]]), 0x3456);
        assert_eq!(r[29..31], [2, 1]);
        // Padding after the 30-byte touchpad report.
        assert!(r[31..].iter().all(|&b| b == 0));
    }
}
