//! Screen edges: where pushing the pointer moves control to the other machine, and where it
//! comes out. Both machines use the same geometry, on their own displays.

use serde::{Deserialize, Serialize};

use crate::frame::Frame;

/// A side of the screen: on the Mac, the side the PC sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }

    /// How much the movement `(dx, dy)` heads through this edge; negative when it heads away.
    pub fn toward(self, dx: f64, dy: f64) -> f64 {
        match self {
            Self::Left => -dx,
            Self::Right => dx,
            Self::Top => -dy,
            Self::Bottom => dy,
        }
    }
}

/// A display, in global screen coordinates with y growing downwards (both macOS's Quartz
/// coordinates and Windows' are).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn right(&self) -> f64 {
        self.x + self.w
    }

    fn bottom(&self) -> f64 {
        self.y + self.h
    }
}

/// The display the pointer at `(x, y)` is on.
pub fn display_at(displays: &[Rect], x: f64, y: f64) -> Option<Rect> {
    // The pointer stops a pixel short of a display's right and bottom sides.
    displays
        .iter()
        .find(|r| r.contains(x, y))
        .or_else(|| displays.iter().find(|r| r.contains(x - 1.0, y - 1.0)))
        .copied()
}

/// The strip along `edge` of `display`, `thickness` wide: where the Mac's hint goes.
pub fn strip_rect(display: Rect, edge: Edge, thickness: f64) -> Rect {
    match edge {
        Edge::Left => Rect {
            w: thickness,
            ..display
        },
        Edge::Right => Rect {
            x: display.right() - thickness,
            w: thickness,
            ..display
        },
        Edge::Top => Rect {
            h: thickness,
            ..display
        },
        Edge::Bottom => Rect {
            y: display.bottom() - thickness,
            h: thickness,
            ..display
        },
    }
}

/// If the pointer at `(x, y)` sits on `edge` of the whole desktop (no display continues past
/// it), how far along that edge, from 0 (left or top) to 1.
pub fn at_edge(displays: &[Rect], x: f64, y: f64, edge: Edge) -> Option<f64> {
    let r = &display_at(displays, x, y)?;
    let beyond = |x, y| displays.iter().any(|d| d.contains(x, y));
    let on = match edge {
        Edge::Left => x <= r.x + 0.5 && !beyond(r.x - 0.5, y),
        Edge::Right => x >= r.right() - 1.5 && !beyond(r.right() + 0.5, y),
        Edge::Top => y <= r.y + 0.5 && !beyond(x, r.y - 0.5),
        Edge::Bottom => y >= r.bottom() - 1.5 && !beyond(x, r.bottom() + 0.5),
    };
    on.then(|| match edge {
        Edge::Left | Edge::Right => ((y - r.y) / r.h).clamp(0.0, 1.0),
        Edge::Top | Edge::Bottom => ((x - r.x) / r.w).clamp(0.0, 1.0),
    })
}

/// Where the pointer comes in through `edge`, `along` the way: on the display furthest out on
/// that side, a couple of pixels in so it doesn't bounce straight back.
pub fn entry_point(displays: &[Rect], edge: Edge, along: f64) -> Option<(f64, f64)> {
    let outwards = |r: &Rect| match edge {
        Edge::Left => -r.x,
        Edge::Right => r.right(),
        Edge::Top => -r.y,
        Edge::Bottom => r.bottom(),
    };
    let r = displays
        .iter()
        .max_by(|a, b| outwards(a).total_cmp(&outwards(b)))?;
    let along = along.clamp(0.0, 1.0);
    let inset = 2.0;
    Some(match edge {
        Edge::Left => (r.x + inset, r.y + along * (r.h - 1.0)),
        Edge::Right => (r.right() - 1.0 - inset, r.y + along * (r.h - 1.0)),
        Edge::Top => (r.x + along * (r.w - 1.0), r.y + inset),
        Edge::Bottom => (r.x + along * (r.w - 1.0), r.bottom() - 1.0 - inset),
    })
}

