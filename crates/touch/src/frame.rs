//! Contact frames: the fingers on the Mac's trackpad at one instant, sent from the Mac to the
//! PC as one QUIC datagram each. A frame carries every contact, not a change, so a lost frame
//! costs nothing but its own moment; the PC lifts all fingers itself if frames stop.
//!
//! | Field    | Size    | Notes                                                  |
//! | -------- | ------- | ------------------------------------------------------ |
//! | Magic    | 1 byte  | `T`; audio packets start with their version, 1         |
//! | Version  | 1 byte  | Rejects mismatched builds                              |
//! | Sequence | 2 bytes | Big-endian, wraps; drops frames that arrive late       |
//! | Time     | 4 bytes | Big-endian, in 100 µs on the Mac's clock, wraps        |
//! | Button   | 1 byte  | 1 while the trackpad is clicked                        |
//! | Count    | 1 byte  | Contacts that follow, at most [`MAX_CONTACTS`]         |
//! | Contacts | 6 each  | Slot, flags (bit 0: touching), x and y (big-endian)    |
//!
//! Positions are fractions of the trackpad's width and height in 1/65535, from its top left.

use std::fmt;

pub const MAGIC: u8 = b'T';
pub const VERSION: u8 = 1;
/// Contacts a frame (and the virtual touchpad) carries; Windows needs at most 5 for gestures.
pub const MAX_CONTACTS: usize = 5;

const HEADER_LEN: usize = 10;
const CONTACT_LEN: usize = 6;
const TIP: u8 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    /// Slot, `0..MAX_CONTACTS`, stable while the finger stays down.
    pub id: u8,
    /// Touching the surface; false once, in the frame where the finger lifts.
    pub tip: bool,
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub seq: u16,
    /// In 100 µs; only differences between frames matter.
    pub time: u32,
    pub button: bool,
    pub contacts: Vec<Contact>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let count = self.contacts.len().min(MAX_CONTACTS);
        let mut out = Vec::with_capacity(HEADER_LEN + count * CONTACT_LEN);
        out.extend_from_slice(&[MAGIC, VERSION]);
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.time.to_be_bytes());
        out.extend_from_slice(&[u8::from(self.button), count as u8]);
        for c in &self.contacts[..count] {
            out.extend_from_slice(&[c.id, if c.tip { TIP } else { 0 }]);
            out.extend_from_slice(&c.x.to_be_bytes());
            out.extend_from_slice(&c.y.to_be_bytes());
        }
        out
    }

    pub fn decode(packet: &[u8]) -> Result<Self, FrameError> {
        if packet.len() < HEADER_LEN {
            return Err(FrameError::TooShort(packet.len()));
        }
        if packet[0] != MAGIC {
            return Err(FrameError::NotAFrame(packet[0]));
        }
        if packet[1] != VERSION {
            return Err(FrameError::Version(packet[1]));
        }
        let count = packet[9] as usize;
        if count > MAX_CONTACTS || packet.len() != HEADER_LEN + count * CONTACT_LEN {
            return Err(FrameError::Length(packet.len()));
        }
        let contacts = packet[HEADER_LEN..]
            .as_chunks::<CONTACT_LEN>()
            .0
            .iter()
            .map(|c| Contact {
                id: c[0],
                tip: c[1] & TIP != 0,
                x: u16::from_be_bytes([c[2], c[3]]),
                y: u16::from_be_bytes([c[4], c[5]]),
            })
            .collect();
        Ok(Self {
            seq: u16::from_be_bytes([packet[2], packet[3]]),
            time: u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]),
            button: packet[8] != 0,
            contacts,
        })
    }

    /// Whether any finger is on the surface.
    pub fn touching(&self) -> bool {
        self.contacts.iter().any(|c| c.tip)
    }

    /// This frame with every finger lifted and the button up: what the PC reports when contact
    /// with the Mac ends mid-touch.
    pub fn lifted(&self) -> Self {
        Self {
            seq: self.seq,
            time: self.time,
            button: false,
            contacts: self
                .contacts
                .iter()
                .filter(|c| c.tip)
                .map(|c| Contact { tip: false, ..*c })
                .collect(),
        }
    }
}

