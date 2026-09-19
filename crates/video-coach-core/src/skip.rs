//! Coalescing rapid skip presses into a small number of player seeks.
//!
//! **This is a pure state machine with no clock.** It never compares a time
//! against a window; it is driven entirely by "is a seek in flight?" plus three
//! events from the caller. `burst_window` is never compared against anything —
//! it is only handed *back* so the caller can arm its own debounce timer. The
//! Swift original accepts a `nowMonotonicSeconds` parameter in all three
//! methods and reads it in none of them; that dead parameter is dropped here,
//! and its absence is why these tests need no fake clock.
//!
//! The policy: the **first** press of any sequence seeks exact. No
//! coarse-then-refine for a single keypress — on long-GOP HEVC the coarse
//! landing visibly snaps to the keyframe before the target (e.g. +2 s for a
//! +3 s skip) and the burst-end settle then jumps the rest of the way, which
//! reads as a double-seek for one keypress. Follow-up presses during flight
//! only accumulate the target; the coarse seek is issued when the leading exact
//! *lands*. Once the user stops pressing, an exact seek settles the frame.

use std::time::Duration;

/// A seek for the caller to issue.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SeekParams {
    pub target_seconds: f64,
    /// `false` = keyframe-tolerant (cheap on long-GOP HEVC).
    /// `true` = exact-frame settle.
    pub exact: bool,
}

/// What the caller should do in response to an event.
///
/// The two fields are independent: a follow-up press returns a debounce re-arm
/// with **no** seek, and the burst-mode switch returns both at once.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SkipDecision {
    pub seek: Option<SeekParams>,
    pub arm_debounce: Option<Duration>,
}

impl SkipDecision {
    const NONE: SkipDecision = SkipDecision {
        seek: None,
        arm_debounce: None,
    };

    fn seek(target_seconds: f64, exact: bool) -> Self {
        SkipDecision {
            seek: Some(SeekParams {
                target_seconds,
                exact,
            }),
            arm_debounce: None,
        }
    }
}

/// Default burst window.
///
/// **Tuned against mpv + VideoToolbox on Apple Silicon.** It must be
/// re-measured against whichever Linux decoder the scan player ends up using
/// (see the Phase 2 gate); do not inherit it as settled truth.
pub const DEFAULT_BURST_WINDOW: Duration = Duration::from_millis(150);

/// What the player is doing right now.
///
/// One field rather than `Option<f64>` plus a `bool`: "exact" is only
/// meaningful while a seek is in flight, and the two-field form leaves a stale
/// flag behind whenever flight clears. This is the one file in the crate that
/// is a state machine, so an unrepresentable illegal state is worth the enum.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Flight {
    Idle,
    Exact(f64),
    Coarse(f64),
}

impl Flight {
    fn target(self) -> Option<f64> {
        match self {
            Flight::Idle => None,
            Flight::Exact(t) | Flight::Coarse(t) => Some(t),
        }
    }
}

#[derive(Debug)]
pub struct SkipCoordinator {
    burst_window: Duration,
    /// Accumulated user intent, if a burst is in progress.
    target: Option<f64>,
    flight: Flight,
    /// The debounce fired while a seek was in flight; settle when it lands.
    exact_pending: bool,
}

impl Default for SkipCoordinator {
    fn default() -> Self {
        Self::new(DEFAULT_BURST_WINDOW)
    }
}

impl SkipCoordinator {
    pub fn new(burst_window: Duration) -> Self {
        SkipCoordinator {
            burst_window,
            target: None,
            flight: Flight::Idle,
            exact_pending: false,
        }
    }

    /// The user pressed a skip key.
    ///
    /// Accumulates from the pending target if a burst is in progress, otherwise
    /// from the player's current position, and clamps to the clip.
    pub fn request_skip(
        &mut self,
        delta: f64,
        current_seconds: f64,
        clip_duration_seconds: f64,
    ) -> SkipDecision {
        let base = self.target.unwrap_or(current_seconds);
        // `.max(0.0)` on the upper bound is load-bearing: `f64::clamp` asserts
        // `min <= max`, so a negative or NaN `clip_duration_seconds` -- which
        // reaches here straight from a user-editable project.json with no
        // validation -- would panic on the first arrow-key press. Swift used
        // nested min/max and never trapped.
        let t = (base + delta).clamp(0.0, clip_duration_seconds.max(0.0));
        self.target = Some(t);
        self.exact_pending = false;

        if self.flight == Flight::Idle {
            // Leading press: seek exact directly so one keypress is one
            // frame-precise jump. `target` stays set so a follow-up press
            // during this seek's flight accumulates from it. No debounce —
            // there is nothing left to settle to.
            self.flight = Flight::Exact(t);
            return SkipDecision::seek(t, true);
        }

        // Follow-up during flight: accumulate and re-arm only. The coarse seek
        // is issued by `seek_completed` once the leading exact lands.
        SkipDecision {
            seek: None,
            arm_debounce: Some(self.burst_window),
        }
    }

    /// The in-flight seek finished.
    pub fn seek_completed(&mut self) -> SkipDecision {
        let landed = std::mem::replace(&mut self.flight, Flight::Idle);

        // (a) The debounce fired mid-flight: settle exact now.
        if self.exact_pending {
            self.exact_pending = false;
            if let Some(t) = self.target {
                self.flight = Flight::Exact(t);
                self.target = None;
                return SkipDecision::seek(t, true);
            }
            return SkipDecision::NONE;
        }

        match landed {
            // (b) The leading exact landed and a follow-up piled up a new
            // target: switch to burst mode — coarse seek plus a debounce
            // re-arm.
            Flight::Exact(_) => {
                if let Some(tgt) = self.target {
                    if Some(tgt) != landed.target() {
                        self.flight = Flight::Coarse(tgt);
                        return SkipDecision {
                            seek: Some(SeekParams {
                                target_seconds: tgt,
                                exact: false,
                            }),
                            arm_debounce: Some(self.burst_window),
                        };
                    }
                }
                // (c) Leading exact landed with nothing pending.
                self.target = None;
                SkipDecision::NONE
            }
            // (d) A coarse seek landed and the target moved on during its
            // flight: refire coarse. No re-arm — the press that moved the
            // target armed it.
            Flight::Coarse(_) => {
                if let Some(tgt) = self.target {
                    if Some(tgt) != landed.target() {
                        self.flight = Flight::Coarse(tgt);
                        return SkipDecision::seek(tgt, false);
                    }
                }
                SkipDecision::NONE
            }
            Flight::Idle => SkipDecision::NONE,
        }
    }

    /// The caller's burst-end debounce fired.
    pub fn burst_ended(&mut self) -> SkipDecision {
        if self.flight == Flight::Idle {
            if let Some(t) = self.target {
                self.flight = Flight::Exact(t);
                self.target = None;
                return SkipDecision::seek(t, true);
            }
        }
        // Only arm the settle when there is something to settle to, preserving
        // the invariant that `target` is Some whenever `exact_pending` is set.
        if self.flight != Flight::Idle && self.target.is_some() {
            self.exact_pending = true;
        }
        SkipDecision::NONE
    }

    /// Clear transient state when the active player swaps. `burst_window` is
    /// preserved.
    pub fn reset(&mut self) {
        self.target = None;
        self.flight = Flight::Idle;
        self.exact_pending = false;
    }
}
