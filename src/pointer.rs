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
//! What the machine does expose is where the pointer actually is. The
//! Amiga has no hardware pointer; Intuition draws one with sprite 0, and
//! effectively every guest follows that convention, so the sprite's
//! on-screen position is the pointer's position and reading it is the
//! same observation a person makes by looking at the screen. This servo
//! injects a delta, watches where sprite 0 moved to once the guest has
//! answered it, and corrects, learning the pixels-per-count ratio it is
//! being given instead of modelling it. That keeps the whole mechanism
//! hardware-derived: nothing here knows which guest is running, only where
//! the hardware is drawing.
//!
//! The servo counts displayed fields, not polls. The mouse counters are
//! free-running, and software samples them -- conventionally once per
//! vertical blank -- then rewrites the pointer sprite's control words,
//! which sprite DMA picks up in the field that follows. Counts injected
//! while field N is being drawn therefore reach the display in field N+1
//! at the earliest, and the observation reads a completed field, so the
//! answer is only visible after two field wraps. Callers do not poll on
//! field boundaries (the frame loop runs an instruction budget, not a
//! field), so a poll that finds no newly completed field, or no answer yet
//! to the counts in flight, is told to wait rather than read as a pointer
//! that refused to move.
//!
//! A guest that draws its pointer into a bitplane rather than a sprite has
//! nothing to observe, and the servo says so rather than moving blindly.

use crate::bus::Bus;

/// The sprite the OS draws its pointer with. A convention, not hardware:
/// a guest is free to draw a pointer any way it likes, and one that does
/// is reported as unobservable rather than guessed at.
const POINTER_SPRITE: usize = 0;

/// Largest quadrature count injected between two samples of the counters.
/// The counters are 8-bit and the guest reads them as a signed
/// difference, so a step past 127 would be seen as motion the other way;
/// 64 keeps a comfortable margin under that and under any delta
/// coalescing in the guest. The servo never has more than one delta in
/// flight, so this is also the most the guest can be handed per sample.
const MAX_COUNTS: i32 = 64;

/// Fields to wait for the guest to answer a delta before reading its
/// silence as a pointer that did not move. The earliest answer is
/// readable two field wraps after the counts go in (see the module notes);
/// the rest is slack for a guest whose input handling draws its answer up
/// to three fields behind its vertical-blank interrupt.
const RESPONSE_FIELDS: u64 = 5;

/// Bounds on the learned pixels-per-count gain, so one anomalous frame
/// (an acceleration threshold crossing, a pointer clamped at a screen
/// edge) cannot produce a wild step.
const MIN_GAIN: f64 = 0.25;
const MAX_GAIN: f64 = 16.0;

/// Displayed fields a servo spends chasing its target before giving up,
/// by default and at most. Sixty fields is a little over a second of
/// emulated time, where a pointer that can converge normally does so in a
/// handful of corrections of two fields each.
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

/// Consecutive answered moves allowed without beating the closest
/// approach so far before the servo calls the pointer stuck. Two covers
/// an oscillation between the lattice points either side of an
/// unreachable target.
const STALL_LIMIT: u32 = 3;

/// What the servo wants to happen next.
#[derive(Debug, Clone, PartialEq)]
pub enum ServoStep {
    /// Apply this quadrature delta to `port`, keep the machine running,
    /// then poll again.
    Move { port: u8, dx: i32, dy: i32 },
    /// Inject nothing: keep the machine running and poll again. No field
    /// has completed since the last poll, or the guest has not displayed
    /// its answer to the delta in flight yet.
    Wait,
    /// The pointer is on the target.
    Arrived { x: i32, y: i32, frames: u32 },
    /// The servo cannot get there; the string says why.
    Failed(String),
}

/// A delta the guest has been handed but not yet been seen to answer.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    /// Where the pointer was drawn when the counts went in.
    from: (i32, i32),
    counts: (i32, i32),
    /// Fields completed when the counts went in.
    field: u64,
}

