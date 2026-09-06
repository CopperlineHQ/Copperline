// SPDX-License-Identifier: GPL-3.0-or-later
import assert from 'node:assert/strict';
import test from 'node:test';
import { RtcLink, decodeCode, encodeCode, newSettings, validateSettings } from './netplay.js';

const settings = { session: '0123456789abcdef0123456789abcdef', delay: 2, window: 8, controller: 'joystick' };
const description = type => ({ type, sdp: 'v=0\r\ns=-\r\n' });

class Channel {
  constructor(label, options) { Object.assign(this, { label, ...options, bufferedAmount: 0, readyState: 'connecting', sent: [] }); }
  send(packet) { this.sent.push(packet); }
  close() { this.readyState = 'closed'; }
}
class Peer extends EventTarget {
  constructor() { super(); this.iceGatheringState = 'complete'; this.connectionState = 'new'; }
  createDataChannel(label, options) { return new Channel(label, options); }
  async createOffer() { return description('offer'); }
  async createAnswer() { return description('answer'); }
  async setLocalDescription(value) { this.localDescription = value; }
  async setRemoteDescription(value) { this.remoteDescription = value; }
  close() { this.connectionState = 'closed'; }
}

test('connection codes round-trip only valid settings and the expected description type', () => {
  const code = encodeCode(description('offer'), settings);
  assert.deepEqual(decodeCode(code, 'offer'), { description: description('offer'), settings });
  assert.throws(() => decodeCode(code, 'answer'), /Expected/);
  for (const bad of ['', 'CLNP1.!', 'CLNP1.' + 'A'.repeat(100000)]) assert.throws(() => decodeCode(bad, 'offer'));
  for (const key of ['delay', 'window']) {
    for (const value of [-1, 1.5, 257, NaN, Infinity, '2']) assert.throws(() => validateSettings({ ...settings, [key]: value }));
  }
  assert.throws(() => validateSettings({ ...settings, controller: 'mouse' }));
  assert.throws(() => validateSettings({ ...settings, session: 'bad' }));
  assert.notEqual(newSettings(0, 1, 'cd32').session, newSettings(0, 1, 'cd32').session);
});

test('host accepts only its answer and negotiates an unordered channel without retransmission', async () => {
  const host = new RtcLink({ PeerConnection: Peer });
  const join = new RtcLink({ PeerConnection: Peer });
  try {
    const offer = await host.offer(settings);
    const answer = await join.answer(offer);
    await assert.rejects(host.accept(encodeCode(description('answer'), { ...settings, delay: 3 })), /different/);
    assert.equal(host.pc.remoteDescription, undefined);
    await host.accept(answer);
    assert.equal(host.channel.ordered, false);
    assert.equal(host.channel.maxRetransmits, 0);
    assert.equal(host.channel.binaryType, 'arraybuffer');
    assert.deepEqual(join.settings, settings);
  } finally { host.close(); join.close(); }
});

test('cancelling ICE gathering rejects the pending offer and closes only once', async () => {
  let closed = 0;
  const link = new RtcLink({ PeerConnection: Peer, onClose: () => closed++ });
  link.pc.iceGatheringState = 'gathering';
  const offer = link.offer(settings);
  await new Promise(resolve => setImmediate(resolve));
  link.close();
  link.close();
  await assert.rejects(offer, /cancelled/);
  assert.equal(closed, 1);
  assert.equal(link.pc.connectionState, 'closed');
});

test('a delayed offer cannot resurrect a cancelled link', async () => {
  const link = new RtcLink({ PeerConnection: Peer });
  let finish;
  link.pc.createOffer = () => new Promise(resolve => { finish = resolve; });
  const offer = link.offer(settings);
  link.close();
  finish(description('offer'));
  await assert.rejects(offer, /cancelled/);
  assert.equal(link.pc.localDescription, undefined);
});

test('packet queues bound throttled-browser bursts and respect send backpressure', async () => {
  const link = new RtcLink({ PeerConnection: Peer });
  await link.offer(settings);
  const channel = link.channel;
  channel.readyState = 'open';
  for (let i = 0; i < 100; i++) channel.onmessage({ data: new Uint8Array([i]).buffer });
  const received = [];
  link.receive({ netplay_receive: bytes => received.push(bytes[0]) });
  assert.equal(received.length, 64);
  assert.equal(received[0], 36);
  assert.equal(received.at(-1), 99);
  let drained = 0;
  const emu = { netplay_take_packet: () => ++drained < 3 ? new Uint8Array([1]) : new Uint8Array() };
  channel.bufferedAmount = 943 * 64;
  link.send(emu);
  assert.equal(drained, 0);
  channel.bufferedAmount = 0;
  link.send(emu);
  assert.equal(channel.sent.length, 2);
  channel.onmessage({ data: new ArrayBuffer(944) });
  assert.equal(link.closed, true);
});

test('duplicate or incompatible channels close without running the emulator', () => {
  let opened = 0;
  const link = new RtcLink({ PeerConnection: Peer, onOpen: () => opened++ });
  link.attach(new Channel('copperline-netplay-v1', { ordered: true, maxRetransmits: null }));
  assert.equal(link.closed, true);
  assert.equal(opened, 0);
});

test('data-channel open runs once, remote close drains queued input before cleanup', async () => {
  let opens = 0;
  let closed;
  const link = new RtcLink({ PeerConnection: Peer, onOpen: () => opens++,
    onClose: (reason, peer) => {
      const received = [];
      peer.receive({ netplay_receive: packet => received.push(...packet) });
      closed = { reason, received };
    } });
  const channel = new Channel('copperline-netplay-v1', { ordered: false, maxRetransmits: 0 });
  link.pc.ondatachannel({ channel });
  channel.onopen();
  channel.onopen();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(opens, 1);
  channel.onmessage({ data: new Uint8Array([7, 8]).buffer });
  channel.onclose();
  assert.deepEqual(closed, { reason: 'Peer disconnected', received: [7, 8] });
  assert.equal(link.closed, true);
  assert.equal(channel.onopen, null);
  assert.equal(link.pc.ondatachannel, null);
  assert.equal(link.incoming.length, 0);
});

test('connection-state failure closes once and an open queued before cancellation stays cancelled', async () => {
  let opens = 0, closes = 0;
  const link = new RtcLink({ PeerConnection: Peer, onOpen: () => opens++, onClose: () => closes++ });
  await link.offer(settings);
  link.channel.onopen();
  link.pc.connectionState = 'failed';
  link.pc.onconnectionstatechange();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(opens, 0);
  assert.equal(closes, 1);
  assert.equal(link.channel.readyState, 'closed');
});
