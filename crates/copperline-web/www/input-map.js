// SPDX-License-Identifier: GPL-3.0-or-later

// Host input to Amiga controller-port state, shared by the copperline.dev/try
// page glue (try.js) and the single-file web player kit (kit-player.js) so a
// mapping fix is made once. Nothing here touches the DOM or the emulator:
// every function maps plain host state (held key names, a Gamepad snapshot)
// to the port-state record the pages hand to WebEmu.set_joystick_port and
// WebEmu.set_cd32_buttons_port.

// One controller port's state: a two-button stick plus the CD32 pad extras.
export const EMPTY_PAD = Object.freeze({
  up: false,
  down: false,
  left: false,
  right: false,
  fire: false,
  button2: false,
  play: false,
  rwd: false,
  ffw: false,
  green: false,
  yellow: false,
});

// Keyboard joystick, two-button stick: the desktop frontend's
// FS-UAE-compatible mapping plus left-hand fire keys. Cursor keys steer,
// Right Ctrl / Right Alt or Left Ctrl fire, Left Alt is the second button
// (left-hand fire pairs with the right-hand arrows, and compact keyboards
// often lack the right-side modifiers). Values name the held-key slots
// keyboardPortState reads.
export const JOY_KEYS_TWO_BUTTON = Object.freeze({
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  ControlRight: 'fireCtrl',
  AltRight: 'fireAlt',
  ControlLeft: 'fireLCtrl',
  AltLeft: 'blueLAlt',
});

// Keyboard CD32 pad: the stick mapping plus the pad extras on
// C/X/D/S/Enter/Z/A.
export const JOY_KEYS_CD32 = Object.freeze({
  ...JOY_KEYS_TWO_BUTTON,
  KeyC: 'red',
  KeyX: 'blue',
  KeyD: 'green',
  KeyS: 'yellow',
  Enter: 'play',
  NumpadEnter: 'play',
  KeyZ: 'rwd',
  KeyA: 'ffw',
});

// The key table for a keyboard joystick mode, or null when the mode leaves
// the keyboard to the Amiga ("off", "touch", anything unknown).
export function joyKeyMap(mode) {
  if (mode === 'keys') return JOY_KEYS_TWO_BUTTON;
  if (mode === 'cd32') return JOY_KEYS_CD32;
  return null;
}

// Port state from the held-key slots the tables above fill in.
export function keyboardPortState(held) {
  const h = held;
  return {
    up: !!h.up,
    down: !!h.down,
    left: !!h.left,
    right: !!h.right,
    fire: !!(h.fireCtrl || h.fireAlt || h.fireLCtrl || h.red),
    button2: !!(h.blue || h.blueLAlt),
    play: !!h.play,
    rwd: !!h.rwd,
    ffw: !!h.ffw,
    green: !!h.green,
    yellow: !!h.yellow,
  };
}

// Merge two sources on the same port. Sources are OR-ed rather than one
// silencing the other, so a pad and the keyboard can both be live without
// either going dead mid-game. A missing source contributes nothing.
export function orPortState(a, b) {
  if (!a) return b ?? EMPTY_PAD;
  if (!b) return a;
  const out = {};
  for (const k of Object.keys(EMPTY_PAD)) out[k] = a[k] || b[k];
  return out;
}

// Analogue stick deflection that counts as a direction.
export const PAD_AXIS_THRESHOLD = 0.5;

// Whether a Gamepad button is held. Browsers report either GamepadButton
// objects or (old WebKit) bare numbers.
export function padPressed(pad, index) {
  const b = pad.buttons[index];
  if (!b) return false;
  return typeof b === 'object' ? b.pressed || b.value > 0.5 : b > 0.5;
}

// Port state from a standard-mapping Gamepad. Sticks and d-pads both steer,
// so the same pad works whichever a game expects. The face buttons follow
// the CD32 pad, a superset of a two-button stick: A fires (red), B is
// button 2 (blue), X/Y are green/yellow, the shoulders are rewind/forward,
// Start is play.
export function readPad(pad) {
  const axis = (i) => (typeof pad.axes[i] === 'number' ? pad.axes[i] : 0);
  return {
    up: padPressed(pad, 12) || axis(1) <= -PAD_AXIS_THRESHOLD,
    down: padPressed(pad, 13) || axis(1) >= PAD_AXIS_THRESHOLD,
    left: padPressed(pad, 14) || axis(0) <= -PAD_AXIS_THRESHOLD,
    right: padPressed(pad, 15) || axis(0) >= PAD_AXIS_THRESHOLD,
    fire: padPressed(pad, 0),
    button2: padPressed(pad, 1),
    green: padPressed(pad, 2),
    yellow: padPressed(pad, 3),
    rwd: padPressed(pad, 4),
    ffw: padPressed(pad, 5),
    play: padPressed(pad, 9),
  };
}
