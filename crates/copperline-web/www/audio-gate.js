// SPDX-License-Identifier: GPL-3.0-or-later

// The audio-clock pacing gate, shared by the copperline.dev/try page glue
// (try.js) and the single-file web player kit (kit-player.js).
//
// The audio clock is the master: while the worklet has plenty queued, the
// gate closes and stepping pauses, locking production to the audio
// device's consumption rate. Two things keep the gate from chopping the
// output. Hysteresis: it closes past 150 ms but reopens only under 90 ms,
// because with a single threshold a queue riding it flips the gate every
// report. And the caller re-anchors the pacer while the gate is closed
// (WebEmu.resync_clock), so the pause is forgiven rather than owed: repaid,
// the reopening tick would step several frames at once and blit only the
// last.

export const AUDIO_GATE_CLOSE_MS = 150;
export const AUDIO_GATE_OPEN_MS = 90;

// The burst cap of an open gate: it only bounds a single tick's catch-up
// work after rAF throttling (the pacer forgives deficits past 100 ms).
export const AUDIO_GATE_MAX_FRAMES = 5;

export function newAudioGate() {
  return { closed: false };
}

// Feed the worklet's latest queue depth through the gate and return how
// many frames the machine may step this tick: 0 while the gate is closed
// (the caller then re-anchors its pacer), AUDIO_GATE_MAX_FRAMES otherwise.
export function audioGateMaxFrames(gate, queuedMs) {
  const limit = gate.closed ? AUDIO_GATE_OPEN_MS : AUDIO_GATE_CLOSE_MS;
  gate.closed = queuedMs > limit;
  return gate.closed ? 0 : AUDIO_GATE_MAX_FRAMES;
}

// The starved-rAF fallback. The worklet's queue reports (one every ~29 ms)
// are a clock the browser never throttles, so a page that is visible but
// starved of animation frames (an unfocused window on a power-saving host)
// can step the machine from them and keep it and its audio real time; only
// the displayed rate degrades to whatever the compositor manages.
//
// RAF_STARVED_MS is how stale the last animation frame may be before a
// report steps the machine itself: above the ~33 ms gap of a merely halved
// (30 Hz) cadence, which the normal tick absorbs by stepping twice, and
// well under the ~100 ms deficit the pacer forgives, past which lost wall
// time becomes lost emulated time.
export const RAF_STARVED_MS = 50;

// The fallback exists for a THROTTLED host: animation frames withheld from
// a machine that is cheap to step. On an OVERLOADED host - the machine
// itself eating more than the report interval per step - stepping on every
// report leaves the main thread no idle at all: the page freezes solid
// while emulated time barely gains, because the wall-paced core cannot
// exceed the host's throughput however often it is called. Past this
// whole-call step cost the fallback stands down and rAF alone drives the
// machine at whatever rate the host can sustain.
export const STEP_OVERLOAD_MS = 25;

// Whether a queue report arriving at `nowMs` should step the machine:
// `lastRafMs` is when the last animation frame ran and `avgStepMs` the
// smoothed whole-call cost of a step (the render stride's avgStepMs).
export function rafStarved(nowMs, lastRafMs, avgStepMs) {
  return nowMs - lastRafMs > RAF_STARVED_MS && avgStepMs < STEP_OVERLOAD_MS;
}
