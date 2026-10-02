// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

// Exercise the page's capture wrapper without booting its DOM/WebGL shell.
const page = readFileSync(new URL('./try.js', import.meta.url), 'utf8');
const start = page.indexOf('function withTvAperture(');
const end = page.indexOf('async function copyScreenshot()', start);
assert.ok(start >= 0 && end > start);
const captureWrapper = page.slice(start, end);

function captureSettings(bezel, integer, autocrop) {
  const state = { bezel, integer, autocrop };
  const context = {
    monitorBezelOn: () => bezel,
    layoutSupported: true,
    scalingMode: integer ? 'integer' : 'smooth',
    autocropOn: autocrop,
    emu: {
      set_monitor_bezel: value => { state.bezel = value; },
      set_scaling: value => { state.integer = value === 'integer'; },
      set_autocrop: value => { state.autocrop = value; },
    },
  };
  runInNewContext(captureWrapper, context);
  return { state, capture: context.withTvAperture };
}

test('screenshots read the framing aperture and restore the live modes before encoding ends', async () => {
  for (const bezel of [false, true]) {
    for (const integer of [false, true]) {
      for (const autocrop of [false, true]) {
        const { state, capture } = captureSettings(bezel, integer, autocrop);
        let finishEncoding;
        const encoded = capture(() => {
          assert.deepEqual(state, { bezel: false, integer: false, autocrop: false });
          return new Promise(resolve => { finishEncoding = resolve; });
        });
        assert.deepEqual(state, { bezel, integer, autocrop });
        finishEncoding('captured aperture');
        assert.equal(await encoded, 'captured aperture');
      }
    }
  }
});

test('a failed screenshot restores Smart autocrop and monitor settings', () => {
  const { state, capture } = captureSettings(true, true, true);
  const failure = new Error('buffer read failed');
  assert.throws(() => capture(() => { throw failure; }), error => error === failure);
  assert.deepEqual(state, { bezel: true, integer: true, autocrop: true });
});
