// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import {
  EMPTY_PAD,
  JOY_KEYS_CD32,
  JOY_KEYS_TWO_BUTTON,
  joyKeyMap,
  keyboardPortState,
  orPortState,
  padPressed,
  readPad,
} from './input-map.js';

test('keyboard joystick modes pick their key tables', () => {
  assert.equal(joyKeyMap('keys'), JOY_KEYS_TWO_BUTTON);
  assert.equal(joyKeyMap('cd32'), JOY_KEYS_CD32);
  assert.equal(joyKeyMap('off'), null);
  assert.equal(joyKeyMap('touch'), null);
  // The two-button mode leaves Enter and the letters to the Amiga keyboard.
  assert.equal(JOY_KEYS_TWO_BUTTON.Enter, undefined);
  assert.equal(JOY_KEYS_TWO_BUTTON.KeyC, undefined);
  assert.equal(JOY_KEYS_CD32.ArrowUp, 'up');
  assert.equal(JOY_KEYS_CD32.Enter, 'play');
});

test('held keys fold into one port state', () => {
  const held = {};
  assert.deepEqual(keyboardPortState(held), { ...EMPTY_PAD });
  held[JOY_KEYS_TWO_BUTTON.ArrowLeft] = true;
  held[JOY_KEYS_TWO_BUTTON.ControlLeft] = true;
  held[JOY_KEYS_TWO_BUTTON.AltLeft] = true;
  const state = keyboardPortState(held);
  assert.equal(state.left, true);
  assert.equal(state.fire, true);
  assert.equal(state.button2, true);
  assert.equal(state.right, false);
  // Any one of the fire keys holds fire; releasing one of two keeps it.
  held[JOY_KEYS_TWO_BUTTON.ControlRight] = true;
  held[JOY_KEYS_TWO_BUTTON.ControlLeft] = false;
  assert.equal(keyboardPortState(held).fire, true);
  held[JOY_KEYS_TWO_BUTTON.ControlRight] = false;
  assert.equal(keyboardPortState(held).fire, false);
});

test('port sources are OR-ed and a missing source contributes nothing', () => {
  const a = { ...EMPTY_PAD, up: true };
  const b = { ...EMPTY_PAD, fire: true };
  assert.deepEqual(orPortState(a, b), { ...EMPTY_PAD, up: true, fire: true });
  assert.equal(orPortState(a, null), a);
  assert.equal(orPortState(null, b), b);
  assert.equal(orPortState(null, null), EMPTY_PAD);
});

test('gamepad buttons read as objects or bare numbers', () => {
  assert.equal(padPressed({ buttons: [{ pressed: true, value: 1 }] }, 0), true);
  assert.equal(padPressed({ buttons: [{ pressed: false, value: 0.7 }] }, 0), true);
  assert.equal(padPressed({ buttons: [{ pressed: false, value: 0.2 }] }, 0), false);
  assert.equal(padPressed({ buttons: [0.9] }, 0), true);
  assert.equal(padPressed({ buttons: [] }, 3), false);
});

test('sticks and d-pad both steer and the face buttons follow the CD32 pad', () => {
  const buttons = Array.from({ length: 16 }, () => ({ pressed: false, value: 0 }));
  const idle = readPad({ buttons, axes: [0.2, -0.2] });
  assert.deepEqual(idle, { ...EMPTY_PAD });

  const stick = readPad({ buttons, axes: [-0.8, 0.6] });
  assert.equal(stick.left, true);
  assert.equal(stick.down, true);
  assert.equal(stick.right, false);
  assert.equal(stick.up, false);

  buttons[12] = { pressed: true, value: 1 };
  buttons[0] = { pressed: true, value: 1 };
  buttons[1] = { pressed: true, value: 1 };
  buttons[9] = { pressed: true, value: 1 };
  const dpad = readPad({ buttons, axes: [] });
  assert.equal(dpad.up, true);
  assert.equal(dpad.fire, true);
  assert.equal(dpad.button2, true);
  assert.equal(dpad.play, true);
  assert.equal(dpad.green, false);
});