/// One in-flight "put the pointer here" request. Poll it as often as
/// convenient while the machine runs -- every pass of the frame loop is
/// fine -- applying whatever [`ServoStep::Move`] it asks for.
#[derive(Debug, Clone)]
pub struct PointerServo {
    port: u8,
    target: (i32, i32),
    tolerance: i32,
    max_frames: u32,
    /// Fields displayed since the first poll.
    frames: u32,
    /// Fields completed at the first poll and at the latest one, so the
    /// servo looks at each displayed field once however often it is
    /// polled.
    first_field: Option<u64>,
    seen_field: Option<u64>,
    /// Moves handed out so far.
    moves: u32,
    /// Closest approach so far, and how many answered moves since it
    /// improved. A pointer whose reachable lattice straddles the target
    /// oscillates rather than converging, and has to be recognised as
    /// arrived-as-close-as-it-gets instead of running out the frame
    /// budget.
    best: Option<(i32, (i32, i32))>,
    stalled: u32,
    /// Pixels per mouse count in the caller's coordinates, learned per
    /// axis. In the rendered field's a lo-res pointer pixel is two columns
    /// of the 716-wide canvas, so horizontal motion starts at 2 and
    /// vertical at 1 unless the caller says otherwise ([`Self::with_gain`]).
    gain: (f64, f64),
    /// The delta currently in flight, so a later poll can measure what the
    /// guest did with it.
    outstanding: Option<InFlight>,
}

impl PointerServo {
    pub fn new(port: u8, target: (i32, i32), tolerance: i32, max_frames: u32) -> Self {
        Self {
            port,
            target,
            tolerance: tolerance.clamp(0, TOLERANCE_LIMIT),
            max_frames: max_frames.clamp(1, FRAME_LIMIT),
            frames: 0,
            first_field: None,
            seen_field: None,
            moves: 0,
            best: None,
            stalled: 0,
            gain: (2.0, 1.0),
            outstanding: None,
        }
    }

    /// Start from `gain` pixels per count instead of the rendered field's
    /// (2, 1): for a caller whose coordinates scale the field, such as a
    /// screenshot's. The servo still learns the real ratio; a good first
    /// guess only saves it an overshoot.
    pub fn with_gain(mut self, gain: (f64, f64)) -> Self {
        self.gain = (
            gain.0.clamp(MIN_GAIN, MAX_GAIN),
            gain.1.clamp(MIN_GAIN, MAX_GAIN),
        );
        self
    }

    pub fn target(&self) -> (i32, i32) {
        self.target
    }

    /// Fields this servo has watched go by so far.
    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// Read the pointer off the last completed field and decide the next
    /// move, in the rendered field's coordinates (the control protocol's
    /// `capture.screenshot`).
    pub fn poll(&mut self, bus: &Bus) -> ServoStep {
        self.poll_observed(bus.emulated_frames(), pointer_position(bus))
    }