/// Whether sequence number `seq` comes after `last`, allowing for wrap-around.
pub fn is_newer(seq: u16, last: u16) -> bool {
    (seq.wrapping_sub(last) as i16) > 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    TooShort(usize),
    NotAFrame(u8),
    Version(u8),
    Length(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::TooShort(n) => write!(f, "a {n}-byte datagram is too short for a touch frame"),
            Self::NotAFrame(b) => write!(f, "datagram starting with {b:#04x} isn't a touch frame"),
            Self::Version(v) => write!(
                f,
                "touch frame version {v}, but this agent speaks {VERSION}; update crossglide on \
                 both machines"
            ),
            Self::Length(n) => write!(f, "touch frame of {n} bytes doesn't match its count"),
        }
    }
}

impl std::error::Error for FrameError {}

/// A finger as the trackpad reports it: its own identifier, and position from the top left in
/// fractions of the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finger {
    pub identifier: i32,
    pub touching: bool,
    pub x: f32,
    pub y: f32,
}

/// Gives fingers stable slots `0..MAX_CONTACTS` and reports each lift once.
#[derive(Debug, Default)]
pub struct Tracker {
    /// The trackpad identifier holding each slot.
    slots: [Option<i32>; MAX_CONTACTS],
}

impl Tracker {
    /// The contacts for one trackpad frame, or `None` when there's nothing to report (no finger
    /// down now and none just lifted). A sixth finger is ignored until a slot frees up.
    pub fn update(&mut self, fingers: &[Finger]) -> Option<Vec<Contact>> {
        let mut contacts = Vec::new();
        let mut freed = [false; MAX_CONTACTS];
        // Lifts first: a finger that stopped touching or vanished frees its slot.
        for (slot, held) in self.slots.iter_mut().enumerate() {
            let Some(identifier) = *held else { continue };
            let finger = fingers.iter().find(|f| f.identifier == identifier);
            if !finger.is_some_and(|f| f.touching) {
                let (x, y) = finger.map_or((0, 0), |f| (fraction(f.x), fraction(f.y)));
                contacts.push(Contact {
                    id: slot as u8,
                    tip: false,
                    x,
                    y,
                });
                *held = None;
                freed[slot] = true;
            }
        }
        for finger in fingers.iter().filter(|f| f.touching) {
            let slot = match self
                .slots
                .iter()
                .position(|s| *s == Some(finger.identifier))
            {
                Some(slot) => slot,
                // A slot freed just now would make Windows see the lifted finger jump here.
                None => match (0..MAX_CONTACTS)
                    .find(|&s| self.slots[s].is_none() && !freed[s])
                    .or_else(|| self.slots.iter().position(Option::is_none))
                {
                    Some(free) => {
                        self.slots[free] = Some(finger.identifier);
                        free
                    }
                    None => continue,
                },
            };
            contacts.push(Contact {
                id: slot as u8,
                tip: true,
                x: fraction(finger.x),
                y: fraction(finger.y),
            });
        }
        // With every other slot taken, a new finger can still land in one freed above; it would
        // appear twice, so the new finger wins.
        let mut seen = [false; MAX_CONTACTS];
        for c in contacts.iter().rev() {
            seen[c.id as usize] |= c.tip;
        }
        contacts.retain(|c| c.tip || !seen[c.id as usize]);
        contacts.sort_by_key(|c| c.id);
        (!contacts.is_empty()).then_some(contacts)
    }
}

