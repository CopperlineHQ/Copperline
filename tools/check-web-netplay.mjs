#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Exercise the release wasm-bindgen bundle; no sockets or display are needed.
import assert from 'node:assert/strict';
import { PACKET_LIMIT } from '../crates/copperline-web/www/netplay.js';
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

const [protocol, packetLimit, headerBytes, inputBytes] = WebEmu.netplay_packet_layout();
assert.equal(protocol, 2);
assert.equal(packetLimit, PACKET_LIMIT, 'Rust and WebRTC packet limits must match');
assert.equal(headerBytes, 111);
assert.equal(inputBytes, 31);
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
  assert.throws(() => validation.start_netplay(1, code, 2, 8, 'analogue'));
  const saved = validation.save_state();
  validation.start_netplay(1, code, 2, 8, 'joystick');
  for (const mutate of [() => validation.reset(), () => validation.save_state(),
    () => validation.load_rom(rom, ext), () => validation.insert_floppy(0, new Uint8Array(901120), 'blank.adf'),
    () => validation.eject_floppy(0), () => validation.load_state(saved),
    () => validation.set_floppy_sounds(false), () => validation.set_floppy_sounds_volume(12),
    () => validation.set_port_device(1, 'mouse'), () => validation.set_floppy_speed(400)]) assert.throws(mutate);
  assert.throws(() => validation.start_netplay(1, code, 2, 8, 'joystick'));
} finally { validation.free(); }
const warm = fresh();
try {
  warm.run(0, 0);
  assert.throws(() => warm.start_netplay(1, code, 2, 8, 'joystick'));
} finally { warm.free(); }

for (const [model, video, delay, window, controller] of [
  ['A500', 'PAL', 0, 8, 'cd32'], ['A500', 'PAL', 2, 8, 'cd32'], ['A1200', 'NTSC', 6, 1, 'cd32'],
  ['A500', 'PAL', 0, 8, 'mouse'], ['A500', 'PAL', 2, 8, 'mouse'], ['A1200', 'NTSC', 6, 1, 'mouse'],
]) {
  const peers = [fresh(model, video), fresh(model, video)];
  try {
    peers.forEach((emu, player) => {
      emu.insert_floppy(0, new Uint8Array(901120), player ? 'other/path.adf' : 'disk.adf');
      emu.set_volume_percent(player ? 40 : 100);
      emu.start_netplay(player + 1, code, delay, window, controller);
    });
    let queued = [];
    const mouseFrame = [-1, -1];
    let packets = 0;
    let checkedSoundGuard = false;
    for (let tick = 0; tick < 1800; tick++) {
      for (let player = 0; player < 2; player++) {
        const emu = peers[player];
        const frame = emu.netplay_status()[1];
        emu.set_joystick_port(2, frame % 9 < 3, false, false, false, frame % 7 < 3, false);
        emu.set_cd32_buttons_port(2, frame % 5 < 2, frame % 7 < 2, frame % 11 < 3, frame % 13 < 4, frame % 17 < 3);
        emu.key_event('Space', frame % 13 < 4);
        if (controller === 'mouse' && mouseFrame[player] !== frame) {
          emu.mouse_delta(frame % 19 - 9, 11 - (frame + player) % 23);
          for (const [button, bit] of [[0, 0], [2, 1], [1, 2]]) {
            emu.mouse_button(button, !!((frame % 8) & (1 << bit)));
          }
          mouseFrame[player] = frame;
        }
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
          assert.ok(bytes.length <= packetLimit);
          // Inspect the sampled input, independently of peer checksum equality.
          const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
          for (let offset = headerBytes; offset < bytes.length; offset += inputBytes) {
            const sampled = Number(view.getBigUint64(offset, true)) - delay;
            if (sampled < 0) continue;
            const expected = (sampled % 9 < 3 ? 1 : 0) | (sampled % 7 < 3 ? 16 : 0)
              | (sampled % 5 < 2 ? 64 : 0) | (sampled % 7 < 2 ? 128 : 0)
              | (sampled % 11 < 3 ? 256 : 0) | (sampled % 13 < 4 ? 512 : 0)
              | (sampled % 17 < 3 ? 1024 : 0);
            assert.equal(view.getUint16(offset + 8, true), controller === 'mouse' ? 0 : expected, 'controller routing');
            if (controller === 'mouse') {
              assert.equal(view.getInt16(offset + 26, true), sampled % 19 - 9, 'mouse X sampled once');
              assert.equal(view.getInt16(offset + 28, true), 11 - (sampled + player) % 23, 'mouse Y sampled once');
              assert.equal(bytes[offset + 30], sampled % 8, 'three mouse buttons');
            }
            assert.equal(bytes[offset + 10 + 8], sampled % 13 < 4 ? 1 : 0, 'Space key routing');
          }
          packets++;
          if (packets % 7 === 0) continue;
          queued.push({ due: tick + packets % 5, target: 1 - player, bytes });
          if (packets % 11 === 0) queued.push({ due: tick + packets % 5 + 2, target: 1 - player, bytes });
        }
      }
      if (!checkedSoundGuard && peers[0].netplay_status()[6] >= 60) {
        assert.throws(() => peers[0].set_floppy_sounds(false), /Unavailable during netplay/);
        assert.throws(() => peers[0].set_floppy_sounds_volume(12), /Unavailable during netplay/);
        checkedSoundGuard = true;
      }
      const ready = queued.filter(packet => packet.due <= tick);
      queued = queued.filter(packet => packet.due > tick);
      for (const packet of ready) peers[packet.target].netplay_receive(packet.bytes);
      if (peers.every(emu => { const s = emu.netplay_status(); return s[1] === 120 && s[2] === 120 && s[3] >= 120 && s[6] === 120; })) break;
    }
    assert.ok(checkedSoundGuard);
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
    console.log(`${model}/${video} ${controller} delay=${delay} window=${window}: 120 confirmed/checksummed frames; identical render`);
  } finally { peers.forEach(emu => emu.free()); }
}
console.log('WASM netplay numeric boundaries, session guards, packet loss/reordering and presentation isolation passed');

