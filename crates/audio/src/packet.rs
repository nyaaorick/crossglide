//! The audio packet: one 10 ms Opus frame behind a 7-byte header, sent as one QUIC datagram.
//!
//! | Field           | Size    | Notes                                              |
//! | --------------- | ------- | -------------------------------------------------- |
//! | Version         | 1 byte  | Rejects mismatched builds                          |
//! | Sequence number | 2 bytes | Big-endian, wraps; detects loss and reordering     |
//! | Timestamp       | 4 bytes | Big-endian sample index at 48 kHz, wraps           |
//! | Opus payload    | rest    | About 160 bytes at 128 kbps                        |

use std::fmt;

/// Format version; bumped when the header or the codec settings change.
pub const VERSION: u8 = 1;

pub const HEADER_LEN: usize = 7;

/// Largest payload accepted: far more than a 10 ms frame at 128 kbps needs (292 bytes was the
/// largest in M1), and less than a datagram can carry.
pub const MAX_PAYLOAD: usize = 1200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub seq: u16,
    pub ts: u32,
}

impl Header {
    pub fn write(&self, out: &mut [u8]) {
        out[0] = VERSION;
        out[1..3].copy_from_slice(&self.seq.to_be_bytes());
        out[3..7].copy_from_slice(&self.ts.to_be_bytes());
    }

    /// Splits a packet into its header and Opus payload.
    pub fn parse(packet: &[u8]) -> Result<(Self, &[u8]), PacketError> {
        if packet.len() <= HEADER_LEN {
            return Err(PacketError::TooShort(packet.len()));
        }
        if packet[0] != VERSION {
            return Err(PacketError::Version(packet[0]));
        }
        let payload = &packet[HEADER_LEN..];
        if payload.len() > MAX_PAYLOAD {
            return Err(PacketError::TooLong(packet.len()));
        }
        let header = Self {
            seq: u16::from_be_bytes([packet[1], packet[2]]),
            ts: u32::from_be_bytes([packet[3], packet[4], packet[5], packet[6]]),
        };
        Ok((header, payload))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketError {
    TooShort(usize),
    TooLong(usize),
    Version(u8),
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "a {n}-byte audio packet is too short"),
            Self::TooLong(n) => write!(f, "a {n}-byte audio packet is too long"),
            Self::Version(v) => write!(
                f,
                "audio packet format {v}, but this agent uses {VERSION}; update crossglide on \
                 both machines"
            ),
        }
    }
}

impl std::error::Error for PacketError {}

/// How far `a` is ahead of `b` in wrapping sequence-number order; negative if behind.
pub fn seq_diff(a: u16, b: u16) -> i32 {
    i32::from(a.wrapping_sub(b) as i16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = Header {
            seq: 0xBEEF,
            ts: 0xDEAD_1234,
        };
        let mut packet = [0u8; HEADER_LEN + 3];
        header.write(&mut packet);
        packet[HEADER_LEN..].copy_from_slice(b"abc");
        assert_eq!(Header::parse(&packet), Ok((header, &b"abc"[..])));
    }

    #[test]
    fn bad_packets_are_refused() {
        assert_eq!(Header::parse(&[VERSION; 7]), Err(PacketError::TooShort(7)));
        let mut packet = [0u8; 10];
        packet[0] = VERSION + 1;
        assert_eq!(
            Header::parse(&packet),
            Err(PacketError::Version(VERSION + 1))
        );
        let mut packet = vec![0u8; HEADER_LEN + MAX_PAYLOAD + 1];
        packet[0] = VERSION;
        assert!(matches!(
            Header::parse(&packet),
            Err(PacketError::TooLong(_))
        ));
    }

    #[test]
    fn sequence_numbers_wrap() {
        assert_eq!(seq_diff(1, 0), 1);
        assert_eq!(seq_diff(0, 65535), 1);
        assert_eq!(seq_diff(65535, 0), -1);
        assert_eq!(seq_diff(5, 5), 0);
    }
}
