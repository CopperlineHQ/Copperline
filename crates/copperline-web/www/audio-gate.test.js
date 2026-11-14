// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import {
  AUDIO_GATE_CLOSE_MS,
  AUDIO_GATE_MAX_FRAMES,
  AUDIO_GATE_OPEN_MS,
  RAF_STARVED_MS,
  STEP_OVERLOAD_MS,
  audioGateMaxFrames,
  newAudioGate,
  rafStarved,
} from './audio-gate.js';

test('an open gate steps up to the burst cap', () => {
  const gate = newAudioGate();
  assert.equal(audioGateMaxFrames(gate, 0), AUDIO_GATE_MAX_FRAMES);
  assert.equal(audioGateMaxFrames(gate, AUDIO_GATE_CLOSE_MS), AUDIO_GATE_MAX_FRAMES);
  assert.equal(gate.closed, false);
});

test('the gate closes past the high mark and reopens only under the low one', () => {
  const gate = newAudioGate();
  assert.equal(audioGateMaxFrames(gate, AUDIO_GATE_CLOSE_MS + 1), 0);
  assert.equal(gate.closed, true);
  // Between the marks a closed gate stays closed: a queue riding one
  // threshold must not flip it on every report.
  assert.equal(audioGateMaxFrames(gate, AUDIO_GATE_OPEN_MS + 1), 0);
  assert.equal(audioGateMaxFrames(gate, AUDIO_GATE_OPEN_MS), AUDIO_GATE_MAX_FRAMES);
  assert.equal(gate.closed, false);
  // And an open one stays open in the same band.
  assert.equal(audioGateMaxFrames(gate, AUDIO_GATE_OPEN_MS + 1), AUDIO_GATE_MAX_FRAMES);
});

test('a queue report steps the machine only when rAF is stale and steps are cheap', () => {
  // A halved 30 Hz cadence is the normal tick's business.
  assert.equal(rafStarved(1033, 1000, 2), false);
  assert.equal(rafStarved(1000 + RAF_STARVED_MS, 1000, 2), false);
  assert.equal(rafStarved(1000 + RAF_STARVED_MS + 1, 1000, 2), true);
  // An overloaded host is left to rAF alone, however stale it is.
  assert.equal(rafStarved(5000, 1000, STEP_OVERLOAD_MS), false);
  assert.equal(rafStarved(5000, 1000, STEP_OVERLOAD_MS - 1), true);
});