// A spectator joins a game in progress: it replays the host's history from
// frame zero in arbitrary slices, catches up, then follows live with every
// checkpoint verified and the same picture as the players.
{
  const peers = [fresh(), fresh()];
  const spectators = [];
  try {
    peers.forEach((emu, player) => {
      emu.insert_floppy(0, new Uint8Array(901120), 'disk.adf');
      emu.start_netplay(player + 1, code, 2, 8, 'cd32');
    });
    peers[0].netplay_enable_spectators();
    assert.throws(() => peers[1].spectator_feed_open(), /not enabled/, 'only the host keeps history');
    const feeds = [];
    let queued = [];
    let packets = 0;
    let seed = 11;
    const random = () => { seed = (seed * 48271) % 2147483647; return seed; };
    for (let tick = 0; tick < 6000; tick++) {
      for (let player = 0; player < 2; player++) {
        const emu = peers[player];
        const frame = emu.netplay_status()[1];
        emu.set_joystick_port(2, frame % 9 < 3, false, false, false, frame % 7 < 3, false);
        emu.key_event('Space', (frame + player) % 13 < 4);
        emu.run_hidden(tick * 20, frame < 300 ? 1 : 0);
        emu.take_audio();
        for (;;) {
          const bytes = emu.netplay_take_packet();
          if (!bytes.length) break;
          packets++;
          if (packets % 7 === 0) continue;
          queued.push({ due: tick + packets % 5, target: 1 - player, bytes });
        }
      }
      const ready = queued.filter(packet => packet.due <= tick);
      queued = queued.filter(packet => packet.due > tick);
      for (const packet of ready) peers[packet.target].netplay_receive(packet.bytes);
      // One spectator joins at frame 60 and another at 200, each from frame zero.
      for (const at of [60, 200]) {
        if (feeds.length === [60, 200].indexOf(at) && peers[0].netplay_status()[2] >= at) {
          const spectator = fresh();
          spectator.insert_floppy(0, new Uint8Array(901120), 'anything/else.adf');
          spectator.start_spectating('cd32');
          assert.deepEqual([...spectator.spectate_identity()], [...peers[0].netplay_identity()]);
          assert.throws(() => spectator.start_netplay(1, code, 2, 8, 'cd32'), /Unavailable/);
          assert.throws(() => spectator.reset(), /Unavailable/);
          assert.throws(() => spectator.insert_floppy(0, new Uint8Array(901120), 'x.adf'), /Unavailable/);
          spectators.push(spectator);
          feeds.push(peers[0].spectator_feed_open());
        }
      }
      spectators.forEach((spectator, i) => {
        const bytes = peers[0].spectator_feed_take(feeds[i], 64 * 1024);
        for (let offset = 0; offset < bytes.length;) {
          const len = Math.min(1 + random() % 5000, bytes.length - offset);
          spectator.spectate_receive(bytes.subarray(offset, offset + len));
          offset += len;
        }
        // Local input never reaches a spectator's machine.
        spectator.set_joystick_port(2, true, true, true, true, true, true);
        spectator.key_event('Space', true);
        spectator.mouse_delta(5, 5);
        spectator.run_hidden(tick * 20, 8);
        spectator.take_audio();
      });
      const done = peers.every(emu => { const s = emu.netplay_status(); return s[1] === 300 && s[2] === 300 && s[6] === 300; })
        && spectators.length === 2 && spectators.every(s => s.spectate_status()[1] === 300);
      if (done) break;
    }
    for (const spectator of spectators) {
      const status = spectator.spectate_status();
      assert.deepEqual([...status], [1, 300, 0, 300, 0], `spectator status ${[...status]}`);
    }
    assert.equal(peers[0].spectator_feed_frames(), 300);
    assert.throws(() => peers[0].spectator_feed_take(feeds[0], 10), /budget/);
    peers[0].spectator_feed_close(feeds[0]);
    assert.throws(() => peers[0].spectator_feed_take(feeds[0], 4096), /Unknown/);
    for (const emu of [...peers, ...spectators]) {
      emu.set_overscan('tv');
      emu.run(40000, 0);
    }
    const pixels = [...peers, ...spectators].map(emu => Buffer.from(new Uint8Array(wasm.memory.buffer,
      emu.present_ptr(), emu.present_width() * emu.present_rows() * 4)));
    assert.deepEqual(pixels[2], pixels[0], 'the early spectator shows the host picture');
    assert.deepEqual(pixels[3], pixels[0], 'the late spectator shows the host picture');
    // Corrupt feed bytes fail closed rather than showing a different game.
    const broken = fresh();
    broken.insert_floppy(0, new Uint8Array(901120), 'disk.adf');
    broken.start_spectating('cd32');
    const cursor = peers[0].spectator_feed_open();
    const bytes = peers[0].spectator_feed_take(cursor, 4096);
    bytes[5] ^= 0x80;
    assert.throws(() => { broken.spectate_receive(bytes); broken.run_hidden(0, 8); });
    broken.free();
    console.log('Spectators joined at frames 60 and 200, replayed the host history and matched its picture at frame 300');
  } finally { [...peers, ...spectators].forEach(emu => emu.free()); }
}

for (const configure of [emu => emu.set_floppy_sounds(false), emu => emu.set_floppy_sounds_volume(12)]) {
  const peers = [fresh(), fresh()];
  try {
    configure(peers[1]);
    peers.forEach((emu, player) => {
      emu.start_netplay(player + 1, code, 2, 8, 'joystick');
      emu.run_hidden(0, 0);
    });
    const packets = peers.map(emu => emu.netplay_take_packet());
    peers.forEach((emu, player) => {
      emu.netplay_receive(packets[1 - player]);
      assert.throws(() => emu.run_hidden(1, 0), /initial machine mismatch/);
    });
  } finally { peers.forEach(emu => emu.free()); }
}
console.log('Floppy sound mismatches rejected on both peers');