/// How far a finger must keep sliding while the pointer is stuck on an edge, in fractions of
/// the trackpad's width (about 4 mm), before control goes back to the Mac.
const RETURN_PUSH: f64 = 0.035;
/// The trackpad's height over its width, so both directions are measured in widths.
const ASPECT: f64 = 74.1 / 121.9;

/// On the PC: notices a finger pushing the pointer through one of `edges`. The pointer can't
/// move past the edge, so the push shows only in the contacts, not in the pointer.
#[derive(Debug, Default)]
pub struct ReturnDetector {
    edges: Vec<Edge>,
    /// The single finger's slot and position in the previous frame.
    last: Option<(u8, u16, u16)>,
    pushed: f64,
}

impl ReturnDetector {
    pub fn new(edges: Vec<Edge>) -> Self {
        Self {
            edges,
            ..Self::default()
        }
    }

    /// Takes the next frame. `at_edge` says whether the pointer is on an edge now and how far
    /// along. Returns the edge and position once a push is long enough.
    pub fn update(
        &mut self,
        frame: &Frame,
        at_edge: impl Fn(Edge) -> Option<f64>,
    ) -> Option<(Edge, f64)> {
        // One finger moves the pointer; two or more scroll or swipe, which mustn't switch.
        let mut down = frame.contacts.iter().filter(|c| c.tip);
        let (Some(finger), None) = (down.next(), down.next()) else {
            self.last = None;
            self.pushed = 0.0;
            return None;
        };
        let previous = self.last.replace((finger.id, finger.x, finger.y));
        let Some((_, x, y)) = previous.filter(|p| p.0 == finger.id) else {
            self.pushed = 0.0;
            return None;
        };
        let scale = f64::from(u16::MAX);
        let dx = (f64::from(finger.x) - f64::from(x)) / scale;
        let dy = (f64::from(finger.y) - f64::from(y)) / scale * ASPECT;
        for &edge in &self.edges {
            let toward = edge.toward(dx, dy);
            if toward <= 0.0 {
                continue;
            }
            if let Some(along) = at_edge(edge) {
                self.pushed += toward;
                if self.pushed >= RETURN_PUSH {
                    self.pushed = 0.0;
                    self.last = None;
                    return Some((edge, along));
                }
                return None;
            }
        }
        self.pushed = 0.0;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Contact;

    /// A 1440 x 900 laptop screen with a 1920 x 1080 monitor to its right, tops aligned.
    fn desk() -> Vec<Rect> {
        vec![
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1440.0,
                h: 900.0,
            },
            Rect {
                x: 1440.0,
                y: 0.0,
                w: 1920.0,
                h: 1080.0,
            },
        ]
    }

    #[test]
    fn only_outer_edges_count() {
        let d = desk();
        assert_eq!(at_edge(&d, 0.0, 450.0, Edge::Left), Some(0.5));
        // The laptop's right side leads onto the monitor: not an edge.
        assert_eq!(at_edge(&d, 1439.0, 450.0, Edge::Right), None);
        assert_eq!(at_edge(&d, 3359.0, 540.0, Edge::Right), Some(0.5));
        assert_eq!(at_edge(&d, 1800.0, 1079.0, Edge::Bottom), Some(0.1875));
        // Below the laptop is outside, even though the monitor goes further down.
        assert!(at_edge(&d, 720.0, 899.0, Edge::Bottom).is_some());
        assert_eq!(at_edge(&d, 720.0, 450.0, Edge::Top), None);
        assert_eq!(at_edge(&d, 720.0, 0.0, Edge::Top), Some(0.5));
        assert_eq!(at_edge(&d, -50.0, 0.0, Edge::Top), None);
    }

