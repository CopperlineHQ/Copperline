#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Exercise the release wasm-bindgen bundle; no sockets or display are needed.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const pkg = resolve(process.argv[2] ?? resolve(root, 'crates/copperline-web/pkg'));
const glue = await readFile(`${pkg}/copperline_web.js`, 'utf8');
const { default: init, WebEmu } = await import(`data:text/javascript;base64,${Buffer.from(glue).toString('base64')}`);
const wasm = await init({ module_or_path: await readFile(`${pkg}/copperline_web_bg.wasm`) });
const rom = new Uint8Array(await readFile(resolve(root, 'assets/aros/aros-amiga-m68k-rom.bin')));
const ext = new Uint8Array(await readFile(resolve(root, 'assets/aros/aros-amiga-m68k-ext.bin')));
const code = '0123456789abcdef0123456789abcdef';

function fresh(model = 'A500', video = 'PAL') {
  const emu = new WebEmu(model, video, 2);
  emu.load_rom(rom, ext);
  return emu;
}

const validation = fresh();
try {
  for (const player of [-1, 0, 3, 1.5, 257, NaN, Infinity]) {
    assert.throws(() => validation.start_netplay(player, code, 2, 8, 'joystick'));
  }
  for (const delay of [-1, 7, 1.5, 257, NaN, Infinity]) {
    assert.throws(() => validation.start_netplay(1, code, delay, 8, 'joystick'));
  }
  for (const window of [-1, 0, 13, 1.5, 257, NaN, Infinity]) {
    assert.throws(() => validation.start_netplay(1, code, 2, window, 'joystick'));
  }
  assert.throws(() => validation.start_netplay(1, 'bad', 2, 8, 'joystick'));
  assert.throws(() => validation.start_netplay(1, code, 2, 8, 'mouse'));
  validation.start_netplay(1, code, 2, 8, 'joystick');
  for (const mutate of [() => validation.reset(), () => validation.save_state(),
    () => validation.load_rom(rom, ext), () => validation.insert_floppy(0, new Uint8Array(901120), 'blank.adf'),
    () => validation.eject_floppy(0), () => validation.load_state(new Uint8Array())]) assert.throws(mutate);
  assert.throws(() => validation.start_netplay(1, code, 2, 8, 'joystick'));
} finally { validation.free(); }
const warm = fresh();
try {
  warm.run(0, 0);
  assert.throws(() => warm.start_netplay(1, code, 2, 8, 'joystick'));
} finally { warm.free(); }

for (const [model, video, delay, window] of [['A500', 'PAL', 0, 8], ['A500', 'PAL', 2, 8], ['A1200', 'NTSC', 6, 1]]) {
  const peers = [fresh(model, video), fresh(model, video)];
  try {
    peers.forEach((emu, player) => {
      emu.insert_floppy(0, new Uint8Array(901120), player ? 'other/path.adf' : 'disk.adf');
      emu.set_volume_percent(player ? 40 : 100);
      emu.start_netplay(player + 1, code, delay, window, 'cd32');
    });
    let queued = [];
    let packets = 0;
    for (let tick = 0; tick < 1800; tick++) {
      for (let player = 0; player < 2; player++) {
        const emu = peers[player];
        const frame = emu.netplay_status()[1];
        emu.set_joystick_port(2, frame % 9 < 3, false, false, false, frame % 7 < 3, false);
        emu.set_cd32_buttons_port(2, frame % 5 < 2, frame % 7 < 2, frame % 11 < 3, frame % 13 < 4, frame % 17 < 3);
        emu.key_event('Space', frame % 13 < 4);
        const advance = frame < 120 && (player !== 0 || tick % 90 < 30 || tick % 90 >= 42);
        // Deliberately different rendering cadence, display settings and audio
        // levels: host presentation must not enter machine checkpoint hashes.
        if (player === 1) {
          emu.set_volume_percent(35);
          if (tick % 9 === 0) emu.set_overscan(tick % 18 ? 'tv' : 'full');
        }
        if (player === 1 && tick % 3) emu.run_hidden(tick * 20, advance ? 1 : 0);
        else emu.run(tick * 20, advance ? 1 : 0);
        emu.take_audio();
        for (;;) {
          const bytes = emu.netplay_take_packet();
          if (!bytes.length) break;
          packets++;
          if (packets % 7 === 0) continue;
          queued.push({ due: tick + packets % 5, target: 1 - player, bytes });
          if (packets % 11 === 0) queued.push({ due: tick + packets % 5 + 2, target: 1 - player, bytes });
        }
      }
      const ready = queued.filter(packet => packet.due <= tick);
      queued = queued.filter(packet => packet.due > tick);
      for (const packet of ready) peers[packet.target].netplay_receive(packet.bytes);
      if (peers.every(emu => { const s = emu.netplay_status(); return s[1] === 120 && s[2] === 120 && s[3] >= 120 && s[6] === 120; })) break;
    }
    for (const emu of peers) {
      const status = emu.netplay_status();
      assert.equal(status[1], 120);
      assert.equal(status[2], 120);
      assert.equal(status[6], 120);
      if (delay === 0) assert.ok(status[4] > 0, 'late inputs must exercise rollback');
      emu.set_overscan('tv');
      emu.run(40000, 0);
    }
    const pixels = peers.map(emu => Buffer.from(new Uint8Array(wasm.memory.buffer,
      emu.present_ptr(), emu.present_width() * emu.present_rows() * 4)));
    assert.deepEqual(pixels[0], pixels[1]);
    console.log(`${model}/${video} delay=${delay} window=${window}: 120 confirmed/checksummed frames; identical render`);
  } finally { peers.forEach(emu => emu.free()); }
}
console.log('WASM netplay numeric boundaries, session guards, packet loss/reordering and presentation isolation passed');
