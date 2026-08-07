// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import {
  newRenderStrideState,
  updateRenderStrideState,
} from './render-stride.js';

test('deferred renders do not lower the comparable full-render cost', () => {
  const state = newRenderStrideState();
  updateRenderStrideState(state, 0, 17, 1, true);
  const fullRenderAverage = state.avgFrameStepMs;

  for (let now = 16; now <= 2000; now += 16) {
    updateRenderStrideState(state, now, 5, 1, false);
  }

  assert.equal(state.avgFrameStepMs, fullRenderAverage);
  assert.ok(state.avgStepMs < fullRenderAverage, 'raw-call EWMA still sees hidden work');
});

test('steady overload enters once and lower full-render cost recovers once', () => {
  const state = newRenderStrideState();
  let previous = state.active;
  const transitions = [];

  for (let now = 0; now <= 7000; now += 16) {
    const rendered = !state.active || Math.floor(now / 16) % 2 === 0;
    const fullRenderCost = now < 3500 ? 17 : 10;
    updateRenderStrideState(
      state,
      now,
      rendered ? fullRenderCost : 5,
      1,
      rendered,
    );
    if (state.active !== previous) {
      transitions.push([now, state.active]);
      previous = state.active;
    }
  }

  assert.deepEqual(
    transitions.map(([, active]) => active),
    [true, false],
  );
  assert.ok(transitions[0][0] >= 500, 'entry observes its hold interval');
  assert.ok(transitions[1][0] >= 5500, 'recovery observes its hold interval');
});
