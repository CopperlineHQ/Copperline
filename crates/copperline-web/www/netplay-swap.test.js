// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { DiskSwaps, DISK_LIMIT, validateDisk } from './netplay-swap.js';

class Wire extends EventTarget {
  readyState = 'open';
  bufferedAmount = 0;
  maximum = 0;
  send(value) {
    const data = typeof value === 'string' ? value : value.slice().buffer;
    const size = typeof data === 'string' ? data.length : data.byteLength;
    this.bufferedAmount += size;
    this.maximum = Math.max(this.maximum, this.bufferedAmount);
    setTimeout(() => {
      this.bufferedAmount -= size;
      this.peer.onmessage?.({ data: this.transform?.(data) ?? data });
      if (this.bufferedAmount <= this.bufferedAmountLowThreshold) this.dispatchEvent(new Event('bufferedamountlow'));
    }, 0);
  }
}

class Machine {
  constructor(frame) { this.frame = frame; this.disks = [null, null]; this.events = []; this.ready = true; }
  netplay_status() { return [1, this.frame]; }
  netplay_validate_disk(drive, bytes) { assert.ok([0, 1].includes(drive)); if (bytes[0] === 255) throw new Error('Invalid disk image'); }
  netplay_hold() { this.events.push('hold'); this.held = true; return this.frame; }
  netplay_stop_at(frame) { this.events.push('target'); this.frame = frame; }
  netplay_swap_ready() { return this.ready; }
  netplay_swap_digest() { return new Uint8Array([this.frame, ...this.disks.map(disk => disk?.bytes[0] ?? 0), this.mismatch ?? 0]); }
  netplay_stage_disk(drive, bytes, writable) { this.events.push('stage'); this.pending = { drive, bytes: bytes.slice(), writable }; }
  netplay_apply_disk() { this.events.push('apply'); this.disks[this.pending.drive] = this.pending; this.pending = null; }
  netplay_resume() { this.events.push('resume'); this.held = false; }
}

function pair(t) {
  const wires = [new Wire(), new Wire()];
  wires[0].peer = wires[1]; wires[1].peer = wires[0];
  const machines = [new Machine(120), new Machine(124)];
  const swaps = [];
  const fail = error => swaps.forEach(swap => swap.stop(error));
  for (let i = 0; i < 2; i++) swaps.push(new DiskSwaps(wires[i], { host: !i, machine: () => machines[i], fail }));
  t.after(() => { swaps.forEach(swap => swap.stop()); wires.forEach(wire => { wire.onmessage = null; }); });
  return { host: swaps[0], guest: swaps[1], machines, wires };
}
const disk = value => ({ bytes: new Uint8Array(901120).fill(value), name: `Disk ${value}.adf`, writable: false });
async function idle(guest) {
  for (let i = 0; guest.busy && i < 100; i++) await new Promise(resolve => setTimeout(resolve, 5));
  assert.equal(guest.busy, false);
}

test('repeated DF0/DF1 swaps and ejections wait for matching boundaries and bounded transfers', async t => {
  const { host, guest, machines, wires } = pair(t);
  for (const [drive, image] of [[0, disk(7)], [0, disk(8)], [1, { ...disk(9), writable: true }], [0, null], [0, disk(7)]]) {
    await host.swap(drive, image);
    await idle(guest);
    assert.deepEqual(machines[0].disks, machines[1].disks);
    assert.equal(machines[0].frame, 124);
    for (const machine of machines) {
      assert.deepEqual(machine.events.splice(0), ['hold', 'target', 'stage', 'apply', 'resume']);
      assert.equal(machine.held, false);
    }
    assert.equal(guest.bytes, null);
  }
  assert.ok(wires[0].maximum <= 256 * 1024 + 16 * 1024);
});

test('neither drive changes until both frames and the received disk are verified', async t => {
  const { host, guest, machines, wires } = pair(t);
  machines[1].ready = false;
  const sending = host.swap(0, disk(4));
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(guest.phase, 'waiting');
  assert.deepEqual(machines.map(machine => machine.disks[0]), [null, null]);
  wires[0].bufferedAmount = 512 * 1024;
  machines[1].ready = true;
  await new Promise(resolve => setTimeout(resolve, 30));
  assert.equal(guest.phase, 'receiving');
  assert.deepEqual(machines.map(machine => machine.disks[0]), [null, null]);
  wires[0].bufferedAmount = 0;
  wires[0].dispatchEvent(new Event('bufferedamountlow'));
  await sending;
  await idle(guest);
});

