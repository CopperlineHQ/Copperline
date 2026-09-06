// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { MEDIA_CHUNK, MEDIA_VERSION, MediaTransfer, describeMedia, validateManifest } from './netplay-media.js';

function snapshot(overrides = {}) {
  return { model: 'A1200', video: 'NTSC', floppySpeed: 800, floppySounds: false, monoAudio: true,
    build: 'test build', rom: { rom: new Uint8Array(512 * 1024).fill(7), ext: new Uint8Array(128 * 1024).fill(8), label: 'Host ROM' },
    disks: [{ bytes: new Uint8Array(901120).fill(9), name: 'host.adf', writable: true },
      { bytes: new Uint8Array(901120).fill(10), name: 'second.adf', writable: false }, null, null], ...overrides };
}

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
      if (this.peer.readyState === 'open') this.peer.onmessage?.({ data: this.transform?.(data) ?? data });
      if (this.bufferedAmount <= this.bufferedAmountLowThreshold) this.dispatchEvent(new Event('bufferedamountlow'));
    }, 0);
  }
}

function pair(t) {
  const outgoing = new Wire(), incoming = new Wire();
  outgoing.peer = incoming; incoming.peer = outgoing;
  let host, guest;
  const fail = error => { host?.stop(error); guest?.stop(error); };
  host = new MediaTransfer(outgoing, { host: true, fail });
  guest = new MediaTransfer(incoming, { host: false, fail });
  t.after(() => { host.stop(); guest.stop(); outgoing.readyState = incoming.readyState = 'closed'; });
  return { host, guest, outgoing, incoming };
}

test('host ROM, extended ROM, both drives and machine settings arrive unchanged with bounded buffering', async t => {
  const { host, guest, outgoing } = pair(t);
  const source = snapshot();
  const sending = host.send(source);
  const received = await guest.receive();
  await sending;
  assert.deepEqual(received, source);
  assert.notEqual(received.rom.rom.buffer, source.rom.rom.buffer);
  assert.ok(outgoing.maximum <= 256 * 1024 + MEDIA_CHUNK);
  assert.equal(host.finished, true);
  assert.equal(guest.finished, true);
});

test('all browser model, video and speed options validate; malformed metadata is rejected before allocation', async () => {
  const { manifest } = await describeMedia(snapshot());
  for (const model of ['A500', 'A1200']) for (const video of ['PAL', 'NTSC']) for (const floppySpeed of [0, 100, 200, 400, 800]) {
    assert.doesNotThrow(() => validateManifest({ ...manifest, config: { ...manifest.config, model, video, floppySpeed } }));
  }
  for (const [field, value] of [['model', 'unknown'], ['video', 'unknown'], ['floppySpeed', 257],
    ['floppySounds', 'false'], ['monoAudio', null], ['build', 'x'.repeat(129)]]) {
    assert.throws(() => validateManifest({ ...manifest, config: { ...manifest.config, [field]: value } }));
  }
  for (const field of [{ size: 1e9 }, { size: -1 }, { size: 1.5 }, { hash: 'bad' },
    { kind: 'unknown' }, { label: 'x'.repeat(257) }, { writable: 1 }]) {
    assert.throws(() => validateManifest({ ...manifest, files: [{ ...manifest.files[0], ...field }] }));
  }
  assert.throws(() => validateManifest({ ...manifest, files: [manifest.files[0], manifest.files[0]] }));
  assert.throws(() => validateManifest({ ...manifest, files: manifest.files.slice(1) }));
  assert.throws(() => validateManifest({ ...manifest, type: 'future' }));
});

test('corrupt media never receives a verification acknowledgement', async t => {
  const { host, guest, outgoing } = pair(t);
  let changed = false;
  outgoing.transform = data => {
    if (!changed && data instanceof ArrayBuffer) { new Uint8Array(data)[0] ^= 1; changed = true; }
    return data;
  };
  const results = await Promise.allSettled([host.send(snapshot()), guest.receive()]);
  assert.ok(results.every(result => result.status === 'rejected' && /did not match/.test(result.reason.message)));
  assert.equal(host.finished, false);
});

test('cancellation releases a sender waiting for backpressure and a partial receiver', async t => {
  const { host, guest, outgoing } = pair(t);
  outgoing.bufferedAmount = 512 * 1024;
  const sending = host.send(snapshot());
  setTimeout(() => host.stop(new Error('cancelled')), 20);
  const results = await Promise.allSettled([sending, guest.receive()]);
  assert.ok(results.every(result => result.status === 'rejected' && /cancelled/.test(result.reason.message)));
  assert.equal(guest.files.length, 0);
});

test('out-of-order messages, oversized chunks and premature acknowledgements fail the session', async t => {
  const { manifest } = await describeMedia(snapshot());
  for (const data of [new ArrayBuffer(1), '{', JSON.stringify({ type: MEDIA_VERSION, files: [] })]) {
    const { guest } = pair(t);
    guest.channel.onmessage({ data });
    await assert.rejects(guest.receive());
  }
  const { guest } = pair(t);
  guest.channel.onmessage({ data: JSON.stringify(manifest) });
  guest.channel.onmessage({ data: new ArrayBuffer(MEDIA_CHUNK + 1) });
  await assert.rejects(guest.receive(), /chunk/);
  const { host } = pair(t);
  host.channel.onmessage({ data: '{"verified":true}' });
  await assert.rejects(host.done, /acknowledgement/);
});