    /// [`poll`] with the observation supplied: `field` is the number of
    /// fields the machine has completed and `at` where the last of them
    /// drew the pointer, `None` when it drew none. The servo works in
    /// whatever coordinates `at` and the target are given in, so a caller
    /// that maps the sprite into a screenshot's pixels steers in those.
    ///
    /// [`poll`]: Self::poll
    pub fn poll_observed(&mut self, field: u64, at: Option<(i32, i32)>) -> ServoStep {
        // A poll before another field has completed sees the picture the
        // last one did. Nothing new can be learned from it, and a second
        // delta stacked onto counts the guest has not sampled yet could
        // sum past what its 8-bit counter difference carries.
        if self.seen_field == Some(field) {
            return ServoStep::Wait;
        }
        self.seen_field = Some(field);
        let first = *self.first_field.get_or_insert(field);
        self.frames = u32::try_from(field.saturating_sub(first)).unwrap_or(u32::MAX);
        let Some(at) = at else {
            return ServoStep::Failed(if self.moves == 0 {
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
        // Fold in what the delta in flight achieved, once the guest has
        // drawn its answer. Until then the pointer standing still is
        // latency, not refusal; only a guest that stays silent for the
        // whole response window has really not moved it.
        if let Some(flight) = self.outstanding {
            let waited = field.saturating_sub(flight.field);
            if at == flight.from && waited < RESPONSE_FIELDS && self.frames < self.max_frames {
                return ServoStep::Wait;
            }
            self.outstanding = None;
            let (was, counts) = (flight.from, flight.counts);
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
        self.outstanding = Some(InFlight {
            from: at,
            counts,
            field,
        });
        self.moves += 1;
        ServoStep::Move {
            port: self.port,
            dx: counts.0,
            dy: counts.1,
        }
    }
}

/// Where the guest's pointer is in the rendered field: the coordinates the
/// control protocol's `capture.screenshot` writes out. A scheduled
/// `--screenshot-after` capture frames the field differently; see
/// [`crate::video::present_common::CaptureFieldMap`].
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
    use std::collections::VecDeque;

    /// A stand-in guest pointer. It turns quadrature counts into presented
    /// pixels at a fixed ratio, but only the way the hardware lets it: the
    /// counters are sampled once per vertical blank, as the signed 8-bit
    /// difference since the last sample, and the moved pointer is drawn
    /// from the field that blank starts, `delay` fields later still for a
    /// guest whose input handling lags its interrupt.
    struct Guest {
        ratio: (f64, f64),
        /// Where the pointer is with every sampled count applied.
        at: (i32, i32),
        /// Counts injected since the last vertical blank, and the largest
        /// single sample the guest has taken.
        unsampled: (i32, i32),
        largest_sample: i32,
        /// What the field being drawn and the ones queued behind it show.
        drawn: VecDeque<(i32, i32)>,
        /// Fields completed, and where the last of them drew the pointer.
        completed: u64,
        last_drawn: (i32, i32),
    }

    impl Guest {
        fn new(ratio: (f64, f64), delay: usize, start: (i32, i32)) -> Self {
            Self {
                ratio,
                at: start,
                unsampled: (0, 0),
                largest_sample: 0,
                drawn: std::iter::repeat_n(start, delay + 1).collect(),
                completed: 0,
                last_drawn: start,
            }
        }

        fn inject(&mut self, dx: i32, dy: i32) {
            self.unsampled.0 += dx;
            self.unsampled.1 += dy;
        }

        /// The field being drawn completes, and the vertical blank that
        /// starts the next one samples the counters.
        fn wrap(&mut self) {
            self.last_drawn = self.drawn.pop_front().expect("a field is always queued");
            self.completed += 1;
            self.largest_sample = self
                .largest_sample
                .max(self.unsampled.0.abs())
                .max(self.unsampled.1.abs());
            // What the guest reads is the 8-bit counter difference.
            let sample = |counts: i32| f64::from(counts as i8);
            self.at.0 += (sample(self.unsampled.0) * self.ratio.0).round() as i32;
            self.at.1 += (sample(self.unsampled.1) * self.ratio.1).round() as i32;
            self.unsampled = (0, 0);
            self.drawn.push_back(self.at);
        }
    }

    /// Run `servo` against `guest`, polling it `cadence[n]` times while
    /// field n is drawn (cycling through the pattern): an instruction-
    /// budget frame loop polls some fields more than once and skips
    /// others. Returns the step the servo finished on.
    fn drive(servo: &mut PointerServo, guest: &mut Guest, cadence: &[u32]) -> ServoStep {
        for field in 0..2 * FRAME_LIMIT as usize {
            for _ in 0..cadence[field % cadence.len()] {
                match servo.poll_observed(guest.completed, Some(guest.last_drawn)) {
                    ServoStep::Move { dx, dy, .. } => guest.inject(dx, dy),
                    ServoStep::Wait => {}
                    done => return done,
                }
            }
            guest.wrap();
        }
        panic!("servo never terminated");
    }

    /// Drive a servo to `target` against a guest applying a fixed
    /// pixels-per-count ratio, to check the loop converges without being
    /// told what that ratio is. Returns the fields it took and where the
    /// pointer was drawn when it reported arrival.
    fn converge_with(
        ratio: (f64, f64),
        delay: usize,
        cadence: &[u32],
        start: (i32, i32),
        target: (i32, i32),
        tolerance: i32,
    ) -> (u32, (i32, i32)) {
        let mut guest = Guest::new(ratio, delay, start);
        let mut servo = PointerServo::new(0, target, tolerance, DEFAULT_MAX_FRAMES);
        match drive(&mut servo, &mut guest, cadence) {
            ServoStep::Arrived { x, y, frames } => {
                assert_eq!(
                    (x, y),
                    guest.last_drawn,
                    "arrival reports the drawn pointer"
                );
                assert!(
                    guest.largest_sample <= MAX_COUNTS,
                    "the guest was handed {} counts in one sample",
                    guest.largest_sample
                );
                (frames, (x, y))
            }
            ServoStep::Failed(why) => panic!("servo gave up: {why}"),
            other => panic!("drive returned {other:?}"),
        }
    }

    fn converge(
        ratio: (f64, f64),
        start: (i32, i32),
        target: (i32, i32),
        tolerance: i32,
    ) -> (u32, (i32, i32)) {
        converge_with(ratio, 0, &[1], start, target, tolerance)
    }

    #[test]
    fn converges_at_the_gain_it_was_built_for() {
        let (frames, at) = converge((2.0, 1.0), (0, 0), (300, 120), 0);
        assert_eq!(at, (300, 120));
        // Three corrections, each answered two field wraps after it went in.
        assert_eq!(frames, 6);
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
        assert!(frames <= 20, "took {frames} frames");
    }

    #[test]
    fn converges_when_the_guest_moves_less_than_one_pixel_per_count() {
        let (frames, at) = converge((1.0, 1.0), (10, 10), (200, 90), 0);
        assert_eq!(at, (200, 90));
        assert!(frames <= 16, "took {frames} frames");
    }

    #[test]
    fn repeated_polls_within_a_field_neither_stack_deltas_nor_cost_frames() {
        // The frame loop runs an instruction budget, not a field, so it
        // can poll one field several times. Each extra delta would land
        // on counts the guest has not sampled yet; two 64-count steps in
        // one field read back as -128, motion the wrong way.
        let once = converge_with((2.0, 1.0), 0, &[1], (0, 0), (300, 120), 0);
        let thrice = converge_with((2.0, 1.0), 0, &[3], (0, 0), (300, 120), 0);
        assert_eq!(thrice, once, "the servo works in fields, not polls");
    }

    #[test]
    fn fields_the_caller_never_polls_still_count() {
        // Skipped fields are time the guest had to answer: the servo must
        // pick its answer up late rather than lose it.
        let (frames, at) = converge_with((2.0, 1.0), 0, &[1, 0, 2, 0, 0, 1], (0, 0), (300, 120), 0);
        assert_eq!(at, (300, 120));
        assert!(frames <= 16, "took {frames} frames");
    }

    #[test]
    fn a_guest_that_draws_its_answer_fields_late_still_converges() {
        // Input handled by a task a few fields behind the interrupt: the
        // latest answer the response window still waits for.
        let delay = (RESPONSE_FIELDS - 2) as usize;
        let (frames, at) = converge_with((2.0, 1.0), delay, &[1], (0, 0), (300, 120), 0);
        assert_eq!(at, (300, 120));
        assert_eq!(frames, 3 * RESPONSE_FIELDS as u32);
    }

    #[test]
    fn gives_up_when_the_pointer_will_not_move() {
        let mut guest = Guest::new((0.0, 0.0), 0, (10, 10));
        let mut servo = PointerServo::new(0, (400, 100), 0, DEFAULT_MAX_FRAMES);
        match drive(&mut servo, &mut guest, &[1]) {
            ServoStep::Failed(why) => {
                assert!(why.contains("cannot reach (400, 100)"), "{why}");
                assert!(why.contains("settles 390 px away at (10, 10)"), "{why}");
            }
            other => panic!("a stuck pointer must be reported, got {other:?}"),
        }
        // Each silent move is given the whole response window before it
        // counts against the pointer.
        assert_eq!(servo.frames(), STALL_LIMIT * RESPONSE_FIELDS as u32);
    }

    #[test]
    fn the_frame_budget_counts_displayed_fields() {
        // Far enough that the budget runs out first, polled three times a
        // field: the servo stops after its budget of fields, not polls.
        let mut guest = Guest::new((0.25, 0.25), 0, (0, 0));
        let mut servo = PointerServo::new(0, (600, 280), 0, 10);
        match drive(&mut servo, &mut guest, &[3]) {
            ServoStep::Failed(why) => {
                assert!(
                    why.contains("did not reach (600, 280) in 10 frame(s)"),
                    "{why}"
                )
            }
            other => panic!("the budget should have run out, got {other:?}"),
        }
        assert_eq!(guest.completed, 10);
    }

    #[test]
    fn a_target_the_pointer_is_already_on_needs_no_motion() {
        let mut servo = PointerServo::new(0, (128, 64), 0, DEFAULT_MAX_FRAMES);
        assert_eq!(
            servo.poll_observed(7, Some((128, 64))),
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
        // Sixteen pixels per 64-count step: a long way in fields.
        assert!(frames <= 32, "took {frames} frames");
    }

    #[test]
    fn learned_gain_stays_inside_its_bounds() {
        assert_eq!(learn(2.0, 1000, 1), MAX_GAIN);
        assert_eq!(learn(0.3, 1, 1000), MIN_GAIN);
        // A move against the requested direction leaves the estimate be.
        assert_eq!(learn(2.0, -8, 4), 2.0);
    }
}
