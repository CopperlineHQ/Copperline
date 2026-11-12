// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import {
  AUDIO_GATE_CLOSE_MS,
  AUDIO_GATE_MAX_FRAMES,
  AUDIO_GATE_OPEN_MS,
  audioGateMaxFrames,
  newAudioGate,
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