fn fraction(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finger(identifier: i32, touching: bool, x: f32) -> Finger {
        Finger {
            identifier,
            touching,
            x,
            y: 0.5,
        }
    }

    fn slots(contacts: &[Contact]) -> Vec<(u8, bool)> {
        contacts.iter().map(|c| (c.id, c.tip)).collect()
    }

    #[test]
    fn frames_round_trip() {
        let frame = Frame {
            seq: 65535,
            time: 0x0102_0304,
            button: true,
            contacts: vec![
                Contact {
                    id: 0,
                    tip: true,
                    x: 1,
                    y: 65535,
                },
                Contact {
                    id: 4,
                    tip: false,
                    x: 30000,
                    y: 2,
                },
            ],
        };
        let packet = frame.encode();
        assert_eq!(packet.len(), 10 + 2 * 6);
        assert_eq!(Frame::decode(&packet), Ok(frame));
    }

    #[test]
    fn bad_packets_are_refused() {
        let good = Frame::default().encode();
        assert_eq!(Frame::decode(&good[..5]), Err(FrameError::TooShort(5)));
        assert_eq!(Frame::decode(&[1; 20]), Err(FrameError::NotAFrame(1)));
        let mut version = good.clone();
        version[1] = 9;
        assert_eq!(Frame::decode(&version), Err(FrameError::Version(9)));
        let mut count = good.clone();
        count[9] = 1;
        assert_eq!(Frame::decode(&count), Err(FrameError::Length(10)));
    }

    #[test]
    fn sequence_numbers_wrap() {
        assert!(is_newer(1, 0));
        assert!(is_newer(0, 65535));
        assert!(!is_newer(65535, 0));
        assert!(!is_newer(7, 7));
    }

    #[test]
    fn fingers_keep_their_slots_and_lift_once() {
        let mut tracker = Tracker::default();
        assert_eq!(tracker.update(&[]), None);

        let two = tracker
            .update(&[finger(40, true, 0.0), finger(41, true, 1.0)])
            .unwrap();
        assert_eq!(slots(&two), [(0, true), (1, true)]);
        assert_eq!(two[1].x, 65535);

        // The first finger lifts: reported once with tip off, and the second keeps slot 1.
        let lift = tracker
            .update(&[finger(40, false, 0.1), finger(41, true, 0.9)])
            .unwrap();
        assert_eq!(slots(&lift), [(0, false), (1, true)]);
        let after = tracker.update(&[finger(41, true, 0.8)]).unwrap();
        assert_eq!(slots(&after), [(1, true)]);

        // A new finger takes the free slot 0; then everything vanishes at once.
        let new = tracker
            .update(&[finger(42, true, 0.5), finger(41, true, 0.8)])
            .unwrap();
        assert_eq!(slots(&new), [(0, true), (1, true)]);
        let gone = tracker.update(&[]).unwrap();
        assert_eq!(slots(&gone), [(0, false), (1, false)]);
        assert_eq!(tracker.update(&[]), None);
    }

    #[test]
    fn a_new_finger_avoids_a_slot_freed_in_the_same_frame() {
        let mut tracker = Tracker::default();
        tracker.update(&[finger(1, true, 0.5)]);
        let swapped = tracker.update(&[finger(2, true, 0.5)]).unwrap();
        assert_eq!(slots(&swapped), [(0, false), (1, true)]);

        // With no other slot free, the freed one is reused and reported once.
        let mut full = Tracker::default();
        let five: Vec<_> = (0..5).map(|i| finger(i, true, 0.5)).collect();
        full.update(&five);
        let mut next = five.clone();
        next[0] = finger(9, true, 0.5);
        let reused = full.update(&next).unwrap();
        assert_eq!(reused.len(), MAX_CONTACTS);
        assert!(reused.iter().all(|c| c.tip));
    }

    #[test]
    fn a_sixth_finger_waits_for_a_slot() {
        let mut tracker = Tracker::default();
        let six: Vec<_> = (0..6).map(|i| finger(i, true, 0.5)).collect();
        assert_eq!(tracker.update(&six).unwrap().len(), MAX_CONTACTS);
    }

    #[test]
    fn lifted_keeps_only_fingers_that_were_down() {
        let frame = Frame {
            button: true,
            contacts: vec![
                Contact {
                    id: 0,
                    tip: true,
                    x: 5,
                    y: 6,
                },
                Contact {
                    id: 1,
                    tip: false,
                    x: 7,
                    y: 8,
                },
            ],
            ..Frame::default()
        };
        let lifted = frame.lifted();
        assert!(!lifted.button && !lifted.touching());
        assert_eq!(
            lifted.contacts,
            [Contact {
                id: 0,
                tip: false,
                x: 5,
                y: 6
            }]
        );
    }
}
