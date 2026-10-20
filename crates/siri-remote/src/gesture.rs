//! Turns raw touchpad coordinates into swipes.
//!
//! The remote reports a stream of finger positions with pressure; a stroke
//! ends when pressure drops to zero. Comparing the first and last positions of
//! a stroke gives a direction, which is what a Control4 room actually wants
//! (UP/DOWN/LEFT/RIGHT) rather than absolute coordinates.

use crate::protocol::Finger;

/// Minimum travel before a stroke counts as a swipe. X and Y differ because
/// the remote's Y axis is markedly coarser than its X axis.
const MIN_TRAVEL_X: i32 = 18;
const MIN_TRAVEL_Y: i32 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swipe {
    Up,
    Down,
    Left,
    Right,
}

impl Swipe {
    pub fn as_str(&self) -> &'static str {
        match self {
            Swipe::Up => "SWIPE_UP",
            Swipe::Down => "SWIPE_DOWN",
            Swipe::Left => "SWIPE_LEFT",
            Swipe::Right => "SWIPE_RIGHT",
        }
    }
}

#[derive(Default)]
pub struct SwipeDetector {
    start: Option<Finger>,
    last: Option<Finger>,
}

impl SwipeDetector {
    /// Feed one touch report. Returns a swipe when a stroke completes.
    pub fn update(&mut self, fingers: &[Finger]) -> Option<Swipe> {
        // Two-finger gestures are click-like on this remote; ignore for swipes.
        let f = match fingers.first() {
            Some(f) if fingers.len() == 1 => *f,
            _ => {
                self.start = None;
                self.last = None;
                return None;
            }
        };

        if f.pressure == 0 {
            // Finger lifted: judge the stroke that just ended.
            let result = match (self.start, self.last) {
                (Some(a), Some(b)) => Self::classify(a, b),
                _ => None,
            };
            self.start = None;
            self.last = None;
            return result;
        }

        if self.start.is_none() {
            self.start = Some(f);
        }
        self.last = Some(f);
        None
    }

    fn classify(a: Finger, b: Finger) -> Option<Swipe> {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        // Whichever axis travelled proportionally further wins, so a diagonal
        // resolves to its dominant direction instead of firing twice.
        let ax = dx.abs();
        let ay = dy.abs();
        if ax >= MIN_TRAVEL_X && ax * MIN_TRAVEL_Y >= ay * MIN_TRAVEL_X {
            Some(if dx > 0 { Swipe::Right } else { Swipe::Left })
        } else if ay >= MIN_TRAVEL_Y {
            Some(if dy > 0 { Swipe::Up } else { Swipe::Down })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: i32, y: i32, p: u8) -> Finger {
        Finger { x, y, pressure: p }
    }

    fn stroke(points: &[(i32, i32, u8)]) -> Option<Swipe> {
        let mut d = SwipeDetector::default();
        let mut out = None;
        for &(x, y, p) in points {
            if let Some(s) = d.update(&[f(x, y, p)]) {
                out = Some(s);
            }
        }
        out
    }

    #[test]
    fn detects_a_right_swipe() {
        assert_eq!(
            stroke(&[(10, 20, 30), (40, 22, 28), (70, 21, 25), (70, 21, 0)]),
            Some(Swipe::Right)
        );
    }

    #[test]
    fn detects_a_left_swipe() {
        assert_eq!(
            stroke(&[(90, 20, 30), (50, 21, 28), (10, 20, 25), (10, 20, 0)]),
            Some(Swipe::Left)
        );
    }

    #[test]
    fn detects_vertical_swipes() {
        assert_eq!(
            stroke(&[(50, 5, 30), (50, 30, 28), (50, 50, 25), (50, 50, 0)]),
            Some(Swipe::Up)
        );
        assert_eq!(
            stroke(&[(50, 55, 30), (50, 30, 28), (50, 5, 25), (50, 5, 0)]),
            Some(Swipe::Down)
        );
    }

    #[test]
    fn a_tap_is_not_a_swipe() {
        assert_eq!(stroke(&[(50, 40, 30), (51, 40, 28), (51, 40, 0)]), None);
    }

    #[test]
    fn diagonal_resolves_to_one_direction() {
        // Travels far in x, a little in y: should be horizontal only.
        let s = stroke(&[(10, 20, 30), (80, 32, 28), (80, 32, 0)]);
        assert_eq!(s, Some(Swipe::Right));
    }

    #[test]
    fn two_fingers_do_not_produce_swipes() {
        let mut d = SwipeDetector::default();
        assert_eq!(d.update(&[f(10, 20, 30), f(60, 20, 30)]), None);
        assert_eq!(d.update(&[f(80, 20, 0), f(90, 20, 0)]), None);
    }

    #[test]
    fn incomplete_stroke_yields_nothing() {
        let mut d = SwipeDetector::default();
        assert_eq!(d.update(&[f(10, 20, 30)]), None);
        assert_eq!(d.update(&[f(90, 20, 30)]), None);
    }
}
