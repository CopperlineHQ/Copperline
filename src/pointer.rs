// SPDX-License-Identifier: GPL-3.0-or-later

//! Absolute pointer positioning by closed-loop servo of relative mouse
//! motion.
//!
//! The Amiga mouse has no absolute position to set: it is a quadrature
//! encoder, and the guest turns counts into pointer pixels through its own
//! acceleration curve. Intuition's is history-dependent, so open-loop
//! deltas cannot be aimed -- the same count sequence lands somewhere
//! different depending on what preceded it.
//!
//! What the machine does expose is where the pointer actually is: the
//! Amiga pointer is sprite 0, so the sprite's on-screen position is the
//! pointer's position, and reading it is the same observation a person
//! makes by looking at the screen. This servo injects a delta, watches
//! where sprite 0 moved to on the next frame, and corrects, learning the
//! pixels-per-count ratio it is being given instead of modelling it. That
//! keeps the whole mechanism hardware-derived: nothing here knows which
//! guest is running, only where the hardware is drawing.
//!
//! A guest that draws its pointer into a bitplane rather than a sprite has
//! nothing to observe, and the servo says so rather than moving blindly.

use crate::bus::Bus;

/// The sprite the Amiga pointer is drawn with.
const POINTER_SPRITE: usize = 0;

/// Largest quadrature count injected in one frame. The counters are 8-bit
/// and the guest reads them as a signed difference, so a step past 127
/// would be seen as motion the other way; 64 keeps a comfortable margin
/// under that and under any delta coalescing in the guest.
const MAX_COUNTS: i32 = 64;

/// Bounds on the learned pixels-per-count gain, so one anomalous frame
/// (an acceleration threshold crossing, a pointer clamped at a screen
/// edge) cannot produce a wild step.
const MIN_GAIN: f64 = 0.25;
const MAX_GAIN: f64 = 16.0;

/// Frames a servo spends chasing its target before giving up, by default
/// and at most. Sixty frames is a little over a second of emulated time,
/// where a pointer that can converge normally does so in a handful.
pub const DEFAULT_MAX_FRAMES: u32 = 60;
pub const FRAME_LIMIT: u32 = 600;

/// How close counts as arrived, by default. The pointer moves in lo-res
/// pixels, which are two columns of the presented canvas, so an exact
/// landing is not available for every target -- half of all horizontal
/// coordinates are simply not on the lattice the guest can put the
/// pointer on, and demanding one would fail on them forever. Two pixels
/// is that quantum, and comfortably inside any clickable widget.
pub const DEFAULT_TOLERANCE: i32 = 2;
pub const TOLERANCE_LIMIT: i32 = 64;

/// Consecutive moves allowed without beating the closest approach so far
/// before the servo calls the pointer stuck. Two covers an oscillation
/// between the lattice points either side of an unreachable target.
const STALL_LIMIT: u32 = 3;

/// What the servo wants to happen next.
#[derive(Debug, Clone, PartialEq)]
pub enum ServoStep {
    /// Apply this quadrature delta to `port`, advance exactly one frame,
    /// then poll again.
    Move { port: u8, dx: i32, dy: i32 },
    /// The pointer is on the target.
    Arrived { x: i32, y: i32, frames: u32 },
    /// The servo cannot get there; the string says why.
    Failed(String),
}

/// One in-flight "put the pointer here" request. Poll once per emulated
/// frame, applying whatever [`ServoStep::Move`] it asks for in between.
#[derive(Debug, Clone)]
pub struct PointerServo {
    port: u8,
    target: (i32, i32),
    tolerance: i32,
    max_frames: u32,
    frames: u32,
    /// Closest approach so far, and how many moves since it improved.
    /// A pointer whose reachable lattice straddles the target oscillates
    /// rather than converging, and has to be recognised as arrived-as-
    /// close-as-it-gets instead of running out the frame budget.
    best: Option<(i32, (i32, i32))>,
    stalled: u32,
    /// Presented pixels per mouse count, learned per axis. A lo-res
    /// pointer pixel is two columns of the 716-wide canvas, so horizontal
    /// motion starts at 2 and vertical at 1.
    gain: (f64, f64),
    /// Position and counts of the delta currently in flight, so the next
    /// poll can measure what the guest did with it.
    outstanding: Option<((i32, i32), (i32, i32))>,
}

impl PointerServo {
    pub fn new(port: u8, target: (i32, i32), tolerance: i32, max_frames: u32) -> Self {
        Self {
            port,
            target,
            tolerance: tolerance.clamp(0, TOLERANCE_LIMIT),
            max_frames: max_frames.clamp(1, FRAME_LIMIT),
            frames: 0,
            best: None,
            stalled: 0,
            gain: (2.0, 1.0),
            outstanding: None,
        }
    }

