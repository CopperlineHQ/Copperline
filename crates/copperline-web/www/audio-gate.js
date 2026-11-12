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