test('a connecting disk channel leaves the game running and can be retried once open', async t => {
  const { host, guest, machines, wires } = pair(t);
  wires[0].readyState = 'connecting';
  await assert.rejects(host.swap(0, disk(7)), /not ready yet/);
  assert.equal(host.closed, false);
  assert.equal(host.busy, false);
  assert.deepEqual(machines.map(machine => machine.events), [[], []]);
  wires[0].readyState = 'open';
  await host.swap(0, disk(7));
  await idle(guest);
  assert.deepEqual(machines[0].disks, machines[1].disks);
  assert.ok(machines.every(machine => machine.events.includes('resume')));
});

test('invalid local files preserve the game; the guest cannot initiate a swap', async t => {
  const { host, guest, machines } = pair(t);
  await assert.rejects(host.swap(0, disk(255)), /Invalid disk image/);
  await assert.rejects(guest.swap(0, disk(1)), /Only the host/);
  assert.equal(host.closed, false);
  assert.deepEqual(machines.map(machine => machine.events), [[], []]);
});

test('corruption and before/after state mismatches stop both peers without resuming', async t => {
  for (const failure of ['corrupt', 'before', 'after']) {
    const { host, machines, wires } = pair(t);
    if (failure === 'before') machines[1].mismatch = 1;
    if (failure === 'after') {
      const apply = machines[1].netplay_apply_disk.bind(machines[1]);
      machines[1].netplay_apply_disk = () => { apply(); machines[1].mismatch = 1; };
    }
    wires[0].transform = data => {
      if (failure === 'corrupt' && data instanceof ArrayBuffer) new Uint8Array(data)[0] ^= 1;
      return data;
    };
    await assert.rejects(host.swap(0, disk(7)), /differ|did not match/);
    assert.ok(machines.every(machine => !machine.events.includes('resume')));
    if (failure !== 'after') assert.deepEqual(machines.map(machine => machine.disks[0]), [null, null]);
  }
});

test('cancellation releases blocked transfers and discards partially received bytes', async t => {
  const { host, guest, machines, wires } = pair(t);
  const sending = host.swap(0, disk(5));
  const result = assert.rejects(sending, /cancelled/);
  await new Promise(resolve => setTimeout(resolve, 2));
  wires[0].bufferedAmount = 512 * 1024;
  await new Promise(resolve => setTimeout(resolve, 30));
  guest.stop(new Error('cancelled'));
  await result;
  assert.equal(guest.bytes, null);
  assert.ok(machines.every(machine => !machine.events.includes('resume')));
});

test('metadata limits and unsolicited, oversized or out-of-order messages fail closed', t => {
  const valid = { drive: 0, size: 901120, writable: false, name: 'Disk.adf', hash: 'a'.repeat(64) };
  assert.deepEqual(validateDisk(valid), valid);
  for (const invalid of [{ drive: 2 }, { drive: 0.5 }, { size: -1 }, { size: DISK_LIMIT + 1 },
    { size: 1.5 }, { name: 'a'.repeat(257) }, { writable: 'false' }, { hash: 'bad' }, { size: 0, writable: true }]) {
    assert.throws(() => validateDisk({ ...valid, ...invalid }));
  }
  for (const message of [new ArrayBuffer(1), '{', ' '.repeat(2049), JSON.stringify({ type: 'resume', id: 1 })]) {
    const { guest } = pair(t);
    guest.channel.onmessage({ data: message });
    assert.equal(guest.closed, true);
  }
});

test('a peer that never reaches the confirmed boundary times out without changing disks', async t => {
  t.mock.timers.enable({ apis: ['setTimeout'] });
  const { guest, machines } = pair(t);
  machines[1].ready = false;
  guest.channel.onmessage({ data: JSON.stringify({ type: 'begin', id: 1,
    disk: { drive: 0, size: 901120, name: 'Disk.adf', writable: false, hash: 'a'.repeat(64) } }) });
  // Ignore the reply here: this test exercises the receiver's finite deadline.
  guest.channel.peer.onmessage = null;
  guest.channel.onmessage({ data: JSON.stringify({ type: 'target', id: 1, frame: 124 }) });
  t.mock.timers.tick(30000);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(guest.closed, true);
  assert.deepEqual(machines[1].disks, [null, null]);
  assert.equal(machines[1].events.includes('resume'), false);
});