    #[test]
    fn the_hint_strip_hugs_one_side_of_its_display() {
        let monitor = desk()[1];
        let strip = |edge| strip_rect(monitor, edge, 28.0);
        let (x, y, w, h) = (1440.0, 0.0, 1920.0, 1080.0);
        assert_eq!(strip(Edge::Left), Rect { x, y, w: 28.0, h });
        assert_eq!(
            strip(Edge::Right),
            Rect {
                x: x + w - 28.0,
                y,
                w: 28.0,
                h
            }
        );
        assert_eq!(strip(Edge::Top), Rect { x, y, w, h: 28.0 });
        assert_eq!(
            strip(Edge::Bottom),
            Rect {
                x,
                y: y + h - 28.0,
                w,
                h: 28.0
            }
        );
    }

    #[test]
    fn the_display_is_found_from_the_pointer() {
        let d = desk();
        assert_eq!(display_at(&d, 100.0, 100.0), Some(d[0]));
        // The monitor starts where the laptop ends.
        assert_eq!(display_at(&d, 1440.0, 100.0), Some(d[1]));
        // The pointer stops a pixel short of a display's right and bottom sides.
        assert_eq!(display_at(&d, 3360.0, 1080.0), Some(d[1]));
        assert_eq!(display_at(&d, -50.0, 100.0), None);
    }

    #[test]
    fn the_pointer_enters_on_the_outermost_display() {
        let d = desk();
        assert_eq!(entry_point(&d, Edge::Right, 0.5), Some((3357.0, 539.5)));
        assert_eq!(entry_point(&d, Edge::Left, 0.0), Some((2.0, 0.0)));
        assert_eq!(entry_point(&d, Edge::Bottom, 1.0), Some((3359.0, 1077.0)));
        assert_eq!(entry_point(&[], Edge::Top, 0.5), None);
    }

    fn one_finger(id: u8, x: u16) -> Frame {
        Frame {
            contacts: vec![Contact {
                id,
                tip: true,
                x,
                y: 30000,
            }],
            ..Frame::default()
        }
    }

    #[test]
    fn a_long_enough_push_on_the_edge_returns() {
        let mut detector = ReturnDetector::new(vec![Edge::Left]);
        let on_edge = |e| (e == Edge::Left).then_some(0.25);
        let mut x = 40000;
        assert_eq!(detector.update(&one_finger(0, x), on_edge), None);
        // 1% of the width per frame to the left: returns on the fourth step.
        for _ in 0..3 {
            x -= 655;
            assert_eq!(detector.update(&one_finger(0, x), on_edge), None);
        }
        x -= 655;
        assert_eq!(
            detector.update(&one_finger(0, x), on_edge),
            Some((Edge::Left, 0.25))
        );
    }

    #[test]
    fn pushes_away_from_the_edge_or_off_it_start_over() {
        let mut detector = ReturnDetector::new(vec![Edge::Left]);
        let on_edge = |_| Some(0.5);
        let off_edge = |_| None;
        detector.update(&one_finger(0, 40000), on_edge);
        detector.update(&one_finger(0, 39000), on_edge);
        detector.update(&one_finger(0, 38000), off_edge);
        detector.update(&one_finger(0, 37000), on_edge);
        assert_eq!(detector.update(&one_finger(0, 36000), on_edge), None);
        // Moving right, away from the left edge, never counts.
        for x in (40000..60000).step_by(2000) {
            assert_eq!(detector.update(&one_finger(0, x), on_edge), None);
        }
    }

    #[test]
    fn two_fingers_never_return() {
        let mut detector = ReturnDetector::new(vec![Edge::Left]);
        let on_edge = |_| Some(0.5);
        for x in (0..20).map(|i| 50000 - i * 2000) {
            let mut frame = one_finger(0, x);
            frame.contacts.push(Contact {
                id: 1,
                tip: true,
                x,
                y: 10000,
            });
            assert_eq!(detector.update(&frame, on_edge), None);
        }
    }
}
