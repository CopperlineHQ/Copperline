// SPDX-License-Identifier: GPL-3.0-or-later

import assert from 'node:assert/strict';
import test from 'node:test';
import { canvasDeviceSize, cssToEmuScale, layoutFromArray } from './present-2d.js';

test('the backing store follows the element at device resolution', () => {
  assert.deepEqual(canvasDeviceSize({ clientWidth: 640, clientHeight: 480 }, 2), [1280, 960]);
  assert.deepEqual(canvasDeviceSize({ clientWidth: 333, clientHeight: 250 }, 1.5), [500, 375]);
  // A collapsed element still gets a drawable store, and a missing ratio is 1.
  assert.deepEqual(canvasDeviceSize({ clientWidth: 0, clientHeight: 0 }, undefined), [1, 1]);
});

test('a present_layout result becomes a record, or null with no frame', () => {
  assert.equal(layoutFromArray(undefined), null);
  assert.equal(layoutFromArray([]), null);
  assert.deepEqual(layoutFromArray([1, 2, 668, 540, 10, 20, 1336, 1080, 2, 2]), {
    sx: 1,
    sy: 2,
    sw: 668,
    sh: 540,
    dx: 10,
    dy: 20,
    dw: 1336,
    dh: 1080,
    factors: [2, 2],
  });
  assert.equal(layoutFromArray([0, 0, 668, 540, 0, 0, 800, 600, 0, 0]).factors, null);
});

test('pointer scale is per axis and follows a layout when one is drawn', () => {
  const canvas = { width: 668, height: 540, clientWidth: 800, clientHeight: 600 };
  const plain = cssToEmuScale(null, { width: 668, rows: 540 }, canvas, 2);
  assert.equal(plain.x, 668 / 800);
  assert.equal(plain.y, 540 / 600);
  // No presentation tracked yet: the canvas's own store stands in.
  const early = cssToEmuScale(null, { width: 0, rows: 0 }, canvas, 1);
  assert.equal(early.x, 668 / 800);
  // A bezel leaves the picture a fraction of the element.
  const bezel = cssToEmuScale(null, { width: 668, rows: 540 }, canvas, 1, 0.5);
  assert.equal(bezel.x, 668 / 400);
  const lay = layoutFromArray([0, 0, 668, 540, 0, 0, 1336, 1080, 2, 2]);
  const laid = cssToEmuScale(lay, { width: 668, rows: 540 }, canvas, 2);
  assert.equal(laid.x, 1);
  assert.equal(laid.y, 1);
});