    pub fn target(&self) -> (i32, i32) {
        self.target
    }

    /// Read the pointer off the frame just completed and decide the next
    /// move.
    pub fn poll(&mut self, bus: &Bus) -> ServoStep {
        self.poll_at(pointer_position(bus))
    }

    /// [`poll`] with the observation supplied, so the control loop can be
    /// exercised without an emulator.
    ///
    /// [`poll`]: Self::poll
    fn poll_at(&mut self, at: Option<(i32, i32)>) -> ServoStep {
        let Some(at) = at else {
            return ServoStep::Failed(if self.frames == 0 {
                "sprite 0 is not being drawn: the guest is not using a hardware pointer, \
                 so there is nothing to servo"
                    .to_string()
            } else {
                format!(
                    "sprite 0 stopped being drawn after {} servo frame(s)",
                    self.frames
                )
            });
        };
        // Fold in what the delta already in flight actually achieved.
        if let Some((was, counts)) = self.outstanding.take() {
            if counts.0 != 0 && at.0 != was.0 {
                self.gain.0 = learn(self.gain.0, at.0 - was.0, counts.0);
            }
            if counts.1 != 0 && at.1 != was.1 {
                self.gain.1 = learn(self.gain.1, at.1 - was.1, counts.1);
            }
        }
        let error = (self.target.0 - at.0, self.target.1 - at.1);
        let miss = error.0.abs().max(error.1.abs());
        if miss <= self.tolerance {
            return ServoStep::Arrived {
                x: at.0,
                y: at.1,
                frames: self.frames,
            };
        }
        // Track how close the pointer has been brought. A target that is
        // not on the guest's reachable lattice makes the servo oscillate
        // around it; that is a real answer ("this is as close as the
        // pointer goes"), not something to spend the frame budget on.
        match self.best {
            Some((best, _)) if miss >= best => self.stalled += 1,
            _ => {
                self.best = Some((miss, at));
                self.stalled = 0;
            }
        }
        if self.stalled >= STALL_LIMIT {
            let (miss, at) = self.best.expect("a stall implies a recorded best");
            return ServoStep::Failed(format!(
                "pointer cannot reach ({}, {}): it settles {miss} px away at ({}, {}), \
                 which is outside the {} px tolerance",
                self.target.0, self.target.1, at.0, at.1, self.tolerance,
            ));
        }
        if self.frames >= self.max_frames {
            return ServoStep::Failed(format!(
                "pointer did not reach ({}, {}) in {} frame(s): stopped at ({}, {}), \
                 {} px short horizontally and {} px vertically",
                self.target.0, self.target.1, self.max_frames, at.0, at.1, error.0, error.1,
            ));
        }
        let counts = (
            counts_for(error.0, self.gain.0),
            counts_for(error.1, self.gain.1),
        );
        self.outstanding = Some((at, counts));
        self.frames += 1;
        ServoStep::Move {
            port: self.port,
            dx: counts.0,
            dy: counts.1,
        }
    }
}

/// Where the guest's pointer is on the presented canvas, in the same
/// coordinates `capture.screenshot` writes out.
pub fn pointer_position(bus: &Bus) -> Option<(i32, i32)> {
    crate::video::bitplane::sprite_framebuffer_origin(bus, POINTER_SPRITE)
}

/// Counts to request for a pixel error at the measured gain, bounded by
/// one frame's safe step.
///
/// The gain already expresses the overshoot guard -- error/gain is the
/// number of counts that covers exactly this error -- so there is no
/// second clamp in pixels. Clamping counts by the pixel error would be a
/// unit confusion, and it throttles the servo badly whenever the guest
/// gives less than one pixel per count.
///
/// A non-zero error always asks for at least one count. A count is the
/// smallest motion available, so requesting none would burn a frame
/// injecting nothing and let the stall detector call a reachable target
/// stuck; if one count overshoots, the tolerance and the stall detector
/// are what resolve it.
fn counts_for(error: i32, gain: f64) -> i32 {
    if error == 0 {
        return 0;
    }
    let wanted = (f64::from(error.abs()) / gain).round() as i32;
    wanted.clamp(1, MAX_COUNTS) * error.signum()
}

