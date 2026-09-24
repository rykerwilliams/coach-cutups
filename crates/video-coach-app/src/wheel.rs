//! The wheel over the transport scrubber: scroll deltas in, skips out.
//!
//! **A wheel skips, it doesn't seek.** The playhead moves through
//! `Command::Skip` — the arrow keys' own command — and so through core's
//! [`SkipCoordinator`](video_coach_core::skip::SkipCoordinator), because a
//! flick of the wheel is a burst exactly like a held arrow key and the
//! coordinator's debounce and anchoring are what stop a burst walking
//! backwards. It is also what lets the wheel work mid-take, where the
//! scrubber's own drag is refused.
//!
//! **Notches, not pixels.** A mouse reports one [`NOTCH`] at a time; a
//! trackpad reports a stream of a few pixels each. Skipping by a fraction of a
//! notch would hand the coordinator dozens of commands per flick — and, while
//! recording, log every one of them in the commentary. So the deltas are added
//! up here and a skip leaves only on a whole notch.
//!
//! **Sign** is [`crate::zoom_input`]'s throughout: a positive delta means the
//! content moves right or down, which on a timeline moves the playhead
//! **back**. Scrolling down, or swiping the strip to the left, goes forward.

/// One wheel notch, logical pixels: Slint reports winit's line deltas × 60.
pub const NOTCH: f64 = 60.0;

/// What one notch is worth, seconds: an arrow key's skip...
pub const STEP: f64 = 3.0;
/// ... and Shift's.
pub const SHIFT_STEP: f64 = 10.0;

/// The wheel part-way to its next notch.
#[derive(Debug, Default)]
pub struct Wheel {
    /// Scrolled but not yet spent, logical pixels. Always short of a notch.
    travel: f64,
}

impl Wheel {
    /// The seconds to skip for a scroll of `(dx, dy)` over the scrubber, or
    /// `None` when it hasn't yet added up to a notch.
    ///
    /// The axis with the larger travel wins, so a diagonal swipe doesn't
    /// cancel itself out; a horizontal swipe on a timeline means what a
    /// vertical wheel does. Non-finite deltas do nothing (BACKLOG #28).
    pub fn scrolled(&mut self, dx: f64, dy: f64, shift: bool) -> Option<f64> {
        let delta = if dx.abs() > dy.abs() { dx } else { dy };
        if !delta.is_finite() || delta == 0.0 {
            return None;
        }
        // A flick the other way starts afresh: what the last one left over
        // must not eat the first notch of this one.
        if self.travel.is_sign_negative() != delta.is_sign_negative() {
            self.travel = 0.0;
        }
        self.travel += delta;
        let notches = (self.travel / NOTCH).trunc();
        if notches == 0.0 {
            return None;
        }
        self.travel -= notches * NOTCH;
        Some(-notches * if shift { SHIFT_STEP } else { STEP })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One notch of a mouse wheel is one arrow key: scrolling down (a
    /// negative delta) goes forward, as the sign convention has it, and
    /// Shift makes it the long skip.
    #[test]
    fn a_notch_is_worth_an_arrow_key() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(0.0, -NOTCH, false), Some(STEP));
        assert_eq!(wheel.scrolled(0.0, NOTCH, false), Some(-STEP));
        assert_eq!(wheel.scrolled(0.0, -NOTCH, true), Some(SHIFT_STEP));
    }

    /// A trackpad's fine deltas add up to one notch and no more: eleven
    /// sixths of a notch is one skip with five sixths left over.
    #[test]
    fn fine_deltas_add_up_to_whole_notches() {
        let mut wheel = Wheel::default();
        let sixth = -NOTCH / 6.0;
        for _ in 0..5 {
            assert_eq!(wheel.scrolled(0.0, sixth, false), None);
        }
        assert_eq!(wheel.scrolled(0.0, sixth, false), Some(STEP));
        for _ in 0..5 {
            assert_eq!(wheel.scrolled(0.0, sixth, false), None);
        }
    }

    /// A fling worth several notches skips by all of them at once rather than
    /// dropping the rest.
    #[test]
    fn a_fling_spends_every_notch_it_carries() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(0.0, -3.5 * NOTCH, false), Some(3.0 * STEP));
        // Half a notch is still in hand, so the next half completes one.
        assert_eq!(wheel.scrolled(0.0, -0.5 * NOTCH, false), Some(STEP));
    }

    /// Turning round starts afresh: half a notch forward left over must not
    /// eat the first notch of a scroll the other way.
    #[test]
    fn turning_round_drops_what_was_left_over() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(0.0, -0.5 * NOTCH, false), None);
        assert_eq!(wheel.scrolled(0.0, NOTCH, false), Some(-STEP));
    }

    /// A trackpad swipes horizontally; the bigger axis wins, so a diagonal
    /// doesn't cancel itself out.
    #[test]
    fn the_larger_axis_wins() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(-NOTCH, 10.0, false), Some(STEP));
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(10.0, -NOTCH, false), Some(STEP));
    }

    /// Nothing to spend, and nothing that could poison the running total.
    #[test]
    fn a_still_or_broken_wheel_skips_nothing() {
        let mut wheel = Wheel::default();
        assert_eq!(wheel.scrolled(0.0, 0.0, false), None);
        assert_eq!(wheel.scrolled(f64::NAN, f64::NAN, false), None);
        assert_eq!(wheel.scrolled(0.0, f64::INFINITY, false), None);
        assert_eq!(wheel.scrolled(0.0, -NOTCH, false), Some(STEP));
    }
}
