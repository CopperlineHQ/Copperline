#!/usr/bin/env node
// SPDX-License-Identifier: GPL-3.0-or-later
// Real release WASM, reliable disk control, and lossy/reordered input packets.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { DiskSwaps } from '../crates/copperline-web/www/netplay-swap.js';

const root = fileURLToPath(new URL('../', import.meta.url));
const pkg = resolve(process.argv[2] ?? resolve(root, 'crates/copperline-web/pkg'));
const glue = await readFile(`${pkg}/copperline_web.js`, 'utf8');
const { default: init, WebEmu } = await import(`data:text/javascript;base64,${Buffer.from(glue).toString('base64')}`);
await init({ module_or_path: await readFile(`${pkg}/copperline_web_bg.wasm`) });
const rom = new Uint8Array(await readFile(resolve(root, 'assets/aros/aros-amiga-m68k-rom.bin')));
const ext = new Uint8Array(await readFile(resolve(root, 'assets/aros/aros-amiga-m68k-ext.bin')));

class Wire extends EventTarget {
  readyState = 'open';
  bufferedAmount = 0;
  send(data) {
    data = typeof data === 'string' ? data : Uint8Array.from(data).buffer;
    setTimeout(() => this.peer.onmessage?.({ data }), 1);
  }
}

for (const [model, video, delay, window] of [['A500', 'PAL', 0, 8], ['A500', 'PAL', 2, 8], ['A1200', 'NTSC', 6, 1]]) {
  const peers = [0, 1].map(player => {
    const emu = new WebEmu(model, video, 2);
    emu.load_rom(rom, ext);
    emu.insert_floppy(0, new Uint8Array(901120), 'original.adf');
    emu.start_netplay(player + 1, '0123456789abcdef0123456789abcdef', delay, window, 'joystick');
    return emu;
  });
  const wires = [new Wire(), new Wire()];
  wires[0].peer = wires[1]; wires[1].peer = wires[0];
  const swaps = [];
  let failure, tick = 0, sequence = 0, queued = [];
  for (let i = 0; i < 2; i++) swaps.push(new DiskSwaps(wires[i], { host: !i, machine: () => peers[i],
    fail: error => { failure = error; swaps.forEach(swap => swap.stop(error)); },
    changed: disk => { if (disk) console.log(`Player ${i + 1} changed DF${disk.drive} at frame ${peers[i].netplay_status()[1]}`); } }));
  async function pump(until, limit = Infinity) {
    for (let count = 0; count < 4000 && !until(); count++) {
      if (failure) throw failure;
      for (let i = 0; i < 2; i++) {
        const emu = peers[i], frame = emu.netplay_status()[1];
        emu.key_event('Space', frame % 13 < 3);
        emu.set_joystick_port2(frame % 11 < 4, false, false, false, frame % 9 < 3, false);
        emu.run_hidden(tick * 20, frame < limit && (i || tick % 61 < 55) ? 1 : 0);
        emu.take_audio();
        for (;;) {
          const packet = emu.netplay_take_packet();
          if (!packet.length) break;
          sequence++;
          if (sequence % 7 === 0) continue;
          queued.push({ due: tick + sequence % 4, target: 1 - i, packet });
          if (sequence % 11 === 0) queued.push({ due: tick + 5, target: 1 - i, packet });
        }
      }
      const ready = queued.filter(item => item.due <= tick);
      queued = queued.filter(item => item.due > tick);
      for (const item of ready.reverse()) peers[item.target].netplay_receive(item.packet);
      tick++;
      await new Promise(resolve => setTimeout(resolve, 1));
    }
    if (failure) throw failure;
    assert.ok(until(), `timeline stalled: ${peers.map(emu => [...emu.netplay_status()])}`);
  }
  try {
    await pump(() => peers.every(emu => emu.netplay_status()[6] >= 120), 120);
    for (const invalid of [-1, 2, 1.5, 256, NaN, Infinity]) {
      assert.throws(() => peers[0].netplay_validate_disk(invalid, new Uint8Array(), false));
    }
    assert.throws(() => peers[0].netplay_apply_disk());
    assert.throws(() => peers[0].netplay_resume());
    assert.throws(() => peers[0].netplay_stage_disk(0, new Uint8Array(), false));
    assert.throws(() => peers[0].netplay_swap_digest());
    await assert.rejects(swaps[0].swap(0, { bytes: new Uint8Array(9), name: 'bad.adf', writable: false }));
    assert.equal(swaps[0].closed, false);
    for (const [drive, value, writable] of [[0, 7, false], [0, 8, true], [1, 9, false], [0, null, false], [0, 7, false]]) {
      let finished = false;
      const sending = swaps[0].swap(drive, value === null ? null : {
        bytes: value === 9 ? gzipSync(new Uint8Array(901120).fill(value)) : new Uint8Array(901120).fill(value),
        name: `Disk ${value}.${value === 9 ? 'adz' : 'adf'}`, writable,
      }).then(() => { finished = true; });
      sending.catch(() => {});
      await pump(() => finished && !swaps[1].busy);
      await sending;
      for (const emu of peers) {
        assert.equal(emu.disk_name(drive), value === null ? undefined : `netplay-df${drive}`);
        assert.equal(emu.floppy_write_protected(drive), value === null ? undefined : !writable);
      }
      const next = (Math.ceil(Math.max(...peers.map(emu => emu.netplay_status()[1])) / 60) + 2) * 60;
      await pump(() => peers.every(emu => emu.netplay_status()[6] >= next), next);
      console.log(`${model}/${video} delay=${delay} window=${window}: DF${drive} ${value === null ? 'ejected' : 'replaced'}, checked frame ${next}`);
    }
    peers.forEach(emu => emu.netplay_hold());
    for (const invalid of [-1, 1.5, NaN, Infinity, peers[0].netplay_status()[1] - 1, peers[0].netplay_status()[1] + 33]) {
      assert.throws(() => peers[0].netplay_stop_at(invalid));
    }
    assert.throws(() => peers[0].netplay_resume());
  } finally {
    swaps.forEach(swap => swap.stop());
    wires.forEach(wire => { wire.onmessage = null; });
    peers.forEach(emu => emu.free());
  }
}
console.log('Release WASM synchronized swaps, ejections, numeric boundaries and post-swap rollback passed');