/// Blend a fresh pixels-per-count measurement into the running gain. The
/// average damps a single odd frame without slowing convergence.
fn learn(gain: f64, moved: i32, counts: i32) -> f64 {
    let measured = f64::from(moved) / f64::from(counts);
    if measured <= 0.0 {
        // The pointer went the other way (a clamp at a screen edge, or an
        // axis the guest inverts); keep the estimate rather than flipping
        // its sign and chasing away from the target.
        return gain;
    }
    ((gain + measured) / 2.0).clamp(MIN_GAIN, MAX_GAIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a servo against a synthetic pointer that applies a fixed
    /// pixels-per-count ratio, to check the loop converges without being
    /// told what that ratio is.
    fn converge(
        ratio: (f64, f64),
        start: (i32, i32),
        target: (i32, i32),
        tolerance: i32,
    ) -> (u32, (i32, i32)) {
        let mut servo = PointerServo::new(0, target, tolerance, DEFAULT_MAX_FRAMES);
        let mut at = start;
        for _ in 0..DEFAULT_MAX_FRAMES + 2 {
            // Stand in for the poll's bus read.
            let step = servo.poll_at(Some(at));
            match step {
                ServoStep::Move { dx, dy, .. } => {
                    at.0 += (f64::from(dx) * ratio.0).round() as i32;
                    at.1 += (f64::from(dy) * ratio.1).round() as i32;
                }
                ServoStep::Arrived { frames, .. } => return (frames, at),
                ServoStep::Failed(why) => panic!("servo gave up: {why}"),
            }
        }
        panic!("servo never terminated");
    }

    #[test]
    fn converges_at_the_gain_it_was_built_for() {
        let (frames, at) = converge((2.0, 1.0), (0, 0), (300, 120), 0);
        assert_eq!(at, (300, 120));
        assert!(frames <= 6, "took {frames} frames");
    }

    #[test]
    fn converges_when_the_guest_accelerates_more_than_expected() {
        // Four presented pixels per count on both axes: the servo must
        // learn the ratio rather than overshoot forever.
        let (frames, at) = converge((4.0, 4.0), (600, 250), (40, 12), DEFAULT_TOLERANCE);
        assert!(
            (at.0 - 40).abs() <= DEFAULT_TOLERANCE && (at.1 - 12).abs() <= DEFAULT_TOLERANCE,
            "landed at {at:?}"
        );
        assert!(frames <= 10, "took {frames} frames");
    }

    #[test]
    fn converges_when_the_guest_moves_less_than_one_pixel_per_count() {
        let (frames, at) = converge((1.0, 1.0), (10, 10), (200, 90), 0);
        assert_eq!(at, (200, 90));
        assert!(frames <= 8, "took {frames} frames");
    }

    #[test]
    fn gives_up_when_the_pointer_will_not_move() {
        let mut servo = PointerServo::new(0, (400, 100), 0, 8);
        let mut steps = 0;
        loop {
            match servo.poll_at(Some((10, 10))) {
                ServoStep::Move { .. } => {
                    steps += 1;
                    assert!(steps <= 8, "servo ran past its frame budget");
                }
                ServoStep::Failed(why) => {
                    assert!(why.contains("cannot reach (400, 100)"), "{why}");
                    assert!(why.contains("settles 390 px away at (10, 10)"), "{why}");
                    return;
                }
                ServoStep::Arrived { .. } => panic!("a stuck pointer must not report arrival"),
            }
        }
    }

    #[test]
    fn a_target_the_pointer_is_already_on_needs_no_motion() {
        let mut servo = PointerServo::new(0, (128, 64), 0, DEFAULT_MAX_FRAMES);
        assert_eq!(
            servo.poll_at(Some((128, 64))),
            ServoStep::Arrived {
                x: 128,
                y: 64,
                frames: 0
            }
        );
    }

    #[test]
    fn a_sub_gain_error_still_asks_for_motion() {
        // Four pixels per count with one pixel to go: asking for zero
        // would burn a frame injecting nothing.
        assert_eq!(counts_for(1, 4.0), 1);
        assert_eq!(counts_for(-1, 4.0), -1);
        // A quarter of a pixel per count needs four counts per pixel; the
        // request must not be throttled to the pixel error.
        assert_eq!(counts_for(100, 0.25), MAX_COUNTS);
        assert_eq!(counts_for(4, 0.25), 16);
        assert_eq!(counts_for(0, 2.0), 0);
    }

    #[test]
    fn a_guest_moving_less_than_a_pixel_per_count_still_converges() {
        let (frames, at) = converge((0.25, 0.25), (0, 0), (200, 100), 0);
        assert_eq!(at, (200, 100));
        assert!(frames <= 20, "took {frames} frames");
    }

    #[test]
    fn learned_gain_stays_inside_its_bounds() {
        assert_eq!(learn(2.0, 1000, 1), MAX_GAIN);
        assert_eq!(learn(0.3, 1, 1000), MIN_GAIN);
        // A move against the requested direction leaves the estimate be.
        assert_eq!(learn(2.0, -8, 4), 2.0);
    }
}
